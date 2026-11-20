//! T022 acceptance: login, checkout (contents, modes, symlinks, hostile manifests, resume)
//! and view changes, through the real `lode` binary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind};
use lode_proto::commit::CommitRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::json;

struct Seed<'a> {
    path: &'a str,
    content: &'a [u8],
    kind: EntryKind,
    executable: bool,
}

fn file<'a>(path: &'a str, content: &'static [u8]) -> Seed<'a> {
    Seed {
        path,
        content,
        kind: EntryKind::File,
        executable: false,
    }
}

async fn seed(server: &TestServer, items: &[Seed<'_>]) -> u64 {
    let c = server.admin();
    let mut changes = Vec::new();
    for s in items {
        let id = blob_id(s.content).unwrap();
        c.put_blob("game", &id, std::io::Cursor::new(s.content.to_vec()))
            .await
            .unwrap();
        changes.push(Change {
            path: DepotPath::parse(s.path).unwrap(),
            action: ChangeAction::Add,
            base_id: None,
            new_id: Some(id),
            kind: Some(s.kind),
            executable: s.executable,
            size: Some(s.content.len() as u64),
            copy_from: None,
        });
    }
    let req = CommitRequest {
        request_id: format!("seed-{}", items.len()),
        message: "seed".into(),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    };
    c.commit("game", "main", &req).await.unwrap().rev
}

async fn server_with_depot() -> TestServer {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
}

fn ws_for(server: &TestServer) -> TestWorkspace {
    TestWorkspace::new(server).with_token(server.admin_token())
}

/// Files under `dir`, relative, sorted; `.lode` left out.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".lode" && dir == base {
                continue;
            }
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                walk(base, &e.path(), out);
            } else {
                let rel = e
                    .path()
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_whoami_logout() {
    let server = TestServer::start().await;
    let token = server.create_user("dana", &["read", "write"]).await;
    let ws = TestWorkspace::new(&server);
    let out = ws
        .cmd()
        .args(["--json", "login", server.url(), "--token-stdin"])
        .write_stdin(format!("{token}\n"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "dana");
    // The token file is under the isolated config directory.
    assert!(Path::new(v["token_file"].as_str().unwrap()).starts_with(ws.config_dir()));

    let me = ws.json(&["whoami"]);
    assert_eq!(me["name"], "dana");
    assert_eq!(me["kind"], "user");
    let bye = ws.json(&["logout"]);
    assert_eq!(bye["revoked"], true);
    let (code, err) = ws.json_status(&["whoami"]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "unauthenticated");
    // The revoked token no longer works at all.
    assert!(server.client(&token).whoami().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_reproduces_the_depot() {
    let server = server_with_depot().await;
    let mut items = vec![
        file("README.md", b"# game\n"),
        file("src/main.rs", b"fn main() {}\n"),
        file("src/copy.rs", b"fn main() {}\n"),
        file("assets/big.bin", &[7u8; 300_000]),
        Seed {
            path: "tools/run.sh",
            content: b"#!/bin/sh\necho run\n",
            kind: EntryKind::File,
            executable: true,
        },
    ];
    if cfg!(unix) {
        items.push(Seed {
            path: "link-to-src",
            content: b"src",
            kind: EntryKind::Symlink,
            executable: false,
        });
    }
    let rev = seed(&server, &items).await;
    let ws = ws_for(&server);
    let url = format!("{}/game", server.url());
    let v = ws.json(&["checkout", &url]);
    assert_eq!(v["rev"], rev);
    assert_eq!(v["files"], items.len());
    // Identical content is downloaded once.
    assert_eq!(
        v["downloaded"],
        items.len() as u64 - 1 - u64::from(cfg!(unix))
    );
    let root = ws.root().join("game");
    for s in &items {
        let local = root.join(s.path);
        if s.kind == EntryKind::Symlink {
            assert_eq!(
                std::fs::read_link(&local).unwrap().to_str().unwrap(),
                std::str::from_utf8(s.content).unwrap()
            );
        } else {
            assert_eq!(std::fs::read(&local).unwrap(), s.content, "{}", s.path);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| {
            std::fs::metadata(root.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("tools/run.sh"), 0o755);
        assert_eq!(mode("README.md"), 0o644);
    }
    // Nothing left in .lode/tmp.
    assert_eq!(
        std::fs::read_dir(root.join(".lode/tmp")).unwrap().count(),
        0
    );

    // A second checkout into the same place is refused.
    let (code, err) = ws.json_status(&["checkout", &url]);
    assert_eq!(code, 1);
    assert_eq!(err["error"]["code"], "already_a_workspace");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_checkout_resumes() {
    let server = server_with_depot().await;
    let contents: Vec<(String, Vec<u8>)> = (0..20)
        .map(|i| {
            (
                format!("d{}/f{i}.txt", i % 3),
                format!("file {i}").into_bytes(),
            )
        })
        .collect();
    let files: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    server.commit_files("game", "main", &files).await;
    let ws = ws_for(&server);
    let url = format!("{}/game", server.url());

    let out = ws
        .cmd()
        .env("LODE_TEST_EXIT_AFTER_FILES", "5")
        .args(["checkout", &url])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let root = ws.root().join("game");
    assert_eq!(tree(&root).len(), 5);

    server.requests().clear();
    let v = ws.json(&["checkout", &url]);
    assert_eq!(v["resumed"], true);
    assert_eq!(v["fetched"], 15);
    // Finished files were not downloaded again.
    assert_eq!(server.requests().count("GET", "/blobs/"), 15);
    let mut expected: Vec<String> = contents.iter().map(|c| c.0.clone()).collect();
    expected.sort();
    assert_eq!(tree(&root), expected);
    for (p, c) in &contents {
        assert_eq!(&std::fs::read(root.join(p)).unwrap(), c);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_view_adds_and_removes_exactly_its_files() {
    let server = server_with_depot().await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a/1.txt", b"a1"),
                ("a/2.txt", b"a2"),
                ("b/1.txt", b"b1"),
                ("art/x.psd", b"psd"),
            ],
        )
        .await;
    let ws = ws_for(&server);
    let url = format!("{}/game", server.url());
    ws.json(&["checkout", &url, "ws", "--exclude", "art/**"]);
    let root = ws.root().join("ws");
    let w = |args: &[&str]| -> serde_json::Value {
        let mut full = vec!["-C", "ws"];
        full.extend_from_slice(args);
        ws.json(&full)
    };
    assert_eq!(tree(&root), vec!["a/1.txt", "a/2.txt", "b/1.txt"]);
    assert_eq!(
        w(&["view", "show"])["view"],
        json!({ "include": [], "exclude": ["art/**"] })
    );

    let v = w(&["view", "exclude", "--remove", "art/**"]);
    assert_eq!(v["added"], 1);
    assert_eq!(
        tree(&root),
        vec!["a/1.txt", "a/2.txt", "art/x.psd", "b/1.txt"]
    );

    let v = w(&["view", "exclude", "a/**"]);
    assert_eq!(v["removed"], 2);
    assert_eq!(tree(&root), vec!["art/x.psd", "b/1.txt"]);
    assert!(!root.join("a").exists(), "empty directory left behind");

    let v = w(&["view", "include", "b/**"]);
    assert_eq!(v["removed"], 1);
    assert_eq!(tree(&root), vec!["b/1.txt"]);

    // Local changes block leaving the view, unless forced.
    std::fs::write(root.join("b/1.txt"), b"edited").unwrap();
    let (code, err) = ws.json_status(&["-C", "ws", "view", "exclude", "b/**"]);
    assert_eq!(code, 1);
    assert_eq!(err["error"]["code"], "local_changes");
    assert_eq!(err["error"]["details"]["paths"], json!(["b/1.txt"]));
    assert_eq!(std::fs::read(root.join("b/1.txt")).unwrap(), b"edited");
    w(&["view", "exclude", "b/**", "--force"]);
    assert!(tree(&root).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_change_keeps_local_changes_and_files_committed_or_added_since_the_sync() {
    let server = server_with_depot().await;
    server
        .commit_files("game", "main", &[("a.txt", b"a"), ("art/x.psd", b"psd")])
        .await;
    let ws = ws_for(&server);
    let url = format!("{}/game", server.url());
    ws.json(&["checkout", &url, "ws"]);
    let root = ws.root().join("ws");
    let w = |args: &[&str]| -> serde_json::Value {
        let mut full = vec!["-C", "ws"];
        full.extend_from_slice(args);
        ws.json(&full)
    };
    // Committed after the checkout, so not in the synced revision's manifest.
    std::fs::write(root.join("new.txt"), b"new").unwrap();
    w(&["add", "new.txt"]);
    w(&["commit", "-m", "New"]);
    // Added, not committed.
    std::fs::write(root.join("added.txt"), b"added").unwrap();
    w(&["add", "added.txt"]);
    // Changed, not committed.
    std::fs::write(root.join("a.txt"), b"a, edited").unwrap();

    let v = w(&["view", "exclude", "art/**"]);
    assert_eq!(
        (v["added"].as_u64(), v["removed"].as_u64()),
        (Some(0), Some(1)),
        "{v}"
    );
    assert_eq!(tree(&root), vec!["a.txt", "added.txt", "new.txt"]);
    assert_eq!(
        std::fs::read(root.join("a.txt")).unwrap(),
        b"a, edited",
        "local changes stay"
    );
    let st = w(&["status"]);
    assert!(
        st["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "a.txt" && f["status"] == "modified"),
        "{st}"
    );
    let added: Vec<&str> = st["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["status"] == "added")
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert_eq!(added, ["added.txt"], "{st}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlinked_directory_is_not_followed() {
    let server = server_with_depot().await;
    server
        .commit_files("game", "main", &[("top.txt", b"t"), ("sub/f.txt", b"f")])
        .await;
    let ws = ws_for(&server);
    let url = format!("{}/game", server.url());
    ws.json(&["checkout", &url, "ws", "--exclude", "sub/**"]);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), ws.root().join("ws/sub")).unwrap();
    let (code, err) = ws.json_status(&["-C", "ws", "view", "exclude", "--remove", "sub/**"]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(err["error"]["code"], "unsafe_path");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

mod hostile {
    use super::*;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;

    /// A server whose manifest holds `line` and nothing else.
    async fn fake(line: String) -> String {
        let id = blob_id(&b"evil"[..]).unwrap();
        let app = Router::new()
            .route(
                "/api/v1/depots/{d}/branches/{b}",
                get(|| async {
                    axum::Json(json!({ "name": "main", "head_rev": 1, "created_rev": 1, "state": "active" }))
                }),
            )
            .route(
                "/api/v1/depots/{d}/manifest",
                get(move || {
                    let line = line.clone();
                    async move { ([("content-type", "application/x-ndjson")], format!("{line}\n")) }
                }),
            )
            .route(
                "/api/v1/depots/{d}/blobs/{id}",
                get(|| async { "evil".into_response() }),
            );
        let _ = id;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        url
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paths_escaping_the_root_are_refused() {
        let id = blob_id(&b"evil"[..]).unwrap();
        for path in [
            "../x",
            "/etc/x",
            "a/../../x",
            ".lode/workspace.db",
            ".LODE/x",
            "a\\..\\..\\x",
        ] {
            let url =
                fake(json!({ "path": path, "kind": "file", "id": id, "size": 4 }).to_string())
                    .await;
            let ws = TestWorkspace::detached().with_token("t");
            let (code, err) = ws.json_status(&["checkout", &format!("{url}/game"), "ws"]);
            assert_ne!(code, 0, "{path}: {err}");
            let code_name = err["error"]["code"].as_str().unwrap().to_owned();
            assert!(
                code_name == "protocol" || code_name == "unsafe_path",
                "{path}: {err}"
            );
            // Nothing was written next to or above the workspace.
            let mut beside: Vec<String> = std::fs::read_dir(ws.root())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            beside.sort();
            assert_eq!(beside, vec!["ws"], "{path}");
            assert!(!ws.home().join("x").exists());
            let in_ws = tree(&ws.root().join("ws"));
            assert!(in_ws.is_empty(), "{path}: {in_ws:?}");
        }
    }
}
