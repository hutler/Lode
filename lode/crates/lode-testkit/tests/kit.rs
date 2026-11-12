//! T027 acceptance: the sample test, parallel servers, failpoints, request counts and the
//! CLI wrapper.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::TryStreamExt;
use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind, ErrorCode};
use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::commit::CommitRequest;
use lode_server::Clock;
use lode_testkit::{failpoint, TestServer, TestWorkspace};

fn add(path: &str, content: &[u8]) -> Change {
    Change {
        path: DepotPath::parse(path).unwrap(),
        action: ChangeAction::Add,
        base_id: None,
        new_id: Some(blob_id(content).unwrap()),
        kind: Some(EntryKind::File),
        executable: false,
        size: Some(content.len() as u64),
        copy_from: None,
    }
}

fn request(id: &str, changes: Vec<Change>) -> CommitRequest {
    CommitRequest {
        request_id: id.into(),
        message: id.into(),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    }
}

/// The sample test of the task: a server, a depot, one file committed through
/// `lode-client` and read back.
#[tokio::test]
async fn sample_commit_and_read_back() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let c = server.admin();
    let content = b"hello from the test kit\n";
    let id = blob_id(&content[..]).unwrap();
    c.put_blob("game", &id, std::io::Cursor::new(content.to_vec()))
        .await
        .unwrap();
    let rev = c
        .commit(
            "game",
            "main",
            &request("r1", vec![add("docs/hello.txt", content)]),
        )
        .await
        .unwrap()
        .rev;
    let file = c
        .file("game", rev, &DepotPath::parse("docs/hello.txt").unwrap())
        .await
        .unwrap();
    let mut out = Vec::new();
    file.write_to(&mut out).await.unwrap();
    assert_eq!(out, content);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn servers_do_not_interfere() {
    let (a, b) = tokio::join!(TestServer::start(), TestServer::start());
    assert_ne!(a.url(), b.url());
    assert_ne!(a.data_dir(), b.data_dir());
    a.create_depot("game").await;
    b.create_depot("game").await;
    let ra = a.commit_files("game", "main", &[("a.txt", b"a")]).await;
    let rb = b.commit_files("game", "main", &[("b.txt", b"b")]).await;
    let la: Vec<_> = a
        .admin()
        .manifest("game", ra, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let lb: Vec<_> = b
        .admin()
        .manifest("game", rb, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(la.len(), 1);
    assert_eq!(la[0].path.as_str(), "a.txt");
    assert_eq!(lb[0].path.as_str(), "b.txt");
}

#[tokio::test]
async fn failpoints_abort_the_commit_at_each_step() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let c = server.admin();
    let content = b"x";
    let id = blob_id(&content[..]).unwrap();
    c.put_blob("game", &id, std::io::Cursor::new(content.to_vec()))
        .await
        .unwrap();
    let head = || async { c.branch("game", "main").await.unwrap().head_rev };
    let start = head().await;

    // Before anything durable, and inside the transaction: nothing is committed, and the
    // same request goes through once the point is disarmed.
    for (i, point) in [
        failpoint::COMMIT_BEFORE_TREES,
        failpoint::COMMIT_IN_TRANSACTION,
    ]
    .into_iter()
    .enumerate()
    {
        let req = request(&format!("fp{i}"), vec![add(&format!("f{i}.txt"), content)]);
        server.failpoints().arm(point, 1);
        let err = c.commit("game", "main", &req).await.unwrap_err();
        assert!(err.is_code(ErrorCode::Internal), "{point}: {err:?}");
        assert_eq!(head().await, start + i as u64, "{point}");
        let ok = c.commit("game", "main", &req).await.unwrap();
        assert!(!ok.replayed);
        assert_eq!(head().await, start + i as u64 + 1);
    }

    // After the transaction: the revision exists although the client saw an error; the
    // retry with the same request id is a replay of it.
    let req = request("fp-after", vec![add("after.txt", content)]);
    server
        .failpoints()
        .arm(failpoint::COMMIT_AFTER_TRANSACTION, 1);
    let before = head().await;
    let err = c.commit("game", "main", &req).await.unwrap_err();
    assert!(err.is_code(ErrorCode::Internal));
    assert_eq!(head().await, before + 1);
    let again = c.commit("game", "main", &req).await.unwrap();
    assert!(again.replayed);
    assert_eq!(again.rev, before + 1);

    // The depot is consistent after all of it.
    assert!(c.fsck("game", true).await.unwrap().problems.is_empty());
}

#[tokio::test]
async fn requests_are_counted() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server.requests().clear();
    server
        .commit_files("game", "main", &[("a.txt", b"a"), ("b.txt", b"b")])
        .await;
    assert_eq!(server.requests().count("PUT", "/blobs/"), 2);
    assert_eq!(server.requests().count("POST", "/commits"), 1);
}

#[tokio::test]
async fn users_agents_and_rules() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let rev = server
        .commit_files(
            "game",
            "main",
            &[("open/a.txt", b"a"), ("secret/b.txt", b"b")],
        )
        .await;
    let dana = server.create_user("dana", &["read", "write"]).await;
    server
        .acl(
            "game",
            "dana",
            "**",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    server
        .acl(
            "game",
            "dana",
            "**",
            "secret/**",
            AclEffect::Deny,
            AccessLevel::Read,
        )
        .await;
    let visible: Vec<_> = server
        .client(&dana)
        .manifest("game", rev, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].path.as_str(), "open/a.txt");

    let bot = server.create_agent("dana-bot", "dana", &["read"]).await;
    let me = server.client(&bot).whoami().await.unwrap();
    assert_eq!(me.principal.name, "dana-bot");
    assert_eq!(me.principal.owner.as_deref(), Some("dana"));

    // The clock is the test's.
    let t = server.clock().now_ms();
    server.clock().advance_ms(60_000);
    assert_eq!(server.clock().now_ms(), t + 60_000);
}

#[test]
fn the_cli_runs_isolated() {
    let ws = TestWorkspace::detached();
    let out = ws
        .lode(&["--version"])
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(out).unwrap().starts_with("lode "));
    ws.write("dir/file.txt", "content");
    assert_eq!(ws.read("dir/file.txt"), b"content");
    assert!(ws.exists("dir/file.txt"));
    assert!(ws.config_dir().starts_with(ws.home()));
    assert!(ws.root().starts_with(ws.home()));
}
