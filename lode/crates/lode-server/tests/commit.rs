//! T014 acceptance: the commit engine.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::{blob_id, DepotId, DepotPath, EntryKind, ObjectId, TreeEntry};
use lode_meta::{ChangeRow, Reads, RevisionRow};
use lode_server::commit::lookup;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

const COMMITS: &str = "/api/v1/depots/game/branches/main/commits";

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
    _dir: tempfile::TempDir,
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
    let mut h = H {
        state,
        admin,
        depot: DepotId(0),
        _dir: dir,
    };
    let admin = h.admin.clone();
    let (s, body) = h
        .send("POST", "/api/v1/depots", &admin, json!({ "name": "game" }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
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
    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("user-agent", "lode-test/1.0")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Uploads `content` and returns its id.
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
        assert!(resp.status().is_success(), "{}", resp.status());
        id
    }

    async fn commit(&self, token: &str, rid: &str, changes: Value) -> (StatusCode, Value) {
        self.send(
            "POST",
            COMMITS,
            token,
            json!({ "request_id": rid, "message": "m", "changes": changes }),
        )
        .await
    }

    async fn ok(&self, rid: &str, changes: Value) -> u64 {
        let (s, body) = self.commit(&self.admin, rid, changes).await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["rev"].as_u64().unwrap()
    }

    async fn revision(&self, rev: u64) -> RevisionRow {
        let d = self.depot;
        self.state
            .meta()
            .read(move |tx| tx.revision(d, rev))
            .await
            .unwrap()
            .unwrap()
    }

    async fn rows(&self, rev: u64) -> Vec<ChangeRow> {
        let d = self.depot;
        self.state
            .meta()
            .read(move |tx| tx.changes(d, rev))
            .await
            .unwrap()
    }

    async fn entry(&self, rev: u64, path: &str) -> Option<TreeEntry> {
        let root = self.revision(rev).await.root_tree;
        lookup(
            self.state.trees(),
            self.depot,
            &root,
            &DepotPath::parse(path).unwrap(),
        )
        .await
        .unwrap()
    }

    async fn head(&self) -> u64 {
        let d = self.depot;
        self.state
            .meta()
            .read(move |tx| tx.branch(d, "main"))
            .await
            .unwrap()
            .unwrap()
            .head_rev
    }

    async fn revision_count(&self) -> usize {
        let d = self.depot;
        let head = self.head().await;
        self.state
            .meta()
            .read(move |tx| {
                let mut n = 0;
                for r in 1..=head + 5 {
                    if tx.revision(d, r)?.is_some() {
                        n += 1;
                    }
                }
                Ok(n)
            })
            .await
            .unwrap()
    }

    async fn user(&self, name: &str) -> String {
        let (s, _) = self
            .send(
                "POST",
                "/api/v1/admin/principals",
                &self.admin,
                json!({ "kind": "user", "name": name }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                json!({ "name": "t", "scopes": ["read", "write"] }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        body["token"].as_str().unwrap().to_owned()
    }

    async fn acl(&self, rule: Value) {
        let (s, body) = self
            .send("POST", "/api/v1/admin/acl", &self.admin, rule)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
    }
}

fn add(path: &str, id: &ObjectId, size: usize) -> Value {
    json!({ "path": path, "action": "add", "new_id": id, "kind": "file", "size": size })
}

fn modify(path: &str, base: &ObjectId, id: &ObjectId, size: usize) -> Value {
    json!({ "path": path, "action": "modify", "base_id": base, "new_id": id, "kind": "file", "size": size })
}

fn delete(path: &str, base: &ObjectId) -> Value {
    json!({ "path": path, "action": "delete", "base_id": base })
}

#[tokio::test]
async fn new_depot_has_main_at_r1_with_empty_tree() {
    let h = harness().await;
    assert_eq!(h.head().await, 1);
    let r1 = h.revision(1).await;
    assert_eq!(r1.parent_num, None);
    assert!(h.entry(1, "anything").await.is_none());
}

#[tokio::test]
async fn add_modify_delete_copy_and_prune() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    let c = h.blob(b"gamma").await;

    let r2 = h
        .ok(
            "r2",
            json!([
                add("src/deep/nested/a.txt", &a, 5),
                add("src/b.txt", &b, 5),
                add("top.txt", &c, 5)
            ]),
        )
        .await;
    assert_eq!(r2, 2);
    assert_eq!(h.entry(r2, "src/deep/nested/a.txt").await.unwrap().id, a);
    assert_eq!(h.entry(r2, "src/deep").await.unwrap().kind, EntryKind::Dir);

    // Modify, plus a copy with history.
    let r3 = h
        .ok(
            "r3",
            json!([
                modify("src/b.txt", &b, &c, 5),
                {
                    "path": "copy/a.txt", "action": "add", "new_id": a, "kind": "file",
                    "size": 5, "copy_from": { "path": "src/deep/nested/a.txt", "rev": 2 }
                }
            ]),
        )
        .await;
    assert_eq!(h.entry(r3, "src/b.txt").await.unwrap().id, c);
    assert_eq!(h.entry(r3, "copy/a.txt").await.unwrap().id, a);
    let rows = h.rows(r3).await;
    let copy = rows.iter().find(|r| r.path == "copy/a.txt").unwrap();
    assert_eq!(
        copy.copy_from_path.as_deref(),
        Some("src/deep/nested/a.txt")
    );
    assert_eq!(copy.copy_from_rev, Some(2));

    // Deleting the only file prunes every directory that becomes empty.
    let r4 = h
        .ok("r4", json!([delete("src/deep/nested/a.txt", &a)]))
        .await;
    assert!(h.entry(r4, "src/deep").await.is_none());
    assert!(h.entry(r4, "src/deep/nested").await.is_none());
    assert_eq!(h.entry(r4, "src/b.txt").await.unwrap().id, c);

    // Deleting everything returns to the empty tree.
    let r5 = h
        .ok(
            "r5",
            json!([
                delete("src/b.txt", &c),
                delete("top.txt", &c),
                delete("copy/a.txt", &a)
            ]),
        )
        .await;
    assert_eq!(
        h.revision(r5).await.root_tree,
        h.revision(1).await.root_tree
    );
    assert_eq!(h.revision(r5).await.parent_num, Some(r4));
}

#[tokio::test]
async fn copy_from_a_missing_source_is_rejected() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let (s, body) = h
        .commit(
            &h.admin,
            "x",
            json!([{
                "path": "b.txt", "action": "add", "new_id": a, "kind": "file", "size": 5,
                "copy_from": { "path": "nope.txt", "rev": 1 }
            }]),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(h.head().await, 1);
}

#[tokio::test]
async fn out_of_date_cases() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    let c = h.blob(b"gamma").await;
    h.ok("r2", json!([add("f.txt", &a, 5), add("g.txt", &a, 5)]))
        .await;
    // Someone else changes f.txt.
    h.ok("r3", json!([modify("f.txt", &a, &b, 5)])).await;

    // Modify with a stale base.
    let (s, body) = h
        .commit(&h.admin, "x1", json!([modify("f.txt", &a, &c, 5)]))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "out_of_date");
    assert_eq!(body["details"]["paths"], json!(["f.txt"]));

    // Delete of a changed file.
    let (s, body) = h.commit(&h.admin, "x2", json!([delete("f.txt", &a)])).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["code"], "out_of_date");

    // Add of an existing path.
    let (s, body) = h.commit(&h.admin, "x3", json!([add("g.txt", &c, 5)])).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["details"]["paths"], json!(["g.txt"]));

    // A commit based on r2 that touches only g.txt still succeeds after r3.
    let r = h.ok("x4", json!([modify("g.txt", &a, &c, 5)])).await;
    assert_eq!(r, 4);
    assert_eq!(h.entry(r, "f.txt").await.unwrap().id, b);
}

