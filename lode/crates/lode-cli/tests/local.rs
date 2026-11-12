//! T023 acceptance: status, add, rm, mv, cp, revert, diff and clean through the real
//! `lode` binary, checked against `status` and the workspace database.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;

use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind};
use lode_proto::commit::CommitRequest;
use lode_testkit::{TestServer, TestWorkspace};
use lode_workspace::{ConflictRow, FileState, Workspace};
use serde_json::{json, Value};

/// Seeds `game/main` with `(path, content, executable)`.
async fn seed(server: &TestServer, items: &[(&str, &[u8], bool)]) -> u64 {
    let c = server.admin();
    let mut changes = Vec::new();
    for (path, content, exec) in items {
        let id = blob_id(*content).unwrap();
        c.put_blob("game", &id, std::io::Cursor::new(content.to_vec()))
            .await
            .unwrap();
        changes.push(Change {
            path: DepotPath::parse(path).unwrap(),
            action: ChangeAction::Add,
            base_id: None,
            new_id: Some(id),
            kind: Some(EntryKind::File),
            executable: *exec,
            size: Some(content.len() as u64),
            copy_from: None,
        });
    }
    let names: Vec<&str> = items.iter().map(|i| i.0).collect();
    let req = CommitRequest {
        request_id: format!("seed-{}", names.join(",")),
        message: "seed".into(),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    };
    c.commit("game", "main", &req).await.unwrap().rev
}

const FILES: &[(&str, &[u8], bool)] = &[
    (".lodeignore", b"*.log\nbuild/\n", false),
    (
        "a/1.txt",
        b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n",
        false,
    ),
    ("a/2.txt", b"alpha\nbeta\n", false),
    ("b.txt", b"bee\n", false),
    ("tool.sh", b"#!/bin/sh\necho hi\n", true),
    ("img.bin", b"\x00\x01\x02binary", false),
];

/// A server with the seeded depot and a workspace checked out at its root.
async fn setup() -> (TestServer, TestWorkspace) {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    seed(&server, FILES).await;
    let ws = checkout(&server);
    (server, ws)
}

fn checkout(server: &TestServer) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

