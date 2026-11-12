//! T015 acceptance: revision history, manifests, trees and files.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use lode_core::{blob_id, DepotId, ObjectId};
use lode_meta::{Reads, RevisionRow};
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
    _dir: tempfile::TempDir,
}

async fn harness_with(cfg: impl FnOnce(&mut ServerConfig)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let mut config = ServerConfig::with_data_dir(dir.path());
    cfg(&mut config);
    let state = open_state(
        config,
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

async fn harness() -> H {
    harness_with(|_| {}).await
}

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    chunks: Vec<Bytes>,
}

impl Raw {
    fn body(&self) -> Vec<u8> {
        self.chunks.concat()
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body()).unwrap_or(Value::Null)
    }
    fn lines(&self) -> Vec<Value> {
        let body = self.body();
        std::str::from_utf8(&body)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

impl H {
    async fn raw(&self, req: Request<Body>) -> Raw {
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let mut body = resp.into_body();
        let mut chunks = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.unwrap().into_data() {
                chunks.push(data);
            }
        }
        Raw {
            status,
            headers,
            chunks,
        }
    }

    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let r = self.raw(req).await;
        (r.status, r.json())
    }

    async fn get(&self, uri: &str, token: &str) -> Raw {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        self.raw(req).await
    }

    async fn blob(&self, content: &[u8]) -> ObjectId {
        let id = blob_id(content).unwrap();
        let req = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let r = self.raw(req).await;
        assert!(r.status.is_success(), "{}", r.status);
        id
    }

    async fn commit_on(&self, branch: &str, rid: &str, changes: Value) -> u64 {
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/depots/game/branches/{branch}/commits"),
                &self.admin,
                json!({ "request_id": rid, "message": rid, "changes": changes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["rev"].as_u64().unwrap()
    }

    async fn commit(&self, rid: &str, changes: Value) -> u64 {
        self.commit_on("main", rid, changes).await
    }

    /// Creates `name` from `from` at its head, the way branch creation will (ARCHITECTURE
    /// §5.3): a new revision on the new branch with the source's tree. Returns its number.
    async fn branch(&self, name: &str, from: &str) -> u64 {
        let d = self.depot;
        let (name, from) = (name.to_owned(), from.to_owned());
        self.state
            .meta()
            .write(move |tx| {
                let src = tx.branch(d, &from)?.unwrap();
                let head = tx.revision(d, src.head_rev)?.unwrap();
                let num = tx.allocate_rev(d)?;
                let id = tx.create_branch(d, &name, num, Some((src.id, head.num)))?;
                tx.insert_revision(&RevisionRow {
                    num,
                    branch_id: id,
                    parent_num: Some(head.num),
                    request_id: None,
                    message: "branch".into(),
                    ..head
                })?;
                Ok(num)
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

    async fn acl(&self, rule: Value) -> i64 {
        let (s, body) = self
            .send("POST", "/api/v1/admin/acl", &self.admin, rule)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["id"].as_i64().unwrap()
    }

    /// Full manifest as path → line.
    async fn full(&self, token: &str, rev: u64, view: &str) -> BTreeMap<String, Value> {
        let r = self
            .get(
                &format!("/api/v1/depots/game/manifest?rev={rev}{view}"),
                token,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body());
        r.lines()
            .into_iter()
            .map(|l| (l["path"].as_str().unwrap().to_owned(), l))
            .collect()
    }

    async fn delta(&self, token: &str, base: u64, rev: u64, view: &str) -> Vec<Value> {
        let r = self
            .get(
                &format!("/api/v1/depots/game/manifest?rev={rev}&base={base}{view}"),
                token,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
        r.lines()
    }

    async fn revs(&self, token: &str, query: &str) -> (Vec<u64>, Value) {
        let r = self
            .get(&format!("/api/v1/depots/game/revisions?{query}"), token)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json());
        let body = r.json();
        let nums = body["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rev"].as_u64().unwrap())
            .collect();
        (nums, body)
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

/// Applies a delta to a full manifest.
fn apply(mut m: BTreeMap<String, Value>, delta: &[Value]) -> BTreeMap<String, Value> {
    for line in delta {
        let path = line["path"].as_str().unwrap().to_owned();
        match line["op"].as_str().unwrap() {
            "put" => {
                let mut l = line.clone();
                l.as_object_mut().unwrap().remove("op");
                m.insert(path, l);
            }
            "del" => {
                assert!(m.remove(&path).is_some(), "del of absent {path}");
            }
            other => panic!("unknown op {other}"),
        }
    }
    m
}

#[tokio::test]
async fn full_manifest_of_50000_files_streams_in_chunks() {
    let h = harness().await;
    let a = h.blob(b"same content").await;
    let changes: Vec<Value> = (0..50_000)
        .map(|i| add(&format!("d{:03}/f{i:05}.txt", i / 100), &a, 12))
        .collect();
    let rev = h.commit("big", json!(changes)).await;
    let r = h
        .get(&format!("/api/v1/depots/game/manifest?rev={rev}"), &h.admin)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers["content-type"].to_str().unwrap(),
        "application/x-ndjson"
    );
    // Streamed: many bounded chunks, never the whole body at once.
    assert!(r.chunks.len() > 20, "{} chunks", r.chunks.len());
    let largest = r.chunks.iter().map(Bytes::len).max().unwrap();
    assert!(largest < 128 * 1024, "largest chunk {largest}");
    let lines = r.lines();
    assert_eq!(lines.len(), 50_000);
    assert_eq!(lines[0]["path"], "d000/f00000.txt");
    assert_eq!(lines[49_999]["path"], "d499/f49999.txt");
}

#[tokio::test]
async fn delta_equals_difference_of_full_manifests() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    let c = h.blob(b"gamma").await;
    let r2 = h
        .commit(
            "r2",
            json!([
                add("keep/same.txt", &a, 5),
                add("keep/deep/x.txt", &a, 5),
                add("mod.txt", &a, 5),
                add("gone/one.txt", &a, 5),
                add("gone/two.txt", &b, 5),
                add("swap", &a, 5),
                add("tools/run.sh", &a, 5),
                add("assets/big.bin", &c, 5)
            ]),
        )
        .await;
    h.commit(
        "r3a",
        json!([
            modify("mod.txt", &a, &b, 5),
            delete("gone/one.txt", &a),
            delete("gone/two.txt", &b),
            add("new/added.txt", &c, 5),
            delete("swap", &a),
            {"path": "tools/run.sh", "action": "modify", "base_id": a, "new_id": a,
             "kind": "file", "size": 5, "executable": true}
        ]),
    )
    .await;
    // A file replaced by a directory of the same name (one change set cannot do both).
    let r3 = h.commit("r3", json!([add("swap/inner.txt", &b, 5)])).await;
    let feature = h.branch("feature", "main").await;
    h.commit_on("feature", "f1", json!([delete("swap/inner.txt", &b)]))
        .await;
    let f2 = h
        .commit_on(
            "feature",
            "f2",
            json!([add("swap", &c, 5), add("feature-only.txt", &a, 5)]),
        )
        .await;
    let r_main = h.commit("r-main", json!([delete("mod.txt", &b)])).await;

    for view in ["", "&include=keep/**&include=swap/**", "&exclude=assets/**"] {
        let pairs = [
            (r2, r3),
            (r3, r2),
            (1, r3),
            (r3, 1),
            (r2, f2),
            (f2, r_main),
            (feature, f2),
            (r3, r3),
        ];
        for (m, n) in pairs {
            let delta = h.delta(&h.admin, m, n, view).await;
            let got = apply(h.full(&h.admin, m, view).await, &delta);
            assert_eq!(
                got,
                h.full(&h.admin, n, view).await,
                "{m} -> {n} view {view:?}"
            );
        }
    }
    // Spot checks on content.
    let delta = h.delta(&h.admin, r2, r3, "").await;
    assert!(delta
        .iter()
        .any(|l| l["op"] == "put" && l["path"] == "tools/run.sh" && l["executable"] == true));
    assert!(delta.iter().all(|l| l["path"] != "keep/same.txt"));
    assert!(h.delta(&h.admin, r3, r3, "").await.is_empty());
}

#[tokio::test]
async fn unreadable_paths_never_appear() {
    let h = harness().await;
    let ana = h.user("ana").await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "read"
    }))
    .await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "secret/**", "effect": "deny", "level": "read"
    }))
    .await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    let r2 = h
        .commit(
            "r2",
            json!([add("open.txt", &a, 5), add("secret/plan.txt", &a, 5)]),
        )
        .await;
    let r3 = h
        .commit(
            "r3",
            json!([
                modify("secret/plan.txt", &a, &b, 5),
                add("secret/more.txt", &b, 5)
            ]),
        )
        .await;

    let full = h.full(&ana, r3, "").await;
    assert_eq!(full.keys().collect::<Vec<_>>(), vec!["open.txt"]);
    assert!(h.delta(&ana, r2, r3, "").await.is_empty());
    assert_eq!(h.delta(&ana, 1, r3, "").await.len(), 1);

    let tree = h
        .get(&format!("/api/v1/depots/game/tree?rev={r3}"), &ana)
        .await
        .json();
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["open.txt"]);
    let r = h
        .get(
            &format!("/api/v1/depots/game/tree?rev={r3}&path=secret"),
            &ana,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = h
        .get(
            &format!("/api/v1/depots/game/files?rev={r3}&path=secret/plan.txt"),
            &ana,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["code"], "not_found");
    let r = h
        .get(
            &format!("/api/v1/depots/game/files?rev={r3}&path=secret/none.txt"),
            &ana,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let (nums, body) = h.revs(&ana, "changes=true").await;
    assert_eq!(nums, vec![r3, r2, 1]);
    let text = body.to_string();
    assert!(!text.contains("secret"), "{text}");
    let (nums, _) = h.revs(&ana, "path=secret/plan.txt").await;
    assert!(nums.is_empty());
    let detail = h
        .get(&format!("/api/v1/depots/game/revisions/{r3}"), &ana)
        .await
        .json();
    assert_eq!(detail["changes"], json!([]));

    // The administrator sees everything.
    assert_eq!(h.full(&h.admin, r3, "").await.len(), 3);
}

#[tokio::test]
async fn branch_without_read_access_is_hidden() {
    let h = harness().await;
    let ana = h.user("ana").await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "main", "path": "**", "effect": "allow", "level": "read"
    }))
    .await;
    let a = h.blob(b"alpha").await;
    h.branch("hidden", "main").await;
    let rh = h
        .commit_on("hidden", "h1", json!([add("h.txt", &a, 5)]))
        .await;
    let rm = h.commit("m1", json!([add("m.txt", &a, 5)])).await;
    let (nums, _) = h.revs(&ana, "").await;
    assert_eq!(nums, vec![rm, 1]);
    let r = h
        .get(&format!("/api/v1/depots/game/manifest?rev={rh}"), &ana)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h
        .get("/api/v1/depots/game/revisions?branch=hidden", &ana)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn path_log_follows_a_rename_when_asked() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let b = h.blob(b"beta!").await;
    let c = h.blob(b"gamma").await;
    let r2 = h.commit("r2", json!([add("old.txt", &a, 5)])).await;
    let r3 = h.commit("r3", json!([modify("old.txt", &a, &b, 5)])).await;
    let other = h.commit("x", json!([add("other.txt", &a, 5)])).await;
    let r4 = h
        .commit(
            "r4",
            json!([
                delete("old.txt", &b),
                {"path": "new.txt", "action": "add", "new_id": b, "kind": "file", "size": 5,
                 "copy_from": { "path": "old.txt", "rev": r3 }}
            ]),
        )
        .await;
    let r5 = h.commit("r5", json!([modify("new.txt", &b, &c, 5)])).await;
    assert!(other < r4);

    let (nums, _) = h.revs(&h.admin, "path=new.txt").await;
    assert_eq!(nums, vec![r5, r4]);
    let (nums, _) = h.revs(&h.admin, "path=new.txt&follow=true").await;
    assert_eq!(nums, vec![r5, r4, r3, r2]);
    let (nums, _) = h
        .revs(&h.admin, "path=new.txt&follow=true&branch=main")
        .await;
    assert_eq!(nums, vec![r5, r4, r3, r2]);
    // Paging across the rename.
    let (nums, body) = h.revs(&h.admin, "path=new.txt&follow=true&limit=2").await;
    assert_eq!(nums, vec![r5, r4]);
    assert_eq!(body["more"], true);
    let (nums, body) = h
        .revs(
            &h.admin,
            &format!("path=new.txt&follow=true&limit=2&before={r4}"),
        )
        .await;
    assert_eq!(nums, vec![r3, r2]);
    assert!(body.get("more").is_none());
    let (nums, _) = h
        .revs(&h.admin, &format!("path=new.txt&follow=true&after={r2}"))
        .await;
    assert_eq!(nums, vec![r3, r4, r5]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn polling_with_after_misses_and_repeats_nothing() {
    let h = Arc::new(harness().await);
    let a = h.blob(b"alpha").await;
    h.branch("feature", "main").await;
    let start = 2;
    let writer = {
        let h = h.clone();
        tokio::spawn(async move {
            for i in 0..40 {
                let branch = if i % 2 == 0 { "main" } else { "feature" };
                h.commit_on(
                    branch,
                    &format!("c{i}"),
                    json!([add(&format!("f{i}.txt"), &a, 5)]),
                )
                .await;
            }
        })
    };
    let mut seen: Vec<(u64, String)> = Vec::new();
    let mut last = start - 1;
    loop {
        let done = writer.is_finished();
        let (_, body) = h.revs(&h.admin, &format!("after={last}&limit=3")).await;
        let revs = body["revisions"].as_array().unwrap();
        for r in revs {
            let n = r["rev"].as_u64().unwrap();
            assert!(n > last);
            last = n;
            seen.push((n, r["branch"].as_str().unwrap().to_owned()));
        }
        if revs.is_empty() && done {
            break;
        }
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();
    let nums: Vec<u64> = seen.iter().map(|(n, _)| *n).collect();
    assert_eq!(nums, (start..start + 41).collect::<Vec<_>>());
    assert!(seen.iter().any(|(_, b)| b == "feature"));
    assert!(seen.iter().any(|(_, b)| b == "main"));
}

#[tokio::test]
async fn own_excludes_inherited_revisions() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let r2 = h.commit("r2", json!([add("a.txt", &a, 5)])).await;
    let fb = h.branch("feature", "main").await;
    let r_main = h.commit("m", json!([add("m.txt", &a, 5)])).await;
    let f1 = h
        .commit_on("feature", "f1", json!([add("f.txt", &a, 5)]))
        .await;
    let (nums, body) = h.revs(&h.admin, "branch=feature").await;
    assert_eq!(nums, vec![f1, fb, r2, 1]);
    assert!(!nums.contains(&r_main));
    assert_eq!(body["revisions"][0]["branch"], "feature");
    assert_eq!(body["revisions"][2]["branch"], "main");
    let (nums, _) = h.revs(&h.admin, "branch=feature&own=true").await;
    assert_eq!(nums, vec![f1, fb]);
    let (nums, _) = h.revs(&h.admin, "").await;
    assert_eq!(nums, vec![f1, r_main, fb, r2, 1]);
    let (nums, body) = h.revs(&h.admin, "limit=2").await;
    assert_eq!(nums, vec![f1, r_main]);
    assert_eq!(body["more"], true);
    let (nums, _) = h.revs(&h.admin, &format!("limit=2&before={r_main}")).await;
    assert_eq!(nums, vec![fb, r2]);
}

#[tokio::test]
async fn changes_are_capped_and_filtered() {
    let h = harness_with(|c| c.limits.max_changes_listed = 3).await;
    let ana = h.user("ana").await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "read"
    }))
    .await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "a*", "effect": "deny", "level": "read"
    }))
    .await;
    let x = h.blob(b"x").await;
    let changes: Vec<Value> = ["a1", "a2", "b1", "b2", "b3", "b4"]
        .iter()
        .map(|p| add(p, &x, 1))
        .collect();
    let r = h.commit("r", json!(changes)).await;

    let (_, body) = h.revs(&h.admin, "changes=true&limit=1").await;
    let rev = &body["revisions"][0];
    assert_eq!(rev["rev"], r);
    assert_eq!(rev["changes"].as_array().unwrap().len(), 3);
    assert_eq!(rev["changes_truncated"], true);

    let (_, body) = h.revs(&ana, "changes=true&limit=1").await;
    let paths: Vec<&str> = body["revisions"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["b1", "b2", "b3"]);

    let (_, body) = h.revs(&h.admin, "limit=1").await;
    assert!(body["revisions"][0].get("changes").is_none());

    // The detail view is not capped.
    let detail = h
        .get(&format!("/api/v1/depots/game/revisions/{r}"), &h.admin)
        .await
        .json();
    assert_eq!(detail["changes"].as_array().unwrap().len(), 6);
    assert_eq!(detail["author"]["name"], "admin");
}