#[tokio::test]
async fn missing_blobs_and_case_collisions() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let ghost1 = blob_id(&b"never uploaded 1"[..]).unwrap();
    let ghost2 = blob_id(&b"never uploaded 2"[..]).unwrap();
    let (s, body) = h
        .commit(
            &h.admin,
            "m",
            json!([
                add("a.txt", &a, 5),
                add("x.txt", &ghost1, 16),
                add("y.txt", &ghost2, 16)
            ]),
        )
        .await;
    assert_eq!(s.as_u16(), 409, "{body}");
    assert_eq!(body["code"], "missing_objects");
    let mut want = vec![ghost1.to_string(), ghost2.to_string()];
    want.sort();
    assert_eq!(body["details"]["ids"], json!(want));

    // Within one request.
    let (s, body) = h
        .commit(
            &h.admin,
            "c1",
            json!([add("Readme.md", &a, 5), add("README.md", &a, 5)]),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_path");

    // Against the head, including a directory.
    h.ok("r2", json!([add("Docs/a.txt", &a, 5)])).await;
    let (s, body) = h
        .commit(&h.admin, "c2", json!([add("docs/b.txt", &a, 5)]))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_path");
    assert_eq!(h.head().await, 2);
}

#[tokio::test]
async fn same_request_id_returns_the_same_revision() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let changes = json!([add("a.txt", &a, 5)]);
    let (s1, b1) = h.commit(&h.admin, "once", changes.clone()).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, b2) = h.commit(&h.admin, "once", changes).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b1["rev"], b2["rev"]);
    assert_eq!(b2["replayed"], true);
    assert_eq!(h.revision_count().await, 2);
    assert_eq!(h.rows(2).await.len(), 1);
}

