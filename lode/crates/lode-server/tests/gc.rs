//! T018 acceptance: garbage collection.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::{blob_id, DepotId, EntryKind, ObjectId};
use lode_meta::{MetaError, Reads};
use lode_server::commit::TreeSource;
use lode_server::module::Module;
use lode_server::{
    init, open_state, AppState, Clock, ManualClock, ServerConfig, SharedClock, SystemClock,
};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
    dir: tempfile::TempDir,
}

/// Keeps one object alive, as a shelf would.
struct Keeper(ObjectId);

impl Module for Keeper {
    fn name(&self) -> &'static str {
        "keeper"
    }
    fn gc_roots(
        &self,
        _tx: &dyn Reads,
        _depot: DepotId,
        out: &mut dyn FnMut(ObjectId),
    ) -> Result<(), MetaError> {
        out(self.0);
        Ok(())
    }
}

async fn harness(grace: u64, clock: SharedClock, modules: Vec<Arc<dyn Module>>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let mut config = ServerConfig::with_data_dir(dir.path());
    config.gc.grace_seconds = grace;
    let state = open_state(config, clock, modules).await.unwrap();
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

fn real_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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

    /// Makes an object look `age` old.
    fn age(&self, id: &ObjectId, age: Duration) {
        let f = std::fs::File::options()
            .write(true)
            .open(self.object_path(id))
            .unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    async fn exists(&self, id: &ObjectId) -> bool {
        self.state.blobs().contains(self.depot, id).await.unwrap()
    }

    async fn commit(
        &self,
        rid: &str,
        path: &str,
        id: &ObjectId,
        size: usize,
    ) -> (StatusCode, Value) {
        self.send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            &self.admin,
            json!({ "request_id": rid, "message": rid, "changes": [
                { "path": path, "action": "add", "new_id": id, "kind": "file", "size": size }
            ] }),
        )
        .await
    }

    async fn gc(&self, dry_run: bool) -> Value {
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/admin/depots/game/gc?dry_run={dry_run}"),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        body
    }

    /// Every revision's tree loads and every file it names is stored.
    async fn verify(&self) {
        let d = self.depot;
        let revs = self
            .state
            .meta()
            .read(move |tx| {
                tx.find_revisions(
                    d,
                    &lode_meta::RevisionFilter {
                        limit: u32::MAX,
                        ..Default::default()
                    },
                )
            })
            .await
            .unwrap();
        for r in revs {
            let mut stack = vec![r.root_tree];
            while let Some(t) = stack.pop() {
                let tree = self
                    .state
                    .trees()
                    .tree(d, &t)
                    .await
                    .unwrap_or_else(|e| panic!("r{}: tree {t}: {e}", r.num));
                for e in tree.entries() {
                    if e.kind == EntryKind::Dir {
                        stack.push(e.id);
                    } else {
                        assert!(
                            self.exists(&e.id).await,
                            "r{}: blob {} missing",
                            r.num,
                            e.id
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn removes_old_orphans_and_keeps_everything_else() {
    let kept_by_module = blob_id(&b"shelved content"[..]).unwrap();
    let modules: Vec<Arc<dyn Module>> = vec![Arc::new(Keeper(kept_by_module))];
    let h = harness(3600, Arc::new(ManualClock::new(real_now_ms())), modules).await;

    let used = h.blob(b"used!").await;
    let (s, _) = h.commit("c", "dir/used.txt", &used, 5).await;
    assert_eq!(s, StatusCode::CREATED);
    let old1 = h.blob(b"old orphan one").await;
    let old2 = h.blob(b"old orphan two").await;
    let young = h.blob(b"young orphan").await;
    assert_eq!(h.blob(b"shelved content").await, kept_by_module);
    let two_hours = Duration::from_secs(7200);
    for id in [&used, &old1, &old2, &kept_by_module] {
        h.age(id, two_hours);
    }

    // A dry run counts without deleting.
    let report = h.gc(true).await;
    assert_eq!(report["objects_affected"], 2);
    assert_eq!(report["dry_run"], true);
    assert!(report["bytes_affected"].as_u64().unwrap() > 0);
    assert!(h.exists(&old1).await && h.exists(&old2).await);

    let report = h.gc(false).await;
    assert_eq!(report["objects_affected"], 2, "{report}");
    let bytes = report["bytes_affected"].as_u64().unwrap();
    assert!(bytes > 0);
    assert!(!h.exists(&old1).await);
    assert!(!h.exists(&old2).await);
    assert!(h.exists(&young).await);
    assert!(h.exists(&used).await);
    assert!(h.exists(&kept_by_module).await);
    h.verify().await;
    // Nothing left to do.
    assert_eq!(h.gc(false).await["objects_affected"], 0);
}

#[tokio::test]
async fn shared_subtrees_are_marked_once_and_old_trees_kept() {
    let h = harness(0, Arc::new(SystemClock), vec![]).await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    h.commit("1", "big/deep/a.txt", &a, 5).await;
    h.commit("2", "other.txt", &b, 5).await;
    // Every revision's trees, including superseded roots, survive a zero grace period.
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(h.gc(false).await["objects_affected"], 0);
    h.verify().await;
}

#[tokio::test]
async fn only_server_admins_collect() {
    let h = harness(3600, Arc::new(SystemClock), vec![]).await;
    let (_, body) = h
        .send(
            "POST",
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "rw", "scopes": ["read", "write"] }),
        )
        .await;
    let rw = body["token"].as_str().unwrap().to_owned();
    let (s, _) = h
        .send("POST", "/api/v1/admin/depots/game/gc", &rw, Value::Null)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_racing_commits_never_loses_a_referenced_object() {
    // Zero grace: every unreferenced object is a candidate the moment it is listed, which
    // is the hardest case for in-flight commits.
    let h = Arc::new(harness(0, Arc::new(SystemClock), vec![]).await);
    let clock = SystemClock;
    let start = clock.now_ms();
    let mut writers = Vec::new();
    for w in 0..4 {
        let h = h.clone();
        writers.push(tokio::spawn(async move {
            for i in 0..15 {
                let content = format!("writer {w} file {i}");
                let path = format!("w{w}/d{}/f{i}.txt", i % 3);
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    let id = h.blob(content.as_bytes()).await;
                    let (s, body) = h
                        .commit(&format!("w{w}-{i}-{attempts}"), &path, &id, content.len())
                        .await;
                    match s {
                        StatusCode::CREATED => break,
                        // GC removed the upload before the commit came: upload again.
                        StatusCode::CONFLICT if body["code"] == "missing_objects" => {}
                        _ => panic!("{s}: {body}"),
                    }
                    assert!(attempts < 50, "commit keeps losing its upload");
                }
            }
        }));
    }
    let gc = {
        let h = h.clone();
        tokio::spawn(async move {
            let mut runs = 0;
            loop {
                let (s, body) = h
                    .send(
                        "POST",
                        "/api/v1/admin/depots/game/gc",
                        &h.admin,
                        Value::Null,
                    )
                    .await;
                assert!(
                    s == StatusCode::OK || s == StatusCode::CONFLICT,
                    "{s}: {body}"
                );
                runs += 1;
                if runs > 200 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
    };
    for w in writers {
        w.await.unwrap();
    }
    gc.abort();
    let _ = gc.await;
    assert!(clock.now_ms() >= start);
    h.verify().await;
    // And the final state is complete.
    let d = h.depot;
    let head = h
        .state
        .meta()
        .read(move |tx| Ok(tx.branch(d, "main")?.unwrap().head_rev))
        .await
        .unwrap();
    assert_eq!(head, 61);
}
