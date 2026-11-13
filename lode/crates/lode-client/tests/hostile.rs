//! T054 hostile-server tests: a server that stalls, sends more than it should, or sends a
//! decompression bomb. The client gives up with an error and leaves nothing behind.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lode_client::{Client, Error, RetryPolicy};
use lode_core::blob_id;

/// `n` bytes of `fill`, streamed in 64 KiB chunks without a length.
fn stream_of(n: usize, fill: u8) -> Body {
    let chunk = Bytes::from(vec![fill; 64 * 1024]);
    let chunks = n.div_ceil(chunk.len());
    Body::from_stream(futures::stream::iter(
        (0..chunks).map(move |_| Ok::<_, std::io::Error>(chunk.clone())),
    ))
}

/// A few bytes, then nothing, forever.
fn stalled() -> Body {
    let first = futures::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"st")) });
    let never = futures::stream::pending::<Result<Bytes, std::io::Error>>();
    Body::from_stream(futures::StreamExt::chain(first, never))
}

async fn blob(Path((depot, _id)): Path<(String, String)>) -> Response {
    match depot.as_str() {
        "slow" => stalled().into_response(),
        "big" => stream_of(1024 * 1024, b'x').into_response(),
        "bomb" => {
            let zeros = vec![0u8; 64 * 1024 * 1024];
            let packed = zstd::encode_all(&zeros[..], 19).unwrap();
            ([("content-encoding", "zstd")], packed).into_response()
        }
        _ => "x".into_response(),
    }
}

async fn revision(Path((depot, _n)): Path<(String, u64)>) -> Response {
    match depot.as_str() {
        "slow" => ([("content-type", "application/json")], stalled()).into_response(),
        "silent" => {
            futures::future::pending::<()>().await;
            unreachable!()
        }
        // An endless JSON string.
        "huge" => {
            let head = futures::stream::once(async {
                Ok::<_, std::io::Error>(Bytes::from_static(b"{\"message\":\""))
            });
            let body = Body::from_stream(futures::StreamExt::chain(
                head,
                futures::stream::iter((0..2048).map(|_| Ok(Bytes::from(vec![b'a'; 64 * 1024])))),
            ));
            ([("content-type", "application/json")], body).into_response()
        }
        _ => "{}".into_response(),
    }
}

async fn client(read_timeout: Duration) -> Client {
    let app = Router::new()
        .route("/api/v1/depots/{d}/blobs/{id}", get(blob))
        .route("/api/v1/depots/{d}/revisions/{n}", get(revision));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Client::builder(url)
        .retry(RetryPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .read_timeout(read_timeout)
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_stalled_body_or_answer_times_out() {
    let c = client(Duration::from_millis(300)).await;
    let id = blob_id(&b"anything"[..]).unwrap();
    let started = Instant::now();
    let mut out = Vec::new();
    let err = c.get_blob_to("slow", &id, &mut out).await.unwrap_err();
    assert!(matches!(err, Error::Transport(_) | Error::Io(_)), "{err:?}");
    let err = c.revision("slow", 1).await.unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err:?}");
    let err = c.revision("silent", 1).await.unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn more_than_expected_is_refused_and_leaves_nothing() {
    let c = client(Duration::from_secs(30)).await;
    let id = blob_id(&b"small"[..]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f");
    let err = c.download_blob_max("big", &id, &dest, 5).await.unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let mut out = Vec::new();
    let err = c
        .get_blob_to_max("big", &id, &mut out, 5)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert!(out.len() <= 5, "{} bytes written", out.len());
}

#[tokio::test]
async fn a_decompression_bomb_stops_at_the_limit() {
    let c = client(Duration::from_secs(30)).await;
    let id = blob_id(&b"small"[..]).unwrap();
    let mut out = Vec::new();
    let err = c
        .get_blob_to_max("bomb", &id, &mut out, 1024)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert!(out.len() <= 1024, "{} bytes written", out.len());
}

#[tokio::test]
async fn an_endless_json_answer_is_cut_off() {
    let c = client(Duration::from_secs(30)).await;
    let err = c.revision("huge", 1).await.unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
}
