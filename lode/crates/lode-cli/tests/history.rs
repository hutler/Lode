//! T026 acceptance: `log`, `cat` and `ls`, human and `--json` forms, as snapshots against a
//! seeded depot, in a workspace and without one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_client::Client;
use lode_core::{blob_id, Change, ChangeAction, CopyFrom, DepotPath, EntryKind};
use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::commit::CommitRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

/// 2026-01-02 03:04:05 UTC.
const T0: i64 = 1_767_323_045_000;

fn binary() -> Vec<u8> {
    (0..=255u8).cycle().take(1000).collect()
}

async fn commit(c: &Client, msg: &str, changes: Vec<Change>) -> u64 {
    let req = CommitRequest {
        request_id: format!("req-{msg}"),
        message: msg.into(),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    };
    c.commit("game", "main", &req).await.unwrap().rev
}

async fn put(c: &Client, content: &[u8]) -> lode_core::ObjectId {
    let id = blob_id(content).unwrap();
    c.put_blob("game", &id, std::io::Cursor::new(content.to_vec()))
        .await
        .unwrap();
    id
}

fn add(path: &str, id: lode_core::ObjectId, size: usize, exec: bool) -> Change {
    Change {
        path: DepotPath::parse(path).unwrap(),
        action: ChangeAction::Add,
        base_id: None,
        new_id: Some(id),
        kind: Some(EntryKind::File),
        executable: exec,
        size: Some(size as u64),
        copy_from: None,
    }
}

/// r1 creates the depot; r2 by admin, r3 by dana, r4 (a rename) by dana's agent `bot`.
async fn seeded() -> TestServer {
    let server = TestServer::start().await;
    server.clock().set_ms(T0);
    server.create_depot("game").await;
    let dana = server.create_user("dana", &["read", "write"]).await;
    let bot = server.create_agent("bot", "dana", &["read", "write"]).await;
    for who in ["dana", "bot"] {
        server
            .acl("game", who, "*", "**", AclEffect::Allow, AccessLevel::Write)
            .await;
    }
    let admin = server.admin();
    let readme = put(&admin, b"hello\n").await;
    let main = put(&admin, b"fn main() {}\n").await;
    let bin = put(&admin, &binary()).await;
    let tool = put(&admin, b"#!/bin/sh\n").await;
    commit(
        &admin,
        "Initial import\n\nWith a second paragraph.",
        vec![
            add("README.md", readme, 6, false),
            add("src/main.rs", main, 13, false),
            add("assets/blob.bin", bin, 1000, false),
            add("tool.sh", tool, 10, true),
        ],
    )
    .await;

    server.clock().advance_ms(90_000);
    let c = server.client(&dana);
    let readme2 = put(&c, b"hello\nworld\n").await;
    commit(
        &c,
        "Say more",
        vec![Change {
            path: DepotPath::parse("README.md").unwrap(),
            action: ChangeAction::Modify,
            base_id: Some(readme),
            new_id: Some(readme2),
            kind: Some(EntryKind::File),
            executable: false,
            size: Some(12),
            copy_from: None,
        }],
    )
    .await;

    server.clock().advance_ms(3_600_000);
    let c = server.client(&bot);
    let mut renamed = add("src/app.rs", main, 13, false);
    renamed.copy_from = Some(CopyFrom {
        path: DepotPath::parse("src/main.rs").unwrap(),
        rev: 3,
    });
    commit(
        &c,
        "Rename main",
        vec![
            renamed,
            Change {
                path: DepotPath::parse("src/main.rs").unwrap(),
                action: ChangeAction::Delete,
                base_id: Some(main),
                new_id: None,
                kind: None,
                executable: false,
                size: None,
                copy_from: None,
            },
        ],
    )
    .await;
    server
}

/// A directory with no workspace, using the admin token and `--server`.
fn bare(server: &TestServer) -> TestWorkspace {
    TestWorkspace::new(server).with_token(server.admin_token())
}

