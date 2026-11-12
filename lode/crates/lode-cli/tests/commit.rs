//! T024 acceptance: `lode commit` round trip, uploads, stale files, and a lost answer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;

use lode_testkit::{failpoint, TestServer, TestWorkspace};
use serde_json::Value;

async fn server() -> TestServer {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"apple\n"),
                ("b.txt", b"banana\n"),
                ("c.txt", b"cherry\none\ntwo\n"),
                ("d/e.txt", b"elder\n"),
            ],
        )
        .await;
    server
}

fn checkout(server: &TestServer) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".lode" && dir == base {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(base, &e.path(), out);
            } else {
                let rel = e
                    .path()
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn changed(ws: &TestWorkspace) -> Vec<String> {
    ws.json(&["status"])["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                f["status"].as_str().unwrap(),
                f["path"].as_str().unwrap()
            )
        })
        .collect()
}

async fn head(server: &TestServer) -> u64 {
    server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_modify_delete_rename_round_trip() {
    let server = server().await;
    let ws = checkout(&server);
    ws.write("a.txt", "apple pie\n");
    ws.write("new/n.txt", "new\n");
    ws.json(&["add", "new"]);
    ws.json(&["rm", "b.txt"]);
    // A modified file, moved: add-with-copy-from plus delete.
    ws.write("c.txt", "cherry\none\ntwo\nthree\n");
    ws.json(&["mv", "c.txt", "moved/c.txt"]);
    let v = ws.json(&["commit", "-m", "Many changes"]);
    let rev = v["rev"].as_u64().unwrap();
    assert_eq!(rev, head(&server).await);
    assert_eq!(v["files"], 5);
    assert!(changed(&ws).is_empty(), "{:?}", changed(&ws));

    let other = checkout(&server);
    assert_eq!(snapshot(other.root()), snapshot(ws.root()));
    assert_eq!(other.read("moved/c.txt"), b"cherry\none\ntwo\nthree\n");

    let log = ws.json(&["log", "-n", "1", "-v"]);
    let r = &log["revisions"][0];
    assert_eq!(r["rev"].as_u64(), Some(rev));
    assert_eq!(r["message"], "Many changes");
    let changes = r["changes"].as_array().unwrap();
    let moved = changes.iter().find(|c| c["path"] == "moved/c.txt").unwrap();
    assert_eq!(moved["action"], "add");
    assert_eq!(moved["copy_from"]["path"], "c.txt");
    let gone = changes.iter().find(|c| c["path"] == "c.txt").unwrap();
    assert_eq!(gone["action"], "delete");

    // The committed workspace keeps working: edit again, commit again.
    ws.write("moved/c.txt", "again\n");
    let v2 = ws.json(&["commit", "-m", "Again"]);
    assert_eq!(v2["rev"].as_u64(), Some(rev + 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_missing_blobs_are_uploaded() {
    let server = server().await;
    let ws = checkout(&server);
    // Same content as a.txt (already on the server), and one new file.
    ws.write("copy.txt", "apple\n");
    ws.write("fresh.txt", "fresh\n");
    ws.json(&["add", "copy.txt", "fresh.txt"]);
    server.requests().clear();
    let v = ws.json(&["commit", "-m", "two adds"]);
    assert_eq!(v["uploaded"], 1);
    assert_eq!(server.requests().count("PUT", "/blobs/"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_file_fails_out_of_date_and_changes_nothing() {
    let server = server().await;
    let mine = checkout(&server);
    let theirs = checkout(&server);
    theirs.write("a.txt", "theirs\n");
    theirs.json(&["commit", "-m", "theirs"]);
    let head_before = head(&server).await;

    mine.write("a.txt", "mine\n");
    mine.write("b.txt", "also mine\n");
    let (code, err) = mine.json_status(&["commit", "-m", "mine"]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(err["error"]["code"], "out_of_date");
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("a.txt") && msg.contains("lode update"),
        "{msg}"
    );
    assert_eq!(head(&server).await, head_before);
    assert_eq!(changed(&mine), ["modified a.txt", "modified b.txt"]);
    assert_eq!(mine.read("a.txt"), b"mine\n");

    // An unrelated file still commits.
    let v = mine.json(&["commit", "-m", "just b", "b.txt"]);
    assert_eq!(v["rev"].as_u64(), Some(head_before + 1));
    assert_eq!(changed(&mine), ["modified a.txt"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_answer_is_finished_by_the_next_commit() {
    let server = server().await;
    let ws = checkout(&server);
    let before = head(&server).await;
    ws.write("a.txt", "committed but unanswered\n");
    server
        .failpoints()
        .arm(failpoint::COMMIT_AFTER_TRANSACTION, 1);
    let (code, err) = ws.json_status(&["commit", "-m", "lost"]);
    assert_eq!(code, 4, "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("lode commit"),
        "{err}"
    );
    // The server did commit it.
    assert_eq!(head(&server).await, before + 1);

    let v: Value = ws.json(&["commit"]);
    assert_eq!(v["rev"].as_u64(), Some(before + 1));
    assert_eq!(v["replayed"], true);
    assert_eq!(head(&server).await, before + 1);
    assert!(changed(&ws).is_empty(), "{:?}", changed(&ws));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_before_sending() {
    let server = server().await;
    let ws = checkout(&server);
    let (code, err) = ws.json_status(&["commit", "-m", "x"]);
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("nothing_to_commit"))
    );

    ws.write("a.txt", "changed\n");
    let (code, err) = ws.json_status(&["commit", "-m", "  \n"]);
    assert_eq!((code, err["error"]["code"].as_str()), (1, Some("aborted")));
    let (code, _) = ws.json_status(&["commit"]);
    assert_eq!(code, 2, "no message and no terminal is a usage error");

    ws.remove("b.txt");
    let (code, err) = ws.json_status(&["commit", "-m", "x"]);
    assert_eq!((code, err["error"]["code"].as_str()), (1, Some("missing")));
    ws.json(&["revert", "b.txt"]);

    // -A adds untracked files; -F reads the message.
    ws.write("u.txt", "untracked\n");
    ws.write("msg.txt", "From a file\n# not this\n");
    let v = ws.json(&["commit", "-A", "-F", "msg.txt", "a.txt", "u.txt"]);
    assert_eq!(v["files"], 2);
    let log = ws.json(&["log", "-n", "1"]);
    assert_eq!(log["revisions"][0]["message"], "From a file");
    assert_eq!(changed(&ws), ["untracked msg.txt"]);
}