#[tokio::test]
async fn permission_denied_writes_nothing() {
    let h = harness().await;
    let ana = h.user("ana").await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "write"
    }))
    .await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "locked/**", "effect": "deny", "level": "write"
    }))
    .await;
    let a = h.blob(b"alpha").await;
    let (s, body) = h
        .commit(
            &ana,
            "p",
            json!([add("free.txt", &a, 5), add("locked/x.txt", &a, 5)]),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");
    assert_eq!(body["details"]["paths"], json!(["locked/x.txt"]));
    assert_eq!(h.head().await, 1);
    assert_eq!(h.revision_count().await, 1);

    // Allowed paths go through, recording ana as author and submitter.
    let (s, body) = h.commit(&ana, "p2", json!([add("free.txt", &a, 5)])).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let r = h.revision(2).await;
    assert_eq!(r.author_id, r.submitter_id);
    let prov: Value = serde_json::from_str(r.provenance.as_deref().unwrap()).unwrap();
    assert_eq!(prov["client"], "lode-test/1.0");
}

#[tokio::test]
async fn read_only_token_cannot_commit() {
    let h = harness().await;
    let (_, body) = h
        .send(
            "POST",
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "ro", "scopes": ["read"] }),
        )
        .await;
    let ro = body["token"].as_str().unwrap().to_owned();
    let a = h.blob(b"alpha").await;
    let (s, body) = h.commit(&ro, "r", json!([add("a.txt", &a, 5)])).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(h.head().await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_concurrent_disjoint_commits_get_consecutive_revisions() {
    let h = Arc::new(harness().await);
    let a = h.blob(b"alpha").await;
    let mut tasks = Vec::new();
    for i in 0..50 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            let (s, body) = h
                .commit(
                    &h.admin,
                    &format!("d{i}"),
                    json!([add(&format!("dir{i}/f.txt"), &a, 5)]),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{body}");
            body["rev"].as_u64().unwrap()
        }));
    }
    let mut revs = Vec::new();
    for t in tasks {
        revs.push(t.await.unwrap());
    }
    revs.sort_unstable();
    assert_eq!(revs, (2..52).collect::<Vec<_>>());
    for i in 0..50 {
        assert!(h.entry(51, &format!("dir{i}/f.txt")).await.is_some());
    }
    // Each revision's parent is the one before it.
    for r in 2..52 {
        assert_eq!(h.revision(r).await.parent_num, Some(r - 1));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_concurrent_commits_to_one_file_exactly_one_wins() {
    let h = Arc::new(harness().await);
    let a = h.blob(b"alpha").await;
    h.ok("base", json!([add("hot.txt", &a, 5)])).await;
    let mut tasks = Vec::new();
    for i in 0..50 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            let content = format!("version {i:02}");
            let id = h.blob(content.as_bytes()).await;
            let (s, body) = h
                .commit(
                    &h.admin,
                    &format!("h{i}"),
                    json!([modify("hot.txt", &a, &id, content.len())]),
                )
                .await;
            (s, body)
        }));
    }
    let mut ok = 0;
    for t in tasks {
        let (s, body) = t.await.unwrap();
        if s == StatusCode::CREATED {
            ok += 1;
        } else {
            assert_eq!(body["code"], "out_of_date", "{body}");
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(h.head().await, 3);
}

#[tokio::test]
async fn change_rows_record_old_ids() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    h.ok("r2", json!([add("x/a.txt", &a, 5), add("y.txt", &a, 5)]))
        .await;
    let r3 = h
        .ok(
            "r3",
            json!([
                modify("x/a.txt", &a, &b, 5),
                delete("y.txt", &a),
                {"path": "run.sh", "action": "add", "new_id": b, "kind": "file", "size": 5, "executable": true}
            ]),
        )
        .await;
    let rows = h.rows(r3).await;
    let by: std::collections::HashMap<&str, &ChangeRow> =
        rows.iter().map(|r| (r.path.as_str(), r)).collect();
    assert_eq!(rows.len(), 3);
    let m = by["x/a.txt"];
    assert_eq!(
        (m.action, m.old_id, m.new_id, m.size),
        ('M', Some(a), Some(b), Some(5))
    );
    let d = by["y.txt"];
    assert_eq!(
        (d.action, d.old_id, d.new_id, d.kind),
        ('D', Some(a), None, None)
    );
    let n = by["run.sh"];
    assert_eq!((n.action, n.old_id, n.flags), ('A', None, 1));
    assert!(h.entry(r3, "run.sh").await.unwrap().executable());
}

