//! T054 hostile-client tests for the server: a body that stalls, a decompression bomb, paths
//! that try to leave the depot, and a JSON body larger than allowed. Each is refused, nothing
//! is stored, and the depot still checks clean.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind};
use lode_proto::commit::CommitRequest;
use lode_testkit::{TestServer, TestServerOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_BLOB: u64 = 1024 * 1024;

async fn server() -> TestServer {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.limits.body_idle_seconds = 1;
            c.limits.max_blob_size = MAX_BLOB;
        }),
    })
    .await;
    server.create_depot("game").await;
    server
}

async fn connect(server: &TestServer) -> TcpStream {
    let addr = server.url().trim_start_matches("http://").to_owned();
    TcpStream::connect(addr).await.unwrap()
}

/// Sends one request with `body` and returns the status code and the response body.
async fn send(
    server: &TestServer,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, String) {
    let mut s = connect(server).await;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {}\r\nContent-Length: {}\r\n",
        server.admin_token(),
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    // The server may answer and close before taking the whole body.
    let _ = s.write_all(body).await;
    read_response(&mut s).await
}

async fn read_response(s: &mut TcpStream) -> (u16, String) {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut out))
        .await
        .expect("the server answers or closes");
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
    (status, body)
}

async fn clean(server: &TestServer) {
    let report = server.admin().fsck("game", true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

async fn stored(server: &TestServer, id: &lode_core::ObjectId) -> bool {
    let missing = server
        .admin()
        .missing_objects("game", std::slice::from_ref(id))
        .await
        .unwrap();
    missing.is_empty()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_upload_is_dropped() {
    let server = server().await;
    let data = vec![b'x'; 4096];
    let id = blob_id(&data[..]).unwrap();
    let mut s = connect(&server).await;
    let head = format!(
        "PUT /api/v1/depots/game/blobs/{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        id.to_hex(),
        server.admin_token(),
        data.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(&data[..100]).await.unwrap();
    // …and nothing more.
    let started = Instant::now();
    let (status, body) = read_response(&mut s).await;
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(900) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
    assert!(status == 0 || status >= 400, "{status} {body}");
    assert!(!stored(&server, &id).await);
    clean(&server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decompression_bomb_is_too_large() {
    let server = server().await;
    let data = vec![0u8; 16 * 1024 * 1024];
    let id = blob_id(&data[..]).unwrap();
    let packed = zstd::encode_all(&data[..], 19).unwrap();
    assert!(packed.len() < 64 * 1024);
    let (status, body) = send(
        &server,
        "PUT",
        &format!("/api/v1/depots/game/blobs/{}", id.to_hex()),
        &[("Content-Encoding", "zstd")],
        &packed,
    )
    .await;
    assert_eq!(status, 413, "{body}");
    assert!(body.contains("too_large"), "{body}");
    assert!(!stored(&server, &id).await);
    clean(&server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paths_leaving_the_depot_are_refused() {
    let server = server().await;
    let data = b"evil";
    let id = blob_id(&data[..]).unwrap();
    server
        .admin()
        .put_blob("game", &id, std::io::Cursor::new(data.to_vec()))
        .await
        .unwrap();
    let before = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;
    for path in [
        "../x",
        "/etc/x",
        "a/../../x",
        "a\\..\\x",
        "a/./b",
        "",
        "a//b",
        "C:x",
    ] {
        let mut req = serde_json::to_value(CommitRequest {
            request_id: format!("evil-{path}"),
            message: "evil".into(),
            changes: vec![Change {
                path: DepotPath::parse("ok.txt").unwrap(),
                action: ChangeAction::Add,
                base_id: None,
                new_id: Some(id),
                kind: Some(EntryKind::File),
                executable: false,
                size: Some(4),
                copy_from: None,
            }],
            merged_from: vec![],
            provenance: None,
            extensions: Default::default(),
        })
        .unwrap();
        req["changes"][0]["path"] = path.into();
        let (status, body) = send(
            &server,
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            &[("Content-Type", "application/json")],
            req.to_string().as_bytes(),
        )
        .await;
        assert_eq!(status, 400, "{path}: {body}");
    }
    let head = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;
    assert_eq!(head, before);
    clean(&server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_fields_are_refused() {
    let server = server().await;
    // A message far over the JSON limit.
    let req = serde_json::json!({
        "request_id": "big",
        "message": "m".repeat(64 * 1024 * 1024),
        "changes": [],
    });
    let (status, body) = send(
        &server,
        "POST",
        "/api/v1/depots/game/branches/main/commits",
        &[("Content-Type", "application/json")],
        req.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, 413, "{body}");
    // A blob whose declared length is over the limit is refused before any byte is read.
    let mut s = connect(&server).await;
    let id = blob_id(&b"x"[..]).unwrap();
    let head = format!(
        "PUT /api/v1/depots/game/blobs/{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        id.to_hex(),
        server.admin_token(),
        MAX_BLOB * 4
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let (status, body) = read_response(&mut s).await;
    assert_eq!(status, 413, "{body}");
    clean(&server).await;
}
