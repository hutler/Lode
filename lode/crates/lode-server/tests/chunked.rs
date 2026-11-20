//! T150 acceptance: chunked files on the server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::{ChunkList, ChunkingHasher, DepotId, ObjectId};
use lode_meta::Reads;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

const MIB: usize = 1024 * 1024;

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
    dir: tempfile::TempDir,
}

struct File {
    data: Vec<u8>,
    id: ObjectId,
    list: ChunkList,
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn file(data: Vec<u8>) -> File {
    let mut h = ChunkingHasher::new();
    h.update(&data);
    let r = h.finish();
    File {
        data,
        id: r.id,
        list: r.chunks.expect("large enough to chunk"),
    }
}

fn real_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let mut config = ServerConfig::with_data_dir(dir.path());
    config.gc.grace_seconds = 3600;
    let clock = Arc::new(ManualClock::new(real_now_ms()));
    let state = open_state(config, clock, vec![]).await.unwrap();
    let mut h = H {
        state,
        admin,
        depot: DepotId(0),
        dir,
    };
    let (s, _) = h
        .json("POST", "/api/v1/depots", json!({ "name": "game" }))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.depot = h
        .state
        .meta()
        .read(|tx| tx.depot_by_name("game"))
        .await
        .unwrap()
        .unwrap()
        .id;
    h
}