#[tokio::test]
async fn invalid_lodeconfig_is_rejected() {
    let h = harness().await;
    let bad = b"[checks\nthis is not toml";
    let id = h.blob(bad).await;
    let (s, body) = h
        .commit(&h.admin, "cfg", json!([add(".lodeconfig", &id, bad.len())]))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(h.head().await, 1);

    let good = b"# empty configuration\n";
    let id = h.blob(good).await;
    h.ok("cfg2", json!([add(".lodeconfig", &id, good.len())]))
        .await;
}

#[tokio::test]
async fn wrong_size_unknown_branch_and_empty_commits() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let (s, _) = h.commit(&h.admin, "w", json!([add("a.txt", &a, 6)])).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.commit(&h.admin, "e", json!([])).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/nope/commits",
            &h.admin,
            json!({ "request_id": "n", "message": "", "changes": [add("a.txt", &a, 5)] }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(h.head().await, 1);
}

#[tokio::test]
async fn branch_names_with_slashes_are_percent_encoded() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let d = h.depot;
    // Branch creation is T016; insert one directly to exercise the route.
    h.state
        .meta()
        .write(move |tx| {
            let num = tx.allocate_rev(d)?;
            let main = tx.branch(d, "main")?.unwrap();
            let head = tx.revision(d, main.head_rev)?.unwrap();
            let id = tx.create_branch(d, "release/1.0", num, Some((main.id, head.num)))?;
            tx.insert_revision(&RevisionRow {
                num,
                branch_id: id,
                parent_num: Some(head.num),
                request_id: None,
                ..head
            })?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/release%2F1.0/commits",
            &h.admin,
            json!({ "request_id": "rel", "message": "", "changes": [add("a.txt", &a, 5)] }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["branch"], "release/1.0");
}