#[tokio::test]
async fn access_epoch_header_changes_only_with_acl_or_groups() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let epoch = |r: &Raw| -> u64 {
        r.headers["x-lode-access-epoch"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let r = h.get("/api/v1/depots/game/manifest?rev=1", &h.admin).await;
    let e0 = epoch(&r);
    let rev = h.commit("c", json!([add("a.txt", &a, 5)])).await;
    h.user("ana").await;
    let r = h
        .get(
            &format!("/api/v1/depots/game/manifest?rev={rev}&base=1"),
            &h.admin,
        )
        .await;
    assert_eq!(epoch(&r), e0);

    let id = h
        .acl(json!({
            "depot": "game", "subject": { "kind": "user", "name": "ana" },
            "branch": "*", "path": "**", "effect": "allow", "level": "read"
        }))
        .await;
    let e1 = epoch(&h.get("/api/v1/depots/game/manifest?rev=1", &h.admin).await);
    assert!(e1 > e0);

    let (s, _) = h
        .send(
            "POST",
            "/api/v1/admin/groups",
            &h.admin,
            json!({ "name": "artists" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let e2 = epoch(&h.get("/api/v1/depots/game/manifest?rev=1", &h.admin).await);
    let (s, _) = h
        .send(
            "PUT",
            "/api/v1/admin/groups/artists/members/ana",
            &h.admin,
            json!(null),
        )
        .await;
    assert!(s.is_success(), "{s}");
    let e3 = epoch(&h.get("/api/v1/depots/game/manifest?rev=1", &h.admin).await);
    assert!(e3 > e2);
    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/admin/acl/{id}"),
            &h.admin,
            json!(null),
        )
        .await;
    assert!(s.is_success());
    let e4 = epoch(&h.get("/api/v1/depots/game/manifest?rev=1", &h.admin).await);
    assert!(e4 > e3);
}

#[tokio::test]
async fn tree_and_files() {
    let h = harness().await;
    let content = b"#!/bin/sh\necho hi\n";
    let a = h.blob(content).await;
    let r = h
        .commit(
            "r",
            json!([
                {"path": "bin/run.sh", "action": "add", "new_id": a, "kind": "file",
                 "size": content.len(), "executable": true},
                add("readme.txt", &a, content.len())
            ]),
        )
        .await;
    let tree = h
        .get(&format!("/api/v1/depots/game/tree?rev={r}"), &h.admin)
        .await
        .json();
    assert_eq!(tree["rev"], r);
    assert_eq!(tree["entries"][0]["name"], "bin");
    assert_eq!(tree["entries"][0]["kind"], "dir");
    assert_eq!(tree["entries"][1]["name"], "readme.txt");
    let sub = h
        .get(
            &format!("/api/v1/depots/game/tree?rev={r}&path=bin"),
            &h.admin,
        )
        .await
        .json();
    assert_eq!(sub["path"], "bin");
    assert_eq!(sub["entries"][0]["executable"], true);

    let f = h
        .get(
            &format!("/api/v1/depots/game/files?rev={r}&path=bin/run.sh"),
            &h.admin,
        )
        .await;
    assert_eq!(f.status, StatusCode::OK);
    assert_eq!(f.body(), content);
    assert_eq!(f.headers["x-lode-id"].to_str().unwrap(), a.to_string());
    assert_eq!(f.headers["x-lode-kind"], "file");
    assert_eq!(f.headers["x-lode-executable"], "true");

    let f = h
        .get(
            &format!("/api/v1/depots/game/files?rev={r}&path=bin"),
            &h.admin,
        )
        .await;
    assert_eq!(f.status, StatusCode::NOT_FOUND);
    let f = h
        .get("/api/v1/depots/game/files?rev=99&path=readme.txt", &h.admin)
        .await;
    assert_eq!(f.status, StatusCode::NOT_FOUND);
    let f = h.get("/api/v1/depots/game/manifest", &h.admin).await;
    assert_eq!(f.status, StatusCode::BAD_REQUEST);
    let f = h
        .get("/api/v1/depots/game/revisions?before=3&after=1", &h.admin)
        .await;
    assert_eq!(f.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delta_across_branches_with_different_rules() {
    let h = harness().await;
    let ana = h.user("ana").await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "read"
    }))
    .await;
    h.acl(json!({
        "depot": "game", "subject": { "kind": "user", "name": "ana" },
        "branch": "feature", "path": "secret/**", "effect": "deny", "level": "read"
    }))
    .await;
    let a = h.blob(b"alpha").await;
    let r2 = h
        .commit(
            "r2",
            json!([add("open.txt", &a, 5), add("secret/s.txt", &a, 5)]),
        )
        .await;
    let fb = h.branch("feature", "main").await;
    // Same trees, different visibility: the secret file must be deleted going to feature
    // and put back going to main.
    for (m, n) in [(r2, fb), (fb, r2)] {
        let delta = h.delta(&ana, m, n, "").await;
        let got = apply(h.full(&ana, m, "").await, &delta);
        assert_eq!(got, h.full(&ana, n, "").await, "{m} -> {n}");
    }
    assert_eq!(h.full(&ana, fb, "").await.len(), 1);
    assert_eq!(h.full(&ana, r2, "").await.len(), 2);
}
