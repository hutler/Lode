//! T064 acceptance: clients on unattended machines (build agents). Switching straight to a
//! revision, signing in from the environment, `--version --json`, the shared download cache
//! and the reset sequence for a reused workspace.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

/// Every file under `root` except `.lode`, by relative path with `/`.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".lode" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The blob ids requested with `GET …/blobs/{id}`.
fn blob_gets(server: &TestServer) -> Vec<String> {
    server
        .requests()
        .all()
        .into_iter()
        .filter(|r| r.method == "GET" && r.path.contains("/blobs/"))
        .map(|r| r.path.rsplit('/').next().unwrap().to_owned())
        .collect()
}

fn hex(data: &[u8]) -> String {
    lode_core::blob_id(data).unwrap().to_hex()
}

/// Runs `lode --json args…` with extra environment variables.
fn json_env(ws: &TestWorkspace, env: &[(&str, &Path)], args: &[&str]) -> (i32, Value) {
    let mut cmd = ws.cmd();
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.arg("--json").args(args).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!(
            "lode {args:?}: {e}\n{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), v)
}

/// Checks every cache entry against its name and that no temporary file is left; returns
/// the entry names.
fn cache_entries(dir: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for shard in std::fs::read_dir(dir).unwrap() {
        let shard = shard.unwrap();
        for e in std::fs::read_dir(shard.path()).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with(".tmp-"), "leftover temporary file {name}");
            let data = std::fs::read(e.path()).unwrap();
            assert_eq!(
                hex(&data),
                name,
                "cache entry {name} does not match its content"
            );
            names.insert(name);
        }
    }
    names
}

