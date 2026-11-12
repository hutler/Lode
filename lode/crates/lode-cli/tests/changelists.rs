//! T034 acceptance: changelists group pending changes and commit one at a time.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::{json, Value};

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
                ("c.txt", b"cherry\n"),
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

/// Changelist name of each changed path, from `status --json`.
fn lists(ws: &TestWorkspace) -> Vec<(String, Value)> {
    ws.json(&["status"])["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["changelist"].clone(),
            )
        })
        .collect()
}

fn names(ws: &TestWorkspace) -> Vec<String> {
    ws.json(&["changelist", "list"])["changelists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect()
}

fn cat(ws: &TestWorkspace, spec: &str) -> String {
    ws.json(&["cat", spec])["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_changelists_commit_as_two_revisions() {
    let server = server().await;
    let ws = checkout(&server);
    ws.write("a.txt", "apple 2\n");
    ws.write("b.txt", "banana 2\n");
    ws.write("c.txt", "cherry 2\n");
    ws.write("new.txt", "new\n");
    ws.json(&["changelist", "new", "ui", "-m", "Button work"]);
    ws.json(&["changelist", "new", "art"]);
    ws.json(&["changelist", "new", "spare"]);
    let out = ws.json(&["changelist", "move", "ui", "a.txt"]);
    assert_eq!(out["moved"], json!(["a.txt"]));
    ws.json(&["changelist", "move", "art", "b.txt"]);
    ws.json(&["add", "-c", "art", "new.txt"]);
    assert_eq!(
        lists(&ws),
        vec![
            ("a.txt".into(), json!("ui")),
            ("b.txt".into(), json!("art")),
            ("c.txt".into(), Value::Null),
            ("new.txt".into(), json!("art")),
        ]
    );
    let text = String::from_utf8(ws.lode(&["status"]).get_output().stdout.clone()).unwrap();
    assert_eq!(
        text,
        "M  c.txt\n\nChangelist art:\nM  b.txt\nA  new.txt\n\nChangelist ui: Button work\nM  a.txt\n"
    );
    let list = ws.json(&["changelist", "list"]);
    assert_eq!(
        list["changelists"][0],
        json!({ "name": "default", "description": "", "files": ["c.txt"] })
    );

    // The description is the default message.
    let r1 = ws.json(&["commit", "-c", "ui"]);
    let rev_ui = r1["rev"].as_u64().unwrap();
    assert_eq!(r1["files"], 1);
    let log = ws.json(&["log", "-n", "1"]);
    assert_eq!(log["revisions"][0]["message"], "Button work");
    assert_eq!(cat(&ws, &format!("a.txt@{rev_ui}")), "apple 2\n");
    assert_eq!(cat(&ws, &format!("b.txt@{rev_ui}")), "banana\n");
    // `ui` is empty now and gone; `spare` was never used and stays.
    assert_eq!(names(&ws), vec!["default", "art", "spare"]);

    let r2 = ws.json(&["commit", "-c", "art", "-m", "art"]);
    let rev_art = r2["rev"].as_u64().unwrap();
    assert_eq!(r2["files"], 2);
    assert_eq!(cat(&ws, &format!("b.txt@{rev_art}")), "banana 2\n");
    assert_eq!(cat(&ws, &format!("new.txt@{rev_art}")), "new\n");
    assert_eq!(cat(&ws, &format!("c.txt@{rev_art}")), "cherry\n");

    let (code, v) = ws.json_status(&["commit", "-c", "spare", "-m", "x"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "nothing_to_commit");
    let r3 = ws.json(&["commit", "-c", "default", "-m", "rest"]);
    assert_eq!(r3["files"], 1);
    assert!(lists(&ws).is_empty());
    assert_eq!(names(&ws), vec!["default", "spare"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_files_and_deleting_changelists() {
    let server = server().await;
    let ws = checkout(&server);
    ws.write("a.txt", "apple 2\n");
    ws.write("d/e.txt", "elder 2\n");
    ws.json(&["changelist", "new", "x"]);
    ws.json(&["changelist", "new", "y"]);

    // A directory moves its tracked files; unknown names and paths are refused.
    let out = ws.json(&["changelist", "move", "x", "d"]);
    assert_eq!(out["moved"], json!(["d/e.txt"]));
    let (code, v) = ws.json_status(&["changelist", "move", "nope", "a.txt"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("not_found")));
    let (code, v) = ws.json_status(&["changelist", "move", "x", "zzz.txt"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("not_found")));
    for bad in ["default", "a b", ""] {
        let (code, _) = ws.json_status(&["changelist", "new", bad]);
        assert_eq!(code, 2, "{bad:?}");
    }
    let (code, v) = ws.json_status(&["changelist", "new", "x"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("conflict")));

    // Between changelists, and back to default.
    ws.json(&["changelist", "move", "y", "d/e.txt", "a.txt"]);
    assert_eq!(
        lists(&ws),
        vec![("a.txt".into(), json!("y")), ("d/e.txt".into(), json!("y"))]
    );
    ws.json(&["changelist", "move", "default", "a.txt"]);
    assert_eq!(
        lists(&ws),
        vec![
            ("a.txt".into(), Value::Null),
            ("d/e.txt".into(), json!("y"))
        ]
    );

    // Deleting returns the files to default.
    let out = ws.json(&["changelist", "delete", "y"]);
    assert_eq!(out, json!({ "ok": true, "deleted": "y", "files": 1 }));
    assert!(lists(&ws).iter().all(|(_, l)| l.is_null()));
    let (code, _) = ws.json_status(&["changelist", "delete", "default"]);
    assert_eq!(code, 2);

    // A reverted file leaves its changelist.
    ws.json(&["changelist", "move", "x", "a.txt"]);
    ws.json(&["revert", "a.txt"]);
    ws.write("a.txt", "apple 3\n");
    assert_eq!(lists(&ws)[0], ("a.txt".into(), Value::Null));

    // Both halves of a move go into the changelist, so the commit is a rename.
    ws.json(&["mv", "-c", "x", "b.txt", "b2.txt"]);
    ws.json(&["rm", "-c", "x", "c.txt"]);
    let st = lists(&ws);
    assert!(st.contains(&("b.txt".into(), json!("x"))));
    assert!(st.contains(&("b2.txt".into(), json!("x"))));
    assert!(st.contains(&("c.txt".into(), json!("x"))));
    // Moving one half takes the other along.
    ws.json(&["changelist", "move", "default", "b2.txt"]);
    let st = lists(&ws);
    assert!(st.contains(&("b.txt".into(), Value::Null)));
    ws.json(&["changelist", "move", "x", "b.txt"]);

    ws.json(&["commit", "-c", "x", "-m", "rename"]);
    let log = ws.json(&["log", "-n", "1", "-v"]);
    let changes = log["revisions"][0]["changes"].as_array().unwrap();
    let mut seen: Vec<(String, String)> = changes
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().to_owned(),
                c["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("b.txt".into(), "delete".into()),
            ("b2.txt".into(), "add".into()),
            ("c.txt".into(), "delete".into()),
        ]
    );
    assert_eq!(
        changes.iter().find(|c| c["path"] == "b2.txt").unwrap()["copy_from"]["path"],
        "b.txt"
    );
    // a.txt and d/e.txt were not in it.
    let rest: Vec<String> = lists(&ws).into_iter().map(|(p, _)| p).collect();
    assert_eq!(rest, vec!["a.txt", "d/e.txt"]);
    assert_eq!(names(&ws), vec!["default"]);
}
