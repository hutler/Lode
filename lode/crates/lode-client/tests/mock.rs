//! T020 acceptance against a mock server: error decoding, retries, verified downloads,
//! uploads, manifests and the plain-HTTP rule.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use futures::TryStreamExt;
use lode_client::{Client, Error, RetryPolicy};
use lode_core::{blob_id, ChangeAction, DepotPath, EntryKind, ErrorCode, ObjectId};
use lode_proto::commit::CommitRequest;
use lode_proto::read::{ManifestDelta, ManifestEntry};
use serde_json::json;

#[derive(Default)]
struct Mock {
    hits: Mutex<HashMap<String, usize>>,
    uploads: Mutex<Vec<(Option<String>, Vec<u8>)>>,
    auth: Mutex<Vec<Option<String>>>,
    blobs: Mutex<HashMap<String, (Vec<u8>, bool)>>,
}

impl Mock {
    fn hit(&self, key: &str) -> usize {
        let mut h = self.hits.lock().unwrap();
        let n = h.entry(key.to_owned()).or_default();
        *n += 1;
        *n
    }
    fn hits(&self, key: &str) -> usize {
        self.hits.lock().unwrap().get(key).copied().unwrap_or(0)
    }
}

fn error(code: &str, status: u16, details: serde_json::Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        axum::Json(json!({ "code": code, "message": format!("it is {code}"), "details": details })),
    )
        .into_response()
}

async fn info(State(m): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    m.auth.lock().unwrap().push(
        headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
    );
    let ua = headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ua.contains("lode-client/"), "{ua}");
    // Unavailable once, then fine.
    if m.hit("info") == 1 {
        return (StatusCode::SERVICE_UNAVAILABLE, "try later").into_response();
    }
    axum::Json(json!({ "server_version": "9.9", "api_version": "1", "modules": ["locks"] }))
        .into_response()
}

