//! T050 acceptance: `lode branch` and `lode switch`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn checkout(server: &TestServer) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

fn outcomes(v: &Value) -> BTreeMap<String, String> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn text(ws: &TestWorkspace, path: &str) -> String {
    String::from_utf8(ws.read(path)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_switch_commit_and_back() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("a.txt", b"one\n"), ("b.txt", b"same\n")])
        .await;
    let ws = checkout(&server);

    let created = ws.json(&["branch", "create", "feat", "--switch"]);
    assert_eq!(created["name"], "feat");
    assert_eq!(created["parent"]["branch"], "main");
    assert_eq!(created["parent"]["rev"], r1);
    assert_eq!(created["switch"]["branch"], "feat");
    let st = ws.json(&["status"]);
    assert_eq!(st["branch"], "feat");

    ws.write("a.txt", "feat\n");
    ws.write("new.txt", "only on feat\n");
    ws.json(&["add", "new.txt"]);
    let c = ws.json(&["commit", "-m", "feat work"]);
    assert_eq!(c["branch"], "feat");
    let main = server.admin().branch("game", "main").await.unwrap();
    assert_eq!(main.head_rev, r1, "main is unchanged");

    let back = ws.json(&["switch", "main"]);
    assert_eq!(back["switched_from"], "feat");
    assert_eq!(text(&ws, "a.txt"), "one\n");
    assert!(!ws.exists("new.txt"));
    assert_eq!(ws.json(&["status"])["files"], serde_json::json!([]));
    assert_eq!(ws.json(&["status"])["branch"], "main");

    ws.json(&["switch", "feat"]);
    assert_eq!(text(&ws, "a.txt"), "feat\n");
    assert_eq!(text(&ws, "new.txt"), "only on feat\n");

    let list = ws.json(&["branch", "list"]);
    assert_eq!(list["current"], "feat");
    let names: Vec<&str> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["feat", "main"]);

    // An older revision of a branch, and one that is not on it.
    ws.json(&["switch", "main", "-r", &r1.to_string()]);
    assert_eq!(text(&ws, "a.txt"), "one\n");
    let (code, err) = ws.json_status(&["switch", "feat", "-r", "999"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "not_found");

    let archived = ws.json(&["branch", "archive", "feat"]);
    assert_eq!(archived["state"], "archived");
    let list = ws.json(&["branch", "list"]);
    assert_eq!(list["branches"].as_array().unwrap().len(), 1);
    let list = ws.json(&["branch", "list", "--all"]);
    assert_eq!(list["branches"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switch_carries_local_changes() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("same.txt", b"same\n"),
                ("lines.txt", b"1\n2\n3\n4\n5\n"),
                ("both.txt", b"base\n"),
            ],
        )
        .await;
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: "feat".into(),
                from_branch: "main".into(),
                from_rev: None,
            },
        )
        .await
        .unwrap();
    server
        .commit_files(
            "game",
            "feat",
            &[("lines.txt", b"ONE\n2\n3\n4\n5\n"), ("both.txt", b"feat\n")],
        )
        .await;
    let ws = checkout(&server);
    ws.write("same.txt", "same\nmine\n");
    ws.write("lines.txt", "1\n2\n3\n4\nFIVE\n");
    ws.write("both.txt", "mine\n");

    let s = ws.json(&["switch", "feat"]);
    let o = outcomes(&s);
    assert_eq!(o.get("lines.txt").map(String::as_str), Some("merged"));
    assert_eq!(o.get("both.txt").map(String::as_str), Some("conflicted"));
    assert!(!o.contains_key("same.txt"), "{o:?}");
    // Identical on both branches: the edit stays as it was.
    assert_eq!(text(&ws, "same.txt"), "same\nmine\n");
    assert_eq!(text(&ws, "lines.txt"), "ONE\n2\n3\n4\nFIVE\n");
    assert!(text(&ws, "both.txt").contains("<<<<<<< mine"));
    let st = ws.json(&["status"]);
    assert_eq!(st["branch"], "feat");

    // Conflicts must be settled before switching again.
    let (code, err) = ws.json_status(&["switch", "main"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "conflicted");
}