fn text(ws: &TestWorkspace, args: &[&str]) -> String {
    let out = ws.lode(args).success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// `--json` output, pretty, with the client version masked.
fn json(ws: &TestWorkspace, args: &[&str]) -> String {
    let v: Value = ws.json(args);
    serde_json::to_string_pretty(&v)
        .unwrap()
        .replace(env!("CARGO_PKG_VERSION"), "[version]")
}

fn remote<'a>(server: &'a TestServer, args: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["--server", server.url()];
    v.extend_from_slice(args);
    v.extend(["--depot", "game"]);
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_output() {
    let server = seeded().await;
    let ws = bare(&server);
    insta::assert_snapshot!("log", text(&ws, &remote(&server, &["log"])));
    insta::assert_snapshot!("log_verbose", text(&ws, &remote(&server, &["log", "-v"])));
    insta::assert_snapshot!(
        "log_path_follow",
        text(&ws, &remote(&server, &["log", "--follow", "src/app.rs"]))
    );
    insta::assert_snapshot!(
        "log_n1_json",
        json(&ws, &remote(&server, &["log", "-n", "1"]))
    );
    // Without --follow the history of the new name starts at the rename.
    let v = ws.json(&remote(&server, &["log", "src/app.rs"]));
    assert_eq!(v["revisions"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ls_output() {
    let server = seeded().await;
    let ws = bare(&server);
    insta::assert_snapshot!("ls_root", text(&ws, &remote(&server, &["ls"])));
    insta::assert_snapshot!("ls_long", text(&ws, &remote(&server, &["ls", "-l"])));
    insta::assert_snapshot!("ls_r2_src", text(&ws, &remote(&server, &["ls", "src@r2"])));
    insta::assert_snapshot!(
        "ls_file_json",
        json(&ws, &remote(&server, &["ls", "tool.sh"]))
    );
    let (code, err) = ws.json_status(&remote(&server, &["ls", "nope/x"]));
    assert_eq!(code, 1, "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_output() {
    let server = seeded().await;
    let ws = bare(&server);
    assert_eq!(
        text(&ws, &remote(&server, &["cat", "README.md"])),
        "hello\nworld\n"
    );
    assert_eq!(
        text(&ws, &remote(&server, &["cat", "README.md@2"])),
        "hello\n"
    );
    // Binary content comes out byte for byte.
    let out = ws
        .lode(&remote(&server, &["cat", "assets/blob.bin"]))
        .success();
    assert_eq!(out.get_output().stdout, binary());
    insta::assert_snapshot!(
        "cat_json",
        json(&ws, &remote(&server, &["cat", "README.md@r3"]))
    );
    let v = ws.json(&remote(&server, &["cat", "assets/blob.bin"]));
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(v["content_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes, binary());
    let (code, err) = ws.json_status(&remote(&server, &["cat", "src/main.rs"]));
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("not_found"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_a_workspace_paths_are_local_and_reads_use_the_synced_revision() {
    let server = seeded().await;
    let ws = bare(&server);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    // Another commit after the checkout: `cat` still shows the synced revision.
    server
        .commit_files("game", "main", &[("README.md", b"newer\n")])
        .await;
    let sub = ws.root().join("src");
    let in_src = |args: &[&str]| {
        let out = ws.cmd().arg("-C").arg(&sub).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(in_src(&["cat", "../README.md"]), "hello\nworld\n");
    assert_eq!(in_src(&["cat", "../README.md@5"]), "newer\n");
    assert_eq!(in_src(&["ls"]), "app.rs\n");
    assert!(in_src(&["log", "-n", "1"]).starts_with("r5 "));
    assert!(in_src(&["log", "app.rs"]).starts_with("r4 "));
    // Outside a workspace, without --depot, it is a usage error.
    let (code, _) = TestWorkspace::new(&server)
        .with_token(server.admin_token())
        .json_status(&["log"]);
    assert_eq!(code, 2);
}
