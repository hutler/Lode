//! T194 acceptance: one access rule keeps every agent's work on shelves for chosen branches.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};

fn checkout(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_deny_write_on_main_keeps_them_on_shelves() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"one\n")])
        .await;
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: "dev".into(),
                from_branch: "main".into(),
                from_rev: None,
                stream: None,
            },
        )
        .await
        .unwrap();
    server.create_user("ana", &["read"]).await;
    let ana_writes = server
        .token_for("ana", &["read", "write", "lock", "shelve"])
        .await;
    // An unattended agent that was given `write`.
    let bot = server
        .create_agent("bot", "ana", &["read", "write", "shelve", "lock"])
        .await;
    for who in ["ana", "bot"] {
        server
            .acl("game", who, "*", "**", AclEffect::Allow, AccessLevel::Write)
            .await;
    }
    let admin = checkout(&server, server.admin_token());
    let added = admin.json(&[
        "admin", "acl", "add", "deny", "write", "agents", "--depot", "game", "--branch", "main",
    ]);
    assert_eq!(added["subject"]["kind"], "agents", "{added}");
    let list = admin.json(&["admin", "acl", "list"]);
    assert!(
        list["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["subject"]["kind"] == "agents"),
        "{list}"
    );
    let check = admin.json(&["admin", "acl", "check", "bot", "game", "main", "a.txt"]);
    assert_eq!(check["level"], "read", "{check}");

    // The agent cannot commit to main, but can shelve there. A lock needs write access to
    // the path, so it cannot lock there either.
    let ws = checkout(&server, &bot);
    ws.write("a.txt", "two\n");
    let (code, out) = ws.json_status(&["commit", "-m", "direct"]);
    assert_ne!(code, 0);
    assert_eq!(out["error"]["code"], "permission_denied", "{out}");
    let (code, out) = ws.json_status(&["lock", "a.txt"]);
    assert_ne!(code, 0);
    assert_eq!(out["error"]["code"], "permission_denied", "{out}");
    let shelf = ws.json(&["shelve", "-m", "proposal", "--keep"]);
    let id = shelf["shelf"].as_u64().unwrap();
    // Nor submit its own shelf there.
    let (code, out) = ws.json_status(&["shelf", "submit", &format!("{id}@1")]);
    assert_ne!(code, 0);
    assert_eq!(out["error"]["code"], "permission_denied", "{out}");
    ws.json(&["revert", "--all"]);

    // On another branch it still commits.
    ws.json(&["switch", "dev"]);
    ws.write("a.txt", "dev\n");
    let done = ws.json(&["commit", "-m", "on dev"]);
    assert!(done["rev"].as_u64().is_some(), "{done}");

    // A user is not an agent: the rule does not touch them.
    let person = checkout(&server, &ana_writes);
    let done = person.json(&["shelf", "submit", &format!("{id}@1")]);
    assert!(done["rev"].as_u64().is_some(), "{done}");
    person.json(&["update"]);
    person.write("a.txt", "three\n");
    let done = person.json(&["commit", "-m", "by a person"]);
    assert!(done["rev"].as_u64().is_some(), "{done}");
}
