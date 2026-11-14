//! T019 acceptance: fsck and backup.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::{blob_id, DepotId, ObjectId};
use lode_meta::Reads;
use lode_server::maintenance::{backup, fsck, FsckOptions};
use lode_server::{init, open_state, AppState, ServerConfig, SystemClock};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
    dir: tempfile::TempDir,
}

async fn open(dir: &Path, grace: u64) -> AppState {
    let mut config = ServerConfig::with_data_dir(dir);
    config.gc.grace_seconds = grace;
    open_state(config, Arc::new(SystemClock), vec![])
        .await
        .unwrap()
}

async fn harness(grace: u64) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let state = open(dir.path(), grace).await;
    let mut h = H {
        state,
        admin,
        depot: DepotId(0),
        dir,
    };
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin.clone(),
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.depot = depot_id(&h.state).await;
    h
}

async fn depot_id(state: &AppState) -> DepotId {
    state
        .meta()
        .read(|tx| tx.depot_by_name("game"))
        .await
        .unwrap()
        .unwrap()
        .id
}

/// Incompressible content, so it is stored uncompressed.
fn noise(seed: &str, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    blake3::Hasher::new()
        .update(seed.as_bytes())
        .finalize_xof()
        .fill(&mut out);
    out
}

impl H {
    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
        let body = if body.is_null() {
            Body::empty()
        } else {
            b = b.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let resp = lode_server::router(self.state.clone())
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn blob(&self, content: &[u8]) -> ObjectId {
        let id = blob_id(content).unwrap();
        let req = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert!(resp.status().is_success());
        id
    }

    async fn commit(&self, rid: &str, path: &str, content: &[u8]) -> (StatusCode, Value, ObjectId) {
        let id = self.blob(content).await;
        let (s, body) = self
            .send(
                "POST",
                "/api/v1/depots/game/branches/main/commits",
                &self.admin,
                json!({ "request_id": rid, "message": rid, "changes": [
                    { "path": path, "action": "add", "new_id": id, "kind": "file", "size": content.len() }
                ] }),
            )
            .await;
        (s, body, id)
    }

    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.dir
            .path()
            .join("depots")
            .join(self.depot.0.to_string())
            .join("objects")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex)
    }

    async fn check(&self, full: bool) -> Vec<String> {
        fsck(&self.state, self.depot, FsckOptions { full })
            .await
            .unwrap()
            .problems
    }
}

#[tokio::test]
async fn clean_depot_passes() {
    let h = harness(86400).await;
    h.commit("a", "a/b/c.txt", b"hello").await;
    h.commit("b", "big.bin", &noise("big", 3 * 1024 * 1024))
        .await;
    h.commit("c", "text.txt", "compressible ".repeat(5000).as_bytes())
        .await;
    assert_eq!(h.check(false).await, Vec::<String>::new());
    assert_eq!(h.check(true).await, Vec::<String>::new());
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/admin/depots/game/fsck?full=true",
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(body.get("problems").is_none());
    assert!(body["objects_checked"].as_u64().unwrap() >= 6);
}

#[tokio::test]
async fn a_deleted_object_is_found() {
    let h = harness(86400).await;
    let (_, _, id) = h.commit("a", "dir/gone.txt", b"soon gone").await;
    std::fs::remove_file(h.object_path(&id)).unwrap();
    let problems = h.check(false).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("r2 dir/gone.txt"), "{problems:?}");
    assert!(problems[0].contains(&id.to_string()));
    assert!(problems[0].contains("missing"));
}

#[tokio::test]
async fn a_truncated_object_is_found() {
    let h = harness(86400).await;
    let (_, _, id) = h.commit("a", "noise.bin", &noise("t", 4096)).await;
    let f = std::fs::File::options()
        .write(true)
        .open(h.object_path(&id))
        .unwrap();
    let len = f.metadata().unwrap().len();
    f.set_len(len - 100).unwrap();
    let problems = h.check(false).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("noise.bin") && problems[0].contains("damaged"));
}