async fn revision(Path((_d, n)): Path<(String, usize)>) -> Response {
    if n < ErrorCode::ALL.len() {
        let code = ErrorCode::ALL[n];
        return error(
            code.as_str(),
            code.http_status(),
            json!({ "paths": ["a/b.txt"] }),
        );
    }
    match n {
        100 => error("frobnicated", 409, serde_json::Value::Null),
        101 => (StatusCode::BAD_REQUEST, "<html>proxy says no</html>").into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn commit(
    State(m): State<Arc<Mock>>,
    Path((_d, branch)): Path<(String, String)>,
) -> Response {
    let n = m.hit(&format!("commit:{branch}"));
    match branch.as_str() {
        "stale" => error("out_of_date", 409, json!({ "paths": ["x.txt"] })),
        "flaky" if n == 1 => (StatusCode::BAD_GATEWAY, "").into_response(),
        _ => axum::Json(json!({ "rev": 7, "branch": branch, "replayed": n > 1 })).into_response(),
    }
}

async fn put_blob(
    State(m): State<Arc<Mock>>,
    Path((_d, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let enc = headers
        .get("content-encoding")
        .map(|v| v.to_str().unwrap().to_owned());
    m.uploads.lock().unwrap().push((enc, body.to_vec()));
    (
        StatusCode::CREATED,
        axum::Json(json!({ "id": id, "created": true })),
    )
        .into_response()
}

async fn get_blob(
    State(m): State<Arc<Mock>>,
    Path((_d, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    m.hit(&format!("blob:{id}"));
    let Some((content, zstd)) = m.blobs.lock().unwrap().get(&id).cloned() else {
        return error("not_found", 404, serde_json::Value::Null);
    };
    let accepts = headers
        .get("accept-encoding")
        .is_some_and(|v| v.to_str().unwrap().contains("zstd"));
    if zstd {
        assert!(accepts);
        return (
            [("content-encoding", "zstd")],
            zstd::encode_all(&content[..], 3).unwrap(),
        )
            .into_response();
    }
    content.into_response()
}

async fn manifest(axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    let q = q.unwrap_or_default();
    let a = blob_id(&b"a"[..]).unwrap();
    let body = if q.contains("base=") {
        format!(
            "{}\n{}\n",
            json!({ "op": "put", "path": "dir/a.txt", "kind": "file", "id": a, "size": 1 }),
            json!({ "op": "del", "path": "old.txt" })
        )
    } else if q.contains("evil") {
        format!(
            "{}\n",
            json!({ "path": "../x", "kind": "file", "id": a, "size": 1 })
        )
    } else {
        assert!(q.contains("include=src%2F**"), "{q}");
        // Split oddly and without a final newline, to exercise line assembly.
        format!(
            "{}\n\n{}",
            json!({ "path": "dir/a.txt", "kind": "file", "id": a, "size": 1, "executable": true }),
            json!({ "path": "link", "kind": "symlink", "id": a, "size": 1 })
        )
    };
    let chunks: Vec<Result<Bytes, std::io::Error>> = body
        .into_bytes()
        .chunks(7)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    (
        [
            ("x-lode-access-epoch", "42"),
            ("content-type", "application/x-ndjson"),
        ],
        Body::from_stream(futures::stream::iter(chunks)),
    )
        .into_response()
}

async fn start() -> (Arc<Mock>, String) {
    let mock = Arc::new(Mock::default());
    let app = Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/depots/{d}/revisions/{n}", get(revision))
        .route("/api/v1/depots/{d}/branches/{b}/commits", post(commit))
        .route("/api/v1/depots/{d}/blobs/{id}", put(put_blob).get(get_blob))
        .route("/api/v1/depots/{d}/manifest", get(manifest))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (mock, url)
}

fn fast() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    }
}

async fn client() -> (Arc<Mock>, Client) {
    let (mock, url) = start().await;
    let c = Client::builder(url)
        .token("tok-1")
        .retry(fast())
        .build()
        .unwrap();
    (mock, c)
}

#[tokio::test]
async fn every_error_code_decodes() {
    let (_m, c) = client().await;
    for (i, code) in ErrorCode::ALL.iter().enumerate() {
        let err = c.revision("game", i as u64).await.unwrap_err();
        let api = err.api().unwrap_or_else(|| panic!("{code:?}: {err:?}"));
        assert_eq!(api.status, code.http_status(), "{code:?}");
        assert_eq!(api.known_code(), Some(*code));
        assert!(err.is_code(*code));
        assert_eq!(api.message, format!("it is {}", code.as_str()));
        assert_eq!(api.paths(), vec!["a/b.txt".to_owned()]);
    }
    // A module's own code arrives intact.
    let err = c.revision("game", 100).await.unwrap_err();
    assert_eq!(err.api().unwrap().code, "frobnicated");
    assert_eq!(err.api().unwrap().known_code(), None);
    // A body that is not the error shape still gives a code from the status.
    let err = c.revision("game", 101).await.unwrap_err();
    let api = err.api().unwrap();
    assert_eq!(api.known_code(), Some(ErrorCode::InvalidRequest));
    assert!(api.message.contains("proxy says no"));
}

#[tokio::test]
async fn unavailable_is_retried_and_the_token_is_sent() {
    let (m, c) = client().await;
    let info = c.info().await.unwrap();
    assert_eq!(info.server_version, "9.9");
    assert_eq!(m.hits("info"), 2);
    let auth = m.auth.lock().unwrap().clone();
    assert_eq!(auth, vec![Some("Bearer tok-1".to_owned()); 2]);
}

#[tokio::test]
async fn retries_are_bounded() {
    let (m, url) = start().await;
    let c = Client::builder(url)
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let err = c.info().await.unwrap_err();
    assert_eq!(err.api().unwrap().status, 503);
    assert!(err.is_transient());
    assert_eq!(m.hits("info"), 1);
}

fn commit_req() -> CommitRequest {
    CommitRequest {
        request_id: "r1".into(),
        message: "m".into(),
        changes: vec![lode_core::Change {
            path: DepotPath::parse("x.txt").unwrap(),
            action: ChangeAction::Delete,
            base_id: None,
            new_id: None,
            kind: None,
            size: None,
            executable: false,
            copy_from: None,
        }],
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    }
}

#[tokio::test]
async fn conflict_is_not_retried_but_a_gateway_error_is() {
    let (m, c) = client().await;
    let err = c.commit("game", "stale", &commit_req()).await.unwrap_err();
    assert!(err.is_code(ErrorCode::OutOfDate));
    assert!(!err.is_transient());
    assert_eq!(m.hits("commit:stale"), 1);

    // Commits are retried because the request id makes a repeat harmless.
    let resp = c.commit("game", "flaky", &commit_req()).await.unwrap();
    assert_eq!(resp.rev, 7);
    assert_eq!(m.hits("commit:flaky"), 2);

    // A branch name with `/` is one path segment.
    let resp = c
        .commit("game", "release/1.0", &commit_req())
        .await
        .unwrap();
    assert_eq!(resp.branch, "release/1.0");
}

#[tokio::test]
async fn corrupted_download_fails_and_leaves_nothing() {
    let (m, c) = client().await;
    let dir = tempfile::tempdir().unwrap();
    let good = b"the real content".repeat(1000);
    let id = blob_id(&good[..]).unwrap();
    let wrong_id = blob_id(&b"something else"[..]).unwrap();

    for zstd in [false, true] {
        // Served under the wrong id: the bytes do not hash to what was asked for.
        m.blobs
            .lock()
            .unwrap()
            .insert(wrong_id.to_string(), (good.clone(), zstd));
        let dest = dir.path().join("out.bin");
        let err = c.download_blob("game", &wrong_id, &dest).await.unwrap_err();
        match err {
            Error::HashMismatch { expected, actual } => {
                assert_eq!(expected, wrong_id);
                assert_eq!(actual, id);
            }
            other => panic!("{other:?}"),
        }
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        // The real thing works, plain and compressed, and replaces an existing file.
        std::fs::write(&dest, b"old").unwrap();
        m.blobs
            .lock()
            .unwrap()
            .insert(id.to_string(), (good.clone(), zstd));
        let n = c.download_blob("game", &id, &dest).await.unwrap();
        assert_eq!(n, good.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), good);
        std::fs::remove_file(&dest).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    // Into any writer.
    let mut out = Vec::new();
    c.get_blob_to("game", &id, &mut out).await.unwrap();
    assert_eq!(out, good);

    // A missing blob is the server's not_found, not retried.
    let missing = ObjectId::from_bytes([7; 32]);
    let before = m.hits(&format!("blob:{missing}"));
    let err = c
        .get_blob_to("game", &missing, &mut Vec::new())
        .await
        .unwrap_err();
    assert!(err.is_code(ErrorCode::NotFound));
    assert_eq!(m.hits(&format!("blob:{missing}")), before + 1);
}

#[tokio::test]
async fn uploads_are_compressed_unless_disabled() {
    let (m, url) = start().await;
    let content = b"compress me ".repeat(10_000);
    let id = blob_id(&content[..]).unwrap();
    let c = Client::builder(url.clone()).build().unwrap();
    let resp = c
        .put_blob("game", &id, std::io::Cursor::new(content.clone()))
        .await
        .unwrap();
    assert!(resp.created);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, &content).unwrap();
    let plain = Client::builder(url)
        .upload_compression(None)
        .build()
        .unwrap();
    plain.put_blob_file("game", &id, &file).await.unwrap();

    let uploads = m.uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 2);
    assert_eq!(uploads[0].0.as_deref(), Some("zstd"));
    assert!(uploads[0].1.len() < content.len() / 10);
    assert_eq!(zstd::decode_all(&uploads[0].1[..]).unwrap(), content);
    assert_eq!(uploads[1].0, None);
    assert_eq!(uploads[1].1, content);
}

#[tokio::test]
async fn manifests_stream_typed_lines() {
    let (_m, c) = client().await;
    let full = c
        .manifest("game", 3, &["src/**".to_owned()], &[])
        .await
        .unwrap();
    assert_eq!(full.access_epoch(), Some(42));
    let entries: Vec<ManifestEntry> = full.try_collect().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path.as_str(), "dir/a.txt");
    assert!(entries[0].executable);
    assert_eq!(entries[1].kind, EntryKind::Symlink);

    let delta: Vec<ManifestDelta> = c
        .manifest_delta("game", 2, 3, &[], &[])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(delta.len(), 2);
    assert!(matches!(&delta[1], ManifestDelta::Del { path } if path.as_str() == "old.txt"));

    // A path that is not a valid depot path is refused, not passed on.
    let err = c
        .manifest("game", 3, &["evil".to_owned()], &[])
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn plain_http_to_another_host_is_refused() {
    let err = Client::builder("http://lode.example.com")
        .build()
        .unwrap_err();
    assert!(
        matches!(err, Error::InsecureUrl(ref h) if h == "lode.example.com"),
        "{err:?}"
    );
    assert!(err.to_string().contains("refusing plain HTTP"));
    assert!(Client::builder("http://lode.example.com")
        .allow_insecure_http(true)
        .build()
        .is_ok());
    assert!(Client::builder("http://127.0.0.1:9").build().is_ok());
}