#[test]
fn version_json_names_the_api() {
    let ws = TestWorkspace::detached();
    let (code, v) = ws.json_status(&["--version"]);
    assert_eq!(code, 0);
    assert_eq!(v["ok"], true);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["build"], lode_build::VERSION);
    assert_eq!(v["api"], lode_proto::consts::API_VERSION);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_sign_in_wins_over_a_stored_login() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let builder = server.create_user("builder", &["read", "write"]).await;
    let other = server.create_user("other", &["read"]).await;
    for name in ["builder", "other"] {
        server
            .acl(
                "game",
                name,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
    }

    // No stored login: the variables alone sign in.
    let agent = TestWorkspace::new(&server).with_token(&builder);
    assert_eq!(agent.json(&["whoami"])["name"], "builder");
    agent.json(&["checkout", &format!("{}/game", server.url()), "."]);
    assert_eq!(agent.read("a.txt"), b"a\n");
    // The token file's path is not kept in the workspace.
    let token_path = agent.home().join("token");
    let db = std::fs::read(agent.root().join(".lode/workspace.db")).unwrap();
    let needle = token_path.to_string_lossy().into_owned();
    assert!(
        !db.windows(needle.len()).any(|w| w == needle.as_bytes()),
        "the token file path was written into the workspace"
    );

    // A stored login for another account: the variables still win.
    let ws = TestWorkspace::detached();
    ws.cmd()
        .args(["login", server.url(), "--token-stdin"])
        .write_stdin(format!("{other}\n"))
        .assert()
        .success();
    assert_eq!(ws.json(&["whoami"])["name"], "other");
    let token_file = ws.home().join("builder-token");
    std::fs::write(&token_file, format!("{builder}\n")).unwrap();
    let (code, v) = json_env(
        &ws,
        &[
            ("LODE_SERVER", Path::new(server.url())),
            ("LODE_TOKEN_FILE", &token_file),
        ],
        &["whoami"],
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["name"], "builder");

    // A missing or unreadable token file is an authentication error, not a fallback.
    let missing = ws.home().join("no-such-token");
    let (code, v) = json_env(&ws, &[("LODE_TOKEN_FILE", &missing)], &["whoami"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error"]["code"], "unauthenticated");
    let unreadable = ws.home().join("a-directory");
    std::fs::create_dir_all(&unreadable).unwrap();
    let (code, v) = json_env(&ws, &[("LODE_TOKEN_FILE", &unreadable)], &["whoami"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error"]["code"], "unauthenticated");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_cache_downloads_each_blob_once() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let files: Vec<(String, Vec<u8>)> = (0..12)
        .map(|i| {
            (
                format!("dir{}/f{i}.bin", i % 3),
                format!("content {}\n", i % 9).into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, c)| (p.as_str(), &c[..])).collect();
    server.commit_files("game", "main", &refs).await;
    let distinct: BTreeSet<String> = files.iter().map(|(_, c)| hex(c)).collect();
    assert_eq!(distinct.len(), 9);

    let shared = tempfile::tempdir().unwrap();
    let cache_dir = shared.path().join("cache");
    let url = format!("{}/game", server.url());

    // One workspace finds the cache through its settings, the other through LODE_CACHE_DIR.
    let ws1 = TestWorkspace::new(&server).with_token(server.admin_token());
    std::fs::create_dir_all(ws1.config_dir()).unwrap();
    std::fs::write(
        ws1.config_dir().join("settings.toml"),
        format!("cache_dir = {:?}\n", cache_dir.to_string_lossy()),
    )
    .unwrap();
    let ws2 = TestWorkspace::new(&server).with_token(server.admin_token());

    server.requests().clear();
    ws1.json(&["checkout", &url, "."]);
    let (code, v) = json_env(
        &ws2,
        &[("LODE_CACHE_DIR", &cache_dir)],
        &["checkout", &url, "."],
    );
    assert_eq!(code, 0, "{v}");
    let gets = blob_gets(&server);
    assert_eq!(
        gets.len(),
        distinct.len(),
        "each blob is downloaded once: {gets:?}"
    );
    assert_eq!(gets.iter().cloned().collect::<BTreeSet<_>>(), distinct);
    assert_eq!(tree(ws1.root()), tree(ws2.root()));
    assert_eq!(cache_entries(&cache_dir), distinct);

    // Workspace files are copies: a build changing one leaves the cache alone.
    ws1.write("dir0/f0.bin", "changed by a build\n");
    assert_eq!(cache_entries(&cache_dir), distinct);
    assert_eq!(ws2.read("dir0/f0.bin"), b"content 0\n");

    // A damaged entry is noticed, downloaded again and replaced.
    let victim = hex(b"content 1\n");
    let entry = cache_dir.join(&victim[..2]).join(&victim);
    std::fs::write(&entry, b"bit rot").unwrap();
    let ws3 = TestWorkspace::new(&server).with_token(server.admin_token());
    server.requests().clear();
    let (code, v) = json_env(
        &ws3,
        &[("LODE_CACHE_DIR", &cache_dir)],
        &["checkout", &url, "."],
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(blob_gets(&server), vec![victim.clone()]);
    assert_eq!(ws3.read("dir1/f1.bin"), b"content 1\n");
    assert_eq!(cache_entries(&cache_dir), distinct);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_processes_keep_the_cache_consistent() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let files: Vec<(String, Vec<u8>)> = (0..60)
        .map(|i| {
            (
                format!("d{}/f{i}.txt", i % 5),
                format!("{i:04}\n").repeat(500).into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, c)| (p.as_str(), &c[..])).collect();
    server.commit_files("game", "main", &refs).await;
    let distinct: BTreeSet<String> = files.iter().map(|(_, c)| hex(c)).collect();

    let shared = tempfile::tempdir().unwrap();
    let cache_dir = shared.path().join("cache");
    let url = format!("{}/game", server.url());
    let workspaces: Vec<TestWorkspace> = (0..3)
        .map(|_| TestWorkspace::new(&server).with_token(server.admin_token()))
        .collect();
    let children: Vec<_> = workspaces
        .iter()
        .map(|ws| {
            ws.std_cmd()
                .env("LODE_CACHE_DIR", &cache_dir)
                .args(["--json", "checkout", &url, "."])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let expected: BTreeMap<String, Vec<u8>> = files.into_iter().collect();
    for ws in &workspaces {
        assert_eq!(tree(ws.root()), expected);
    }
    assert_eq!(cache_entries(&cache_dir), distinct);
}

/// Text conflict on `b.txt` from a merge, plus every other kind of local damage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_sequence_and_switch_to_a_revision() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"a1\n"),
                ("b.txt", b"b1\n"),
                ("c.txt", b"c1\n"),
                ("dir/d.txt", b"d1\n"),
            ],
        )
        .await;
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: "rel".into(),
                from_branch: "main".into(),
                from_rev: Some(r1),
                stream: None,
            },
        )
        .await
        .unwrap();
    server
        .commit_files("game", "rel", &[("b.txt", b"b rel\n")])
        .await;
    server
        .commit_files("game", "main", &[("b.txt", b"b main\n"), ("e.txt", b"e\n")])
        .await;
    let target = server
        .commit_files("game", "rel", &[("a.txt", b"a rel\n"), ("g.txt", b"g\n")])
        .await;
    // The branch moves on; the build wants `target`, not the head.
    server
        .commit_files("game", "rel", &[("h.txt", b"later\n")])
        .await;

    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    // Damage: pending merge with a conflict, modified, added, deleted, missing, untracked.
    let (code, merged) = ws.json_status(&["merge", "rel"]);
    assert_ne!(
        merged["counts"]["conflicted"],
        Value::Null,
        "{code} {merged}"
    );
    ws.write("e.txt", "edited\n");
    ws.write("new.txt", "added\n");
    ws.json(&["add", "new.txt"]);
    ws.json(&["rm", "c.txt"]);
    ws.remove("dir/d.txt");
    ws.write("junk.txt", "untracked\n");
    ws.write("junkdir/deep/x.o", "untracked\n");
    let st = ws.json(&["status"]);
    assert!(!st["files"].as_array().unwrap().is_empty());

    // The reset sequence.
    ws.json(&["revert", "--all"]);
    ws.json(&["clean"]);
    server.requests().clear();
    let switched = ws.json(&["switch", "rel", "-r", &target.to_string()]);
    assert_eq!(switched["branch"], "rel");
    let gets = blob_gets(&server);

    let fresh = TestWorkspace::new(&server).with_token(server.admin_token());
    fresh.json(&[
        "checkout",
        &format!("{}/game", server.url()),
        ".",
        "-b",
        "rel",
    ]);
    fresh.json(&["update", "-r", &target.to_string()]);
    let want = tree(fresh.root());
    assert_eq!(tree(ws.root()), want);
    assert_eq!(want["a.txt"], b"a rel\n");
    assert!(!want.contains_key("h.txt"));

    let st = ws.json(&["status"]);
    assert_eq!(st["branch"], "rel");
    assert_eq!(st["files"], serde_json::json!([]));
    assert_eq!(fresh.json(&["status"])["rev"], st["rev"]);

    // Only blobs of the target revision crossed the wire.
    let in_target: BTreeSet<String> = want.values().map(|c| hex(c)).collect();
    assert!(!gets.is_empty());
    for id in &gets {
        assert!(
            in_target.contains(id),
            "switch downloaded {id}, which is not in r{target}"
        );
    }
}
