//! T054: the server stopped abruptly at each failpoint of the commit path and of GC, then
//! restarted on the same data. Every depot checks clean with `fsck`, and a client retry
//! behaves as SPEC §10 says: a commit the server never finished is made once on retry, one it
//! finished is replayed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;

use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind, ErrorCode};
use lode_proto::commit::CommitRequest;
use lode_testkit::{failpoint, TestServer, TestServerOptions};

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

async fn clean(server: &TestServer) {
    let report = server.admin().fsck("game", true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_path_crashes_recover() {
    let mut server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("base.txt", b"base")])
        .await;
    let head = |s: &TestServer| {
        let c = s.admin();
        async move { c.branch("game", "main").await.unwrap().head_rev }
    };

    for (i, point) in [
        failpoint::COMMIT_BEFORE_TREES,
        failpoint::COMMIT_IN_TRANSACTION,
        failpoint::COMMIT_AFTER_TRANSACTION,
    ]
    .into_iter()
    .enumerate()
    {
        let content = format!("content {i}");
        let id = blob_id(content.as_bytes()).unwrap();
        server
            .admin()
            .put_blob("game", &id, Cursor::new(content.clone().into_bytes()))
            .await
            .unwrap();
        let req = request(
            &format!("crash-{i}"),
            vec![add(&format!("f{i}.txt"), content.as_bytes())],
        );
        let before = head(&server).await;
        server.failpoints().arm(point, 1);
        let err = server
            .admin()
            .commit("game", "main", &req)
            .await
            .unwrap_err();
        assert!(err.is_code(ErrorCode::Internal), "{point}: {err:?}");

        server.restart().await;
        clean(&server).await;
        let committed = point == failpoint::COMMIT_AFTER_TRANSACTION;
        assert_eq!(
            head(&server).await,
            before + u64::from(committed),
            "{point}"
        );

        // The client retries with the same request id.
        let ok = server.admin().commit("game", "main", &req).await.unwrap();
        assert_eq!(ok.replayed, committed, "{point}");
        assert_eq!(ok.rev, before + 1, "{point}");
        assert_eq!(head(&server).await, before + 1, "{point}");
        clean(&server).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gc_crashes_recover() {
    let mut server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.gc.grace_seconds = 0),
    })
    .await;
    server.create_depot("game").await;
    let rev = server
        .commit_files(
            "game",
            "main",
            &[("keep.txt", b"keep"), ("also.txt", b"also")],
        )
        .await;
    // Unreferenced uploads: garbage once the grace period has passed.
    let upload = |s: &TestServer, n: usize| {
        let c = s.admin();
        async move {
            for k in 0..n {
                let data = format!("orphan {k} {}", n * 100);
                let id = blob_id(data.as_bytes()).unwrap();
                c.put_blob("game", &id, Cursor::new(data.into_bytes()))
                    .await
                    .unwrap();
            }
        }
    };
    upload(&server, 3).await;
    server.clock().advance_ms(10_000);

    for point in [failpoint::GC_AFTER_MARK, failpoint::GC_IN_BATCH] {
        server.failpoints().arm(point, 1);
        let err = server.admin().gc("game", false).await.unwrap_err();
        assert!(err.is_code(ErrorCode::Internal), "{point}: {err:?}");
        server.restart().await;
        clean(&server).await;
    }
    let report = server.admin().gc("game", false).await.unwrap();
    // One orphan went in the interrupted batch; the other two now.
    assert_eq!(report.objects_affected, 2);
    clean(&server).await;

    // Everything committed is still there.
    for (path, want) in [("keep.txt", &b"keep"[..]), ("also.txt", &b"also"[..])] {
        let f = server
            .admin()
            .file("game", rev, &DepotPath::parse(path).unwrap())
            .await
            .unwrap();
        let mut body = Vec::new();
        f.write_to(&mut body).await.unwrap();
        assert_eq!(body, want);
    }
    // And the server takes commits after all of it.
    server
        .commit_files("game", "main", &[("after.txt", b"after")])
        .await;
    clean(&server).await;
}
