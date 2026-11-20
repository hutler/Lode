//! T176 acceptance: `lode link` prints the same `lode://` links Lode Desk copies, and
//! `lode shelve --json` carries the shelf's link.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_ops::links::{self, Link};
use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_prints_file_revision_and_shelf_links() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let rev = server
        .commit_files("game", "main", &[("Content/hero one.txt", b"hi\n")])
        .await;
    let ana = server.create_user("ana", &["read", "write", "shelve"]).await;
    server
        .acl("game", "ana", "*", "**", AclEffect::Allow, AccessLevel::Write)
        .await;
    let ws = TestWorkspace::new(&server).with_token(&ana);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    // The test server is plain http on loopback: links say so with `http=1`.
    let host = server.url().trim_start_matches("http://").trim_end_matches('/').to_owned();

    let v = ws.json(&["link", "Content/hero one.txt"]);
    let text = v["link"].as_str().unwrap();
    assert_eq!(text, format!("lode://{host}/game/Content/hero%20one.txt@{rev}?http=1"));
    assert_eq!(v["kind"], "file");
    assert!(matches!(links::parse(text).unwrap(), Link::File { rev: Some(r), .. } if r == rev));

    let v = ws.json(&["link", "Content/hero one.txt", "--branch", "main"]);
    assert_eq!(
        v["link"],
        format!("lode://{host}/game/Content/hero%20one.txt?branch=main&http=1")
    );
    let v = ws.json(&["link", "-r", "1"]);
    assert_eq!(v["link"], format!("lode://{host}/game/r1?http=1"));
    let v = ws.json(&["link", "--shelf", "7"]);
    assert_eq!(v["link"], format!("lode://{host}/game/shelf/7?http=1"));

    // Human output is the link alone.
    let out = ws.cmd().args(["link", "-r", "1"]).output().unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("lode://{host}/game/r1?http=1\n")
    );

    // Outside a workspace, with --depot: depot paths, the head of main by default.
    let outside = TestWorkspace::detached();
    let v = outside.json(&[
        "--server",
        server.url(),
        "link",
        "--depot",
        "game",
        "Content/a.txt@3",
    ]);
    assert_eq!(v["link"], format!("lode://{host}/game/Content/a.txt@3?http=1"));
    let (code, err) = ws.json_status(&["link"]);
    assert_eq!(code, 2, "{err}");

    // shelve --json carries the shelf's link.
    ws.write("Content/hero one.txt", "changed\n");
    let shelf = ws.json(&["shelve", "-m", "try"]);
    let id = shelf["shelf"].as_u64().unwrap();
    assert_eq!(
        shelf["link"],
        format!("lode://{host}/game/shelf/{id}?http=1")
    );
}