#[tokio::test]
async fn a_flipped_byte_is_found_by_full() {
    let h = harness(86400).await;
    let (_, _, id) = h.commit("a", "noise.bin", &noise("f", 4096)).await;
    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .open(h.object_path(&id))
        .unwrap();
    let len = f.metadata().unwrap().len();
    f.seek(SeekFrom::Start(len - 10)).unwrap();
    f.write_all(&[0x5a]).unwrap();
    drop(f);
    // Same length, so only re-hashing notices.
    assert!(h.check(false).await.is_empty());
    let problems = h.check(true).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(&id.to_string()) && problems[0].contains("hashes to"));
}

#[tokio::test]
async fn a_branch_head_at_a_missing_revision_is_found() {
    let h = harness(86400).await;
    h.commit("a", "a.txt", b"a").await;
    h.state
        .meta()
        .with_writer(|c| c.execute("UPDATE branch SET head_rev = 99", lode_meta::vals![]))
        .unwrap();
    let problems = h.check(false).await;
    assert!(
        problems
            .iter()
            .any(|p| p.contains("branch main") && p.contains("r99")),
        "{problems:?}"
    );
    assert!(problems
        .iter()
        .any(|p| p.contains("not below the next revision number")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_during_commits_passes_fsck() {
    let h = Arc::new(harness(86400).await);
    let writer = {
        let h = h.clone();
        tokio::spawn(async move {
            for i in 0..60 {
                let (s, body, _) = h
                    .commit(
                        &format!("c{i}"),
                        &format!("d{}/f{i}.txt", i % 5),
                        format!("file {i}").as_bytes(),
                    )
                    .await;
                assert_eq!(s, StatusCode::CREATED, "{body}");
            }
        })
    };
    let dest_parent = tempfile::tempdir().unwrap();
    let mut backups = Vec::new();
    for n in 0..3 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let dest = dest_parent.path().join(format!("b{n}"));
        let report = backup(&h.state, &dest).await.unwrap();
        assert!(report.objects > 0);
        // With Postgres the backup holds the objects but not the database (pg_dump's job),
        // so it cannot be opened on its own.
        if !report.database_external {
            backups.push(dest);
        }
    }
    writer.await.unwrap();
    for dest in backups {
        let copy = open(&dest, 86400).await;
        let d = depot_id(&copy).await;
        let report = fsck(&copy, d, FsckOptions { full: true }).await.unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
    }
    // A non-empty destination is refused.
    let busy = dest_parent.path().join("b0");
    assert!(backup(&h.state, &busy).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_during_commits_leaves_a_consistent_depot() {
    let h = Arc::new(harness(0).await);
    let writer = {
        let h = h.clone();
        tokio::spawn(async move {
            for i in 0..40 {
                let content = format!("file {i}");
                for attempt in 0.. {
                    let (s, body, _) = h
                        .commit(
                            &format!("c{i}-{attempt}"),
                            &format!("d{}/f{i}.txt", i % 4),
                            content.as_bytes(),
                        )
                        .await;
                    if s == StatusCode::CREATED {
                        break;
                    }
                    assert_eq!(body["code"], "missing_objects", "{body}");
                    // A GC between upload and commit sends the writer back; under a loaded
                    // machine that can happen many times in a row (seen 50 on Windows).
                    assert!(attempt < 1000, "the writer never got a commit through");
                }
            }
        })
    };
    while !writer.is_finished() {
        let (s, body) = h
            .send(
                "POST",
                "/api/v1/admin/depots/game/gc",
                &h.admin,
                Value::Null,
            )
            .await;
        assert!(s.is_success() || s == StatusCode::CONFLICT, "{body}");
        // Leave the writer room between collections; correctness does not depend on it.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    writer.await.unwrap();
    assert_eq!(h.check(true).await, Vec::<String>::new());
}