impl H {
    async fn raw(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, bytes)
    }

    fn req(&self, method: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.admin))
    }

    async fn json(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let b = self.req(method, uri);
        let req = if body.is_null() {
            b.body(Body::empty()).unwrap()
        } else {
            b.header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let (s, _, bytes) = self.raw(req).await;
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn put_blob(&self, data: &[u8]) {
        let id = lode_core::blob_id(data).unwrap();
        let req = self
            .req("PUT", &format!("/api/v1/depots/game/blobs/{id}"))
            .body(Body::from(data.to_vec()))
            .unwrap();
        let (s, _, b) = self.raw(req).await;
        assert!(s.is_success(), "{s} {}", String::from_utf8_lossy(&b));
    }

    async fn put_chunks(&self, f: &File) {
        let mut off = 0;
        for c in &f.list.chunks {
            self.put_blob(&f.data[off..off + c.size as usize]).await;
            off += c.size as usize;
        }
    }

    async fn put_list(&self, id: &ObjectId, list: &ChunkList) -> (StatusCode, Value) {
        let req = self
            .req("PUT", &format!("/api/v1/depots/game/blobs/{id}/chunks"))
            .body(Body::from(list.encode()))
            .unwrap();
        let (s, _, b) = self.raw(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn upload(&self, f: &File) {
        self.put_chunks(f).await;
        let (s, b) = self.put_list(&f.id, &f.list).await;
        assert!(s.is_success(), "{s} {b}");
    }

    async fn download(&self, id: &ObjectId, range: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut b = self.req("GET", &format!("/api/v1/depots/game/blobs/{id}"));
        if let Some(r) = range {
            b = b.header("range", r);
        }
        let (s, _, bytes) = self.raw(b.body(Body::empty()).unwrap()).await;
        (s, bytes)
    }

    async fn commit(&self, rid: &str, changes: Value) -> (StatusCode, Value) {
        self.json(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            json!({ "request_id": rid, "message": rid, "changes": changes }),
        )
        .await
    }

    async fn exists(&self, id: &ObjectId) -> bool {
        self.state.blobs().contains(self.depot, id).await.unwrap()
    }

    fn age_all(&self) {
        let root = self
            .dir
            .path()
            .join("depots")
            .join(self.depot.0.to_string())
            .join("objects");
        for e in walk(&root) {
            let f = std::fs::File::options().write(true).open(&e).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(7200))
                .unwrap();
        }
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_checks_and_download_assembles() {
    let h = harness().await;
    let f = file(noise(1, 20 * MIB + 77));

    // No chunks yet: refused with every missing id.
    let (s, b) = h.put_list(&f.id, &f.list).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "missing_objects");
    assert_eq!(
        b["details"]["ids"].as_array().unwrap().len(),
        f.list.chunks.len()
    );

    h.put_chunks(&f).await;
    // The wrong id for these chunks.
    let wrong = lode_core::blob_id(&b"something else"[..]).unwrap();
    let (s, b) = h.put_list(&wrong, &f.list).await;
    assert_eq!(b["code"], "hash_mismatch", "{s} {b}");
    // Garbage body.
    let req = h
        .req("PUT", &format!("/api/v1/depots/game/blobs/{}/chunks", f.id))
        .body(Body::from(vec![1u8, 2, 3]))
        .unwrap();
    assert_eq!(h.raw(req).await.0, StatusCode::BAD_REQUEST);

    let (s, b) = h.put_list(&f.id, &f.list).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["created"], true);
    let (s, _) = h.put_list(&f.id, &f.list).await;
    assert_eq!(s, StatusCode::OK);

    // The file id counts as present.
    let (_, b) = h
        .json(
            "POST",
            "/api/v1/depots/game/objects/missing",
            json!({ "ids": [f.id] }),
        )
        .await;
    assert_eq!(b["missing"], json!([]));

    let (s, body) = h.download(&f.id, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body == f.data, "whole download differs");

    let start = f.list.chunks[0].size as usize - 3;
    let (s, body) = h
        .download(&f.id, Some(&format!("bytes={start}-{}", start + 9)))
        .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, f.data[start..start + 10]);
    let (s, body) = h.download(&f.id, Some("bytes=-5")).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, f.data[f.data.len() - 5..]);
    let (s, _) = h
        .download(&f.id, Some(&format!("bytes={}-", f.data.len())))
        .await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);

    // The chunk list comes back as stored.
    let req = h
        .req("GET", &format!("/api/v1/depots/game/blobs/{}/chunks", f.id))
        .body(Body::empty())
        .unwrap();
    let (s, _, bytes) = h.raw(req).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ChunkList::decode(&bytes).unwrap(), f.list);

    // A file stored whole says so.
    let small = noise(2, 1000);
    h.put_blob(&small).await;
    let small_id = lode_core::blob_id(&small[..]).unwrap();
    let (s, b) = h
        .json(
            "GET",
            &format!("/api/v1/depots/game/blobs/{small_id}/chunks"),
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b["details"]["reason"], "not_chunked");
    // Range works on whole files too.
    let (s, body) = h.download(&small_id, Some("bytes=10-19")).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, small[10..20]);

    // /info reports the chunking parameters.
    let (_, info) = h.json("GET", "/api/v1/info", Value::Null).await;
    assert_eq!(info["chunking"]["algorithm"], lode_core::CHUNK_ALGORITHM);
    assert_eq!(info["chunking"]["threshold"], lode_core::CHUNK_THRESHOLD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_files_are_not_accepted_as_chunks() {
    let h = harness().await;
    let data = noise(3, 2 * MIB);
    let a = &data[..MIB];
    let b = &data[MIB..];
    h.put_blob(a).await;
    h.put_blob(b).await;
    let list = ChunkList {
        chunks: vec![
            lode_core::ChunkRef {
                id: lode_core::blob_id(a).unwrap(),
                size: MIB as u32,
            },
            lode_core::ChunkRef {
                id: lode_core::blob_id(b).unwrap(),
                size: MIB as u32,
            },
        ],
    };
    let id = lode_core::blob_id(&data[..]).unwrap();
    let (s, b) = h.put_list(&id, &list).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commits_gc_fsck_and_obliterate() {
    let h = harness().await;
    let v1 = file(noise(4, 18 * MIB));
    // v2 shares most chunks with v1.
    let mut d2 = v1.data.clone();
    let mid = d2.len() / 2;
    d2[mid..mid + 4096].copy_from_slice(&noise(5, 4096));
    let v2 = file(d2);
    h.upload(&v1).await;
    let (s, b) = h
        .commit(
            "c1",
            json!([{ "path": "big.bin", "action": "add", "new_id": v1.id, "kind": "file", "size": v1.data.len() }]),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let r1 = b["rev"].as_u64().unwrap();
    // A wrong size is refused.
    h.upload(&v2).await;
    let (s, _) = h
        .commit(
            "bad",
            json!([{ "path": "big.bin", "action": "modify", "base_id": v1.id, "new_id": v2.id, "kind": "file", "size": 5 }]),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = h
        .commit(
            "c2",
            json!([{ "path": "big.bin", "action": "modify", "base_id": v1.id, "new_id": v2.id, "kind": "file", "size": v2.data.len() }]),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let r2 = b["rev"].as_u64().unwrap();
    let old: std::collections::HashSet<_> = v1.list.chunks.iter().map(|c| c.id).collect();
    let fresh = v2
        .list
        .chunks
        .iter()
        .filter(|c| !old.contains(&c.id))
        .count();
    assert!(fresh <= 3, "{fresh} new chunks");

    // The file reads back through `files`.
    let req = h
        .req(
            "GET",
            &format!("/api/v1/depots/game/files?rev={r2}&path=big.bin"),
        )
        .body(Body::empty())
        .unwrap();
    let (s, _, body) = h.raw(req).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body == v2.data);

    // An orphan chunked file: its list and own chunks go once old; shared chunks stay.
    let orphan = file(noise(6, 17 * MIB));
    h.upload(&orphan).await;
    h.age_all();
    let (s, b) = h
        .json("POST", "/api/v1/admin/depots/game/gc", Value::Null)
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(!h.exists(&orphan.id).await);
    for c in &orphan.list.chunks {
        assert!(!h.exists(&c.id).await, "orphan chunk {} kept", c.id);
    }
    for c in v1.list.chunks.iter().chain(&v2.list.chunks) {
        assert!(h.exists(&c.id).await, "live chunk {} removed", c.id);
    }
    assert!(h.exists(&v1.id).await && h.exists(&v2.id).await);

    let (_, b) = h
        .json(
            "POST",
            "/api/v1/admin/depots/game/fsck?full=true",
            Value::Null,
        )
        .await;
    assert!(b["problems"].as_array().is_none_or(|a| a.is_empty()), "{b}");

    // Obliterate v1 (no head holds it): its own chunks go, those shared with v2 stay.
    let (s, b) = h
        .json(
            "POST",
            "/api/v1/admin/depots/game/obliterate",
            json!({ "targets": [{ "path": "big.bin", "rev": r1 }], "reason": "test", "dry_run": false }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(!h.exists(&v1.id).await);
    let v2_chunks: std::collections::HashSet<_> = v2.list.chunks.iter().map(|c| c.id).collect();
    for c in &v1.list.chunks {
        assert_eq!(
            h.exists(&c.id).await,
            v2_chunks.contains(&c.id),
            "chunk {}",
            c.id
        );
    }
    let (s, _) = h.download(&v2.id, None).await;
    assert_eq!(s, StatusCode::OK);
    // v1's own chunk cannot be uploaded again.
    let own = v1
        .list
        .chunks
        .iter()
        .position(|c| !v2_chunks.contains(&c.id))
        .unwrap();
    let off: usize = v1.list.chunks[..own].iter().map(|c| c.size as usize).sum();
    let bytes = &v1.data[off..off + v1.list.chunks[own].size as usize];
    let req = h
        .req(
            "PUT",
            &format!("/api/v1/depots/game/blobs/{}", v1.list.chunks[own].id),
        )
        .body(Body::from(bytes.to_vec()))
        .unwrap();
    assert_eq!(h.raw(req).await.0, StatusCode::GONE);
    let (_, b) = h
        .json("POST", "/api/v1/admin/depots/game/fsck", Value::Null)
        .await;
    assert!(b["problems"].as_array().is_none_or(|a| a.is_empty()), "{b}");

    // A missing chunk of a live file is a problem.
    let victim = v2.list.chunks[0].id;
    h.state.blobs().delete(h.depot, &victim).await.unwrap();
    let (_, b) = h
        .json("POST", "/api/v1/admin/depots/game/fsck", Value::Null)
        .await;
    let problems = b["problems"].to_string();
    assert!(problems.contains(&victim.to_hex()), "{b}");
}
