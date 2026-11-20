//! T200 acceptance: `lode commit` on a `[branches] shelf_only` branch.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};

const RULE: &[u8] = b"[branches]\nshelf_only = [\"main\"]\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_is_refused_and_a_shelf_goes_through() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[(".lodeconfig", RULE)])
        .await;
    let token = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.write("b.txt", "b\n");
    ws.json(&["add", "b.txt"]);

    // Refused before anything is uploaded.
    let before = server.requests().count("PUT", "/blobs/");
    let (code, v) = ws.json_status(&["commit", "-m", "b"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "shelf_required", "{v}");
    assert_eq!(v["error"]["details"]["branch"], "main", "{v}");
    assert_eq!(server.requests().count("PUT", "/blobs/"), before);
    let out = ws.cmd().args(["commit", "-m", "b"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("takes changes through shelves only"), "{err}");
    assert!(err.contains("lode shelf submit"), "{err}");

    // A local edit of the rule does not help: the server refuses the same way.
    ws.write(".lodeconfig", "");
    let (code, v) = ws.json_status(&["commit", "-m", "b", "b.txt"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "shelf_required", "{v}");
    ws.json(&["revert", ".lodeconfig"]);

    // The shelf route.
    let id = ws.json(&["shelve", "-m", "b"])["shelf"].as_u64().unwrap();
    let v = ws.json(&["shelf", "submit", &format!("{id}@1")]);
    assert!(v["rev"].as_u64().is_some(), "{v}");

    // A depot admin's override, recorded on the revision.
    ws.json(&["update"]);
    ws.write("c.txt", "c\n");
    ws.json(&["add", "c.txt"]);
    let v = ws.json(&["commit", "-m", "c", "--override", "release tonight"]);
    assert_eq!(v["shelf_only_override"], true, "{v}");
    let rev = v["rev"].as_u64().unwrap();
    let view = ws.json(&["log", "-n", "1"]);
    let text = view.to_string();
    assert!(text.contains("release tonight"), "{view}");
    assert!(text.contains(&rev.to_string()), "{view}");
    let out = ws.cmd().args(["log", "-n", "1"]).output().unwrap();
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(
        human.contains("committed directly to a shelf-only branch by admin: release tonight"),
        "{human}"
    );
}