/// `status --json` as `path -> status`.
fn status(ws: &TestWorkspace) -> BTreeMap<String, String> {
    let v = ws.json(&["status"]);
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn st(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

fn db(ws: &TestWorkspace) -> Workspace {
    Workspace::open(&lode_workspace::paths::canonical(ws.root()).unwrap()).unwrap()
}

fn row(ws: &TestWorkspace, path: &str) -> Option<lode_workspace::FileRow> {
    db(ws).db().file(&DepotPath::parse(path).unwrap()).unwrap()
}

#[cfg(unix)]
fn exec_bit(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn exec_bit(_p: &Path) -> bool {
    false
}

#[cfg(unix)]
fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Every file under `dir` (`.lode` left out) with its content and executable bit, and every
/// directory (as `dir/`).
fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, bool)> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, bool)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".lode" && dir == base {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                out.insert(format!("{rel}/"), (Vec::new(), false));
                walk(base, &e.path(), out);
            } else {
                out.insert(rel, (std::fs::read(e.path()).unwrap(), exec_bit(&e.path())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_command_shows_in_status_and_the_database() {
    let (_server, ws) = setup().await;
    assert!(status(&ws).is_empty());

    // Modify, add, remove.
    ws.write("a/1.txt", "changed\n");
    ws.write("new/n.txt", "new\n");
    ws.write("debug.log", "ignored\n");
    let v = ws.json(&["add", "new"]);
    assert_eq!(strings(&v["added"]), ["new/n.txt"]);
    let (code, err) = ws.json_status(&["add", "debug.log"]);
    assert_eq!((code, err["error"]["code"].as_str()), (1, Some("ignored")));
    ws.json(&["add", "--force", "debug.log"]);
    ws.json(&["rm", "b.txt"]);
    assert!(!ws.exists("b.txt"));
    // A modified file is not removed without --force or --keep.
    let (code, err) = ws.json_status(&["rm", "a/1.txt"]);
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("local_changes"))
    );

    // Move and copy.
    let v = ws.json(&["mv", "a/2.txt", "c/2.txt"]);
    assert_eq!(v["moved"], json!([{ "from": "a/2.txt", "to": "c/2.txt" }]));
    assert!(!ws.exists("a/2.txt") && ws.exists("c/2.txt"));
    ws.json(&["cp", "tool.sh", "tool2.sh"]);
    assert_eq!(exec_bit(&ws.root().join("tool2.sh")), cfg!(unix));
    // An already done rename is recorded with --after.
    std::fs::rename(ws.root().join("img.bin"), ws.root().join("img2.bin")).unwrap();
    assert_eq!(
        status(&ws).get("img.bin").map(String::as_str),
        Some("missing")
    );
    ws.json(&["mv", "--after", "img.bin", "img2.bin"]);

    assert_eq!(
        status(&ws),
        st(&[
            ("a/1.txt", "modified"),
            ("a/2.txt", "deleted"),
            ("b.txt", "deleted"),
            ("c/2.txt", "added"),
            ("debug.log", "added"),
            ("img.bin", "deleted"),
            ("img2.bin", "added"),
            ("new/n.txt", "added"),
            ("tool2.sh", "added"),
        ])
    );
    let v = ws.json(&["status"]);
    let moved = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "c/2.txt")
        .unwrap()
        .clone();
    assert_eq!(moved["copy_from"]["path"], "a/2.txt");
    assert_eq!(moved["moved"], true);
    let copied = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "tool2.sh")
        .unwrap()
        .clone();
    assert_eq!(copied["moved"], false);

    let r = row(&ws, "c/2.txt").unwrap();
    assert_eq!(r.state, FileState::Added);
    assert_eq!(r.copy_from.unwrap().path.as_str(), "a/2.txt");
    assert_eq!(row(&ws, "a/2.txt").unwrap().state, FileState::Deleted);
    assert_eq!(row(&ws, "b.txt").unwrap().state, FileState::Deleted);
    assert_eq!(row(&ws, "tool2.sh").unwrap().executable, cfg!(unix));

    // rm --keep stops tracking an add and leaves the file; add brings a deleted file back.
    ws.json(&["rm", "--keep", "new/n.txt"]);
    assert!(row(&ws, "new/n.txt").is_none() && ws.exists("new/n.txt"));
    ws.write("b.txt", "bee\n");
    ws.json(&["add", "b.txt"]);
    assert_eq!(row(&ws, "b.txt").unwrap().state, FileState::Normal);
    // Moving back onto the origin undoes the move.
    ws.json(&["mv", "c/2.txt", "a/2.txt"]);
    assert!(row(&ws, "c/2.txt").is_none());
    assert_eq!(row(&ws, "a/2.txt").unwrap().state, FileState::Normal);
    assert!(!ws.exists("c"), "empty directory left behind");
    let s = status(&ws);
    assert!(
        !s.contains_key("a/2.txt") && !s.contains_key("b.txt"),
        "{s:?}"
    );
    assert_eq!(s.get("new/n.txt").map(String::as_str), Some("untracked"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directories_move_and_copy_with_their_files() {
    let (_server, ws) = setup().await;
    ws.json(&["mv", "a", "z"]);
    assert_eq!(
        status(&ws),
        st(&[
            ("a/1.txt", "deleted"),
            ("a/2.txt", "deleted"),
            ("z/1.txt", "added"),
            ("z/2.txt", "added"),
        ])
    );
    std::fs::create_dir(ws.root().join("dir")).unwrap();
    let v = ws.json(&["cp", "z", "b.txt", "dir"]);
    assert_eq!(v["copied"].as_array().unwrap().len(), 3);
    assert_eq!(ws.read("dir/z/1.txt"), FILES[1].1);
    assert_eq!(ws.read("dir/b.txt"), b"bee\n");
    // A copy of a moved file keeps the move's origin.
    assert_eq!(
        row(&ws, "dir/z/1.txt")
            .unwrap()
            .copy_from
            .unwrap()
            .path
            .as_str(),
        "a/1.txt"
    );
    let (code, _) = ws.json_status(&["mv", "z", "z/inner"]);
    assert_eq!(code, 2);
    let (code, err) = ws.json_status(&["mv", "nothing", "x"]);
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("not_found"))
    );
    let (code, err) = ws.json_status(&["cp", "b.txt", "tool.sh"]);
    assert_eq!(code, 1, "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revert_restores_exact_content_and_mode() {
    let (server, ws) = setup().await;
    let (code, err) = ws.json_status(&["revert"]);
    assert_eq!((code, err["error"]["code"].as_str()), (2, Some("usage")));

    ws.write("a/1.txt", "edited\n");
    ws.remove("a/2.txt");
    ws.json(&["rm", "b.txt"]);
    #[cfg(unix)]
    {
        chmod(&ws.root().join("tool.sh"), 0o644);
        chmod(&ws.root().join("a/1.txt"), 0o755);
    }
    ws.write("added.txt", "mine\n");
    ws.json(&["add", "added.txt"]);
    // Base content comes from the server: nothing is cached yet.
    let before = server.requests().count("GET", "/blobs/");
    let v = ws.json(&["revert", "a", "b.txt", "tool.sh", "added.txt"]);
    assert_eq!(
        strings(&v["reverted"]),
        ["a/1.txt", "a/2.txt", "added.txt", "b.txt", "tool.sh"]
    );
    assert!(server.requests().count("GET", "/blobs/") > before);
    assert_eq!(ws.read("a/1.txt"), FILES[1].1);
    assert_eq!(ws.read("a/2.txt"), FILES[2].1);
    assert_eq!(ws.read("b.txt"), FILES[3].1);
    assert_eq!(exec_bit(&ws.root().join("tool.sh")), cfg!(unix));
    assert!(!exec_bit(&ws.root().join("a/1.txt")));
    // A reverted add stays on disk, untracked.
    assert_eq!(ws.read("added.txt"), b"mine\n");
    assert_eq!(status(&ws), st(&[("added.txt", "untracked")]));
    assert!(row(&ws, "added.txt").is_none());
    assert_eq!(row(&ws, "b.txt").unwrap().state, FileState::Normal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revert_all_clears_conflicts_and_their_files() {
    let (_server, ws) = setup().await;
    // An update would leave this; it is set up directly until `lode update` exists.
    let mut w = db(&ws);
    let path = DepotPath::parse("img.bin").unwrap();
    let mut r = w.db().file(&path).unwrap().unwrap();
    r.state = FileState::Conflicted;
    w.db_mut()
        .write(|tx| {
            tx.put_file(&r)?;
            tx.put_conflict(&ConflictRow {
                path: path.clone(),
                base_id: r.base_id,
                theirs_id: r.base_id,
                theirs_rev: 7,
                mine_id: None,
            })
        })
        .unwrap();
    drop(w);
    ws.write("img.bin", "mine");
    ws.write("img.bin.theirs-r7", "theirs");
    assert_eq!(
        status(&ws).get("img.bin").map(String::as_str),
        Some("conflicted")
    );
    ws.json(&["revert", "--all"]);
    assert_eq!(ws.read("img.bin"), FILES[5].1);
    assert!(!ws.exists("img.bin.theirs-r7"));
    assert!(db(&ws).db().conflicts().unwrap().is_empty());
    assert!(status(&ws).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revert_all_and_clean_return_to_a_fresh_checkout() {
    let (server, ws) = setup().await;
    let fresh = checkout(&server);
    let pristine = snapshot(fresh.root());

    // Damage of every kind.
    ws.write("a/1.txt", "modified\n");
    ws.write("added/x.txt", "added\n");
    ws.json(&["add", "added"]);
    ws.json(&["rm", "b.txt"]);
    ws.remove("a/2.txt");
    ws.json(&["mv", "tool.sh", "bin/tool.sh"]);
    ws.write("untracked.txt", "u\n");
    ws.write("deep/er/u.txt", "u\n");
    std::fs::create_dir_all(ws.root().join("empty/dir")).unwrap();
    ws.write("keep.log", "ignored\n");
    ws.write("build/out.o", "ignored\n");
    ws.write("a/trace.log", "ignored\n");
    #[cfg(unix)]
    chmod(&ws.root().join("img.bin"), 0o755);

    ws.json(&["revert", "--all"]);
    assert_eq!(
        status(&ws)
            .values()
            .collect::<std::collections::HashSet<_>>(),
        [&"untracked".to_owned()].into_iter().collect()
    );

    let dry = ws.json(&["clean", "--dry-run"]);
    let listed = strings(&dry["removed"]);
    assert!(listed.contains(&"untracked.txt".to_owned()), "{listed:?}");
    assert!(listed.contains(&"empty/".to_owned()), "{listed:?}");
    assert!(ws.exists("untracked.txt"), "dry run removed something");
    let real = ws.json(&["clean"]);
    assert_eq!(strings(&real["removed"]), listed);

    let mut expected = pristine.clone();
    for p in ["keep.log", "build/out.o", "a/trace.log"] {
        expected.insert(p.to_owned(), (b"ignored\n".to_vec(), false));
    }
    expected.insert("build/".to_owned(), (Vec::new(), false));
    assert_eq!(snapshot(ws.root()), expected);

    let dry = ws.json(&["clean", "--ignored", "-n"]);
    let real = ws.json(&["clean", "--ignored"]);
    assert_eq!(dry["removed"], real["removed"]);
    assert_eq!(snapshot(ws.root()), pristine);
    assert!(ws.exists(".lode/workspace.db"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_shows_text_and_binary_changes() {
    let (_server, ws) = setup().await;
    ws.write("a/1.txt", "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n");
    ws.write("img.bin", b"\x00\x01changed".as_slice());
    ws.write("n.txt", "fresh\n");
    ws.json(&["add", "n.txt"]);

    let v = ws.json(&["diff"]);
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    let one = &files[0];
    assert_eq!(one["path"], "a/1.txt");
    assert_eq!(
        (one["insertions"].as_u64(), one["deletions"].as_u64()),
        (Some(1), Some(1))
    );
    let patch = one["patch"].as_str().unwrap();
    assert!(patch.contains("--- a/a/1.txt\n+++ b/a/1.txt\n"), "{patch}");
    assert!(patch.contains("-three\n+THREE\n"), "{patch}");
    let img = &files[1];
    assert_eq!(img["binary"], true);
    assert_eq!(
        (img["old_size"].as_u64(), img["new_size"].as_u64()),
        (Some(9), Some(9))
    );
    assert!(img["patch"]
        .as_str()
        .unwrap()
        .contains("Binary files a/img.bin and b/img.bin differ"));
    assert!(files[2]["patch"]
        .as_str()
        .unwrap()
        .contains("--- /dev/null\n+++ b/n.txt\n"));

    let out = ws.lode(&["diff", "--stat"]).success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(text.contains("a/1.txt | 2 +-"), "{text}");
    assert!(text.contains("img.bin | Bin 9 -> 9 bytes"), "{text}");
    assert!(
        text.contains("3 files changed, 2 insertions(+), 1 deletion(-)"),
        "{text}"
    );
    // Paths narrow the diff.
    let v = ws.json(&["diff", "a"]);
    assert_eq!(v["files"].as_array().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_output_applies_with_patch() {
    if std::process::Command::new("patch")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no `patch` tool");
        return;
    }
    let (server, ws) = setup().await;
    ws.write(
        "a/1.txt",
        "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight",
    );
    ws.write("a/2.txt", "alpha\n\nbeta\ngamma\n");
    ws.write("sub/new.txt", "brand\nnew\n");
    ws.json(&["add", "sub"]);
    ws.json(&["rm", "b.txt"]);
    let out = ws.lode(&["diff"]).success();
    let patch_text = out.get_output().stdout.clone();

    let other = checkout(&server);
    let applied = std::process::Command::new("patch")
        .args(["-p1", "--batch", "--silent"])
        .current_dir(other.root())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(&patch_text)?;
            child.wait()
        })
        .unwrap();
    assert!(
        applied.success(),
        "{}",
        String::from_utf8_lossy(&patch_text)
    );
    for p in ["a/1.txt", "a/2.txt", "sub/new.txt"] {
        assert_eq!(other.read(p), ws.read(p), "{p}");
    }
    assert!(!other.exists("b.txt"));
}
