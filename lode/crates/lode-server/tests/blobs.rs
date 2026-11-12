//! T013 acceptance: depots and blob transfer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use lode_core::{blob_id, ObjectId, Tree};
use lode_meta::Reads;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
    dir: tempfile::TempDir,
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let state = open_state(
        ServerConfig::with_data_dir(dir.path()),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        vec![],
    )
    .await
    .unwrap();
    let h = H { state, admin, dir };
    let (s, body) = h
        .json(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    h
}

struct Raw {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Bytes,
}

impl H {
    async fn raw(&self, req: Request<Body>) -> Raw {
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        Raw {
            status,
            headers,
            body,
        }
    }

    async fn json(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let r = self.raw(req).await;
        (
            r.status,
            serde_json::from_slice(&r.body).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, uri: &str, token: &str) -> (StatusCode, Value) {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let r = self.raw(req).await;
        (
            r.status,
            serde_json::from_slice(&r.body).unwrap_or(Value::Null),
        )
    }

    async fn upload(
        &self,
        token: &str,
        id: &ObjectId,
        body: Body,
        zstd: bool,
    ) -> (StatusCode, Value) {
        let mut b = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {token}"));
        if zstd {
            b = b.header("content-encoding", "zstd");
        }
        let r = self.raw(b.body(body).unwrap()).await;
        (
            r.status,
            serde_json::from_slice(&r.body).unwrap_or(Value::Null),
        )
    }

    async fn download(&self, token: &str, id: &ObjectId, accept_zstd: bool) -> Raw {
        let mut b = Request::get(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {token}"));
        if accept_zstd {
            b = b.header("accept-encoding", "gzip, zstd");
        }
        self.raw(b.body(Body::empty()).unwrap()).await
    }

    async fn token(&self, scopes: &[&str]) -> String {
        let (_, body) = self
            .json(
                "POST",
                "/api/v1/tokens",
                &self.admin,
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        body["token"].as_str().unwrap().to_owned()
    }

    fn tmp_files(&self) -> usize {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(self.dir.path().join("depots")) {
            for d in rd.flatten() {
                if let Ok(t) = std::fs::read_dir(d.path().join("tmp")) {
                    n += t.count();
                }
            }
        }
        n
    }
}

fn id_of(data: &[u8]) -> ObjectId {
    blob_id(data).unwrap()
}

/// Deterministic incompressible bytes, generated on the fly.
struct Noise {
    state: u64,
    left: u64,
}

impl Noise {
    fn new(len: u64) -> Self {
        Self {
            state: 0x9E37_79B9_7F4A_7C15,
            left: len,
        }
    }
}

impl Read for Noise {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        let n = n - n % 8;
        let n = if n == 0 {
            self.left.min(buf.len() as u64) as usize
        } else {
            n
        };
        for chunk in buf[..n].chunks_mut(8) {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            let bytes = self.state.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        self.left -= n as u64;
        Ok(n)
    }
}

fn noise_body(len: u64) -> Body {
    let stream = futures::stream::unfold(Noise::new(len), |mut n| async move {
        let mut buf = vec![0u8; 1 << 20];
        let got = n.read(&mut buf).unwrap();
        if got == 0 {
            return None;
        }
        buf.truncate(got);
        Some((Ok::<_, std::io::Error>(Bytes::from(buf)), n))
    });
    Body::from_stream(stream)
}

#[tokio::test]
async fn depot_create_and_list() {
    let h = harness().await;
    let (s, body) = h
        .json(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    let (s, _) = h
        .json(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "../x" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Branch main at r1 with the empty tree.
    let (branch, rev) = h
        .state
        .meta()
        .read(|tx| {
            let d = tx.depot_by_name("game")?.unwrap();
            let b = tx.branch(d.id, "main")?.unwrap();
            let r = tx.revision(d.id, 1)?.unwrap();
            Ok((b, r))
        })
        .await
        .unwrap();
    assert_eq!((branch.head_rev, branch.created_rev), (1, 1));
    let empty = Tree::from_entries(vec![]).unwrap().id();
    assert_eq!(rev.root_tree, empty);
    assert!(h
        .state
        .blobs()
        .contains(rev.depot_id, &empty)
        .await
        .unwrap());

    let (s, body) = h.get("/api/v1/depots", &h.admin).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["depots"][0]["name"], "game");

    // A user without rules sees no depots and cannot create one.
    h.json(
        "POST",
        "/api/v1/admin/principals",
        &h.admin,
        json!({ "kind": "user", "name": "ana" }),
    )
    .await;
    let (_, t) = h
        .json(
            "POST",
            "/api/v1/admin/principals/ana/tokens",
            &h.admin,
            json!({ "name": "t", "scopes": ["read", "write"] }),
        )
        .await;
    let ana = t["token"].as_str().unwrap().to_owned();
    let (_, body) = h.get("/api/v1/depots", &ana).await;
    assert_eq!(body["depots"], json!([]));
    let (s, _) = h
        .json("POST", "/api/v1/depots", &ana, json!({ "name": "mine" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The depot is invisible to Ana: not_found, not permission_denied.
    assert_eq!(
        h.download(&ana, &empty, false).await.status,
        StatusCode::NOT_FOUND
    );

    let rule = json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "read"
    });
    h.json("POST", "/api/v1/admin/acl", &h.admin, rule).await;
    let (_, body) = h.get("/api/v1/depots", &ana).await;
    assert_eq!(body["depots"][0]["name"], "game");
}

#[tokio::test]
async fn plain_round_trip_and_missing() {
    let h = harness().await;
    let mut data = vec![0u8; 3 << 20];
    Noise::new(data.len() as u64).read_exact(&mut data).unwrap();
    let id = id_of(&data);
    let other = id_of(b"never uploaded");

    let (s, body) = h
        .json(
            "POST",
            "/api/v1/depots/game/objects/missing",
            &h.admin,
            json!({ "ids": [id, other, id] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["missing"], json!([id, other]));

    let (s, body) = h
        .upload(&h.admin, &id, Body::from(data.clone()), false)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["created"], true);
    let (s, body) = h
        .upload(&h.admin, &id, Body::from(data.clone()), false)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["created"], false);

    let (_, body) = h
        .json(
            "POST",
            "/api/v1/depots/game/objects/missing",
            &h.admin,
            json!({ "ids": [id, other] }),
        )
        .await;
    assert_eq!(body["missing"], json!([other]));

    let r = h.download(&h.admin, &id, false).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, data);
    assert_eq!(r.headers["etag"], format!("\"{id}\""));
    assert!(r.headers["cache-control"]
        .to_str()
        .unwrap()
        .contains("immutable"));
    assert_eq!(r.headers["content-length"], data.len().to_string());
    assert!(r.headers.get("content-encoding").is_none());

    let r = h.download(&h.admin, &other, false).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h
        .raw(
            Request::get("/api/v1/depots/game/blobs/xyz")
                .header("authorization", format!("Bearer {}", h.admin))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn zstd_round_trip() {
    let h = harness().await;
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i % 97) as u8).collect();
    let id = id_of(&data);
    let compressed = zstd::encode_all(&data[..], 3).unwrap();
    let (s, body) = h.upload(&h.admin, &id, Body::from(compressed), true).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    let r = h.download(&h.admin, &id, true).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-encoding"], "zstd");
    assert!(r.body.len() < data.len() / 10);
    assert_eq!(r.headers["content-length"], r.body.len().to_string());
    assert_eq!(zstd::decode_all(&r.body[..]).unwrap(), data);

    let r = h.download(&h.admin, &id, false).await;
    assert!(r.headers.get("content-encoding").is_none());
    assert_eq!(r.body, data);

    // A client that does not accept zstd for an incompressible blob gets raw bytes either way.
    let small = b"tiny".to_vec();
    let sid = id_of(&small);
    h.upload(&h.admin, &sid, Body::from(small.clone()), false)
        .await;
    let r = h.download(&h.admin, &sid, true).await;
    assert_eq!(r.body, small);

    let (s, _) = h.upload(&h.admin, &id, Body::from("x"), true).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "garbage zstd");
}

#[tokio::test]
async fn hash_mismatch_stores_nothing() {
    let h = harness().await;
    let claimed = id_of(b"what the client claims");
    let (s, body) = h
        .upload(&h.admin, &claimed, Body::from("something else"), false)
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "hash_mismatch");
    let r = h.download(&h.admin, &claimed, false).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let actual = id_of(b"something else");
    assert_eq!(
        h.download(&h.admin, &actual, false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(h.tmp_files(), 0);
}

#[tokio::test]
async fn aborted_upload_leaves_nothing() {
    let h = harness().await;
    let id = id_of(&[1u8; 3 << 20]);
    let stream = futures::stream::iter(vec![
        Ok(Bytes::from(vec![1u8; 1 << 20])),
        Err(std::io::Error::other("client went away")),
    ]);
    let (s, _) = h
        .upload(&h.admin, &id, Body::from_stream(stream), false)
        .await;
    assert!(s.is_client_error() || s.is_server_error(), "{s}");
    assert_eq!(
        h.download(&h.admin, &id, false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(h.tmp_files(), 0);
}

#[tokio::test]
async fn trees_are_never_served_and_always_missing() {
    let h = harness().await;
    let empty = Tree::from_entries(vec![]).unwrap().id();
    assert_eq!(
        h.download(&h.admin, &empty, false).await.status,
        StatusCode::NOT_FOUND
    );
    let (_, body) = h
        .json(
            "POST",
            "/api/v1/depots/game/objects/missing",
            &h.admin,
            json!({ "ids": [empty] }),
        )
        .await;
    assert_eq!(body["missing"], json!([empty]));
}

#[tokio::test]
async fn scopes_for_transfer() {
    let h = harness().await;
    // Without the admin scope even the admin user goes through access rules.
    let rule = json!({
        "subject": { "kind": "all" }, "branch": "*", "path": "**",
        "effect": "allow", "level": "write"
    });
    h.json("POST", "/api/v1/admin/acl", &h.admin, rule).await;
    let read = h.token(&["read"]).await;
    let id = id_of(b"hello");
    let (s, body) = h
        .json(
            "POST",
            "/api/v1/depots/game/objects/missing",
            &read,
            json!({ "ids": [id] }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "permission_denied");
    let (s, _) = h.upload(&read, &id, Body::from("hello"), false).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    h.upload(&h.admin, &id, Body::from("hello"), false).await;
    let r = h.download(&read, &id, false).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(&r.body[..], b"hello");

    let write_only = h.token(&["write"]).await;
    assert_eq!(
        h.download(&write_only, &id, false).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn large_stream_round_trip() {
    // 300 MiB, generated and checked on the fly; neither side holds it in memory.
    const LEN: u64 = 300 << 20;
    let h = harness().await;
    let id = blob_id(Noise::new(LEN)).unwrap();
    let (s, body) = h.upload(&h.admin, &id, noise_body(LEN), false).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    let req = Request::get(format!("/api/v1/depots/game/blobs/{id}"))
        .header("authorization", format!("Bearer {}", h.admin))
        .body(Body::empty())
        .unwrap();
    let resp = lode_server::router(h.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        total += chunk.len() as u64;
        hasher.update(&chunk);
    }
    assert_eq!(total, LEN);
    assert_eq!(ObjectId::from_bytes(*hasher.finalize().as_bytes()), id);
}
