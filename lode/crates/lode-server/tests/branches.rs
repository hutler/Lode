//! T016 acceptance: branches.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::{blob_id, DepotId, ObjectId};
use lode_meta::Reads;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
    depot: DepotId,
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
    let mut h = H {
        state,
        admin,
        depot: DepotId(0),
        dir,
    };
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin.clone(),
            json!({ "name": "game" }),
        )
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

fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                count_files(&p)
            } else {
                1
            }
        })
        .sum()
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

    async fn commit(
        &self,
        branch: &str,
        rid: &str,
        path: &str,
        id: &ObjectId,
    ) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/v1/depots/game/branches/{branch}/commits"),
            &self.admin,
            json!({ "request_id": rid, "message": rid, "changes": [
                { "path": path, "action": "add", "new_id": id, "kind": "file", "size": 5 }
            ] }),
        )
        .await
    }

    async fn create(&self, token: &str, body: Value) -> (StatusCode, Value) {
        self.send("POST", "/api/v1/depots/game/branches", token, body)
            .await
    }

    async fn events(&self) -> Vec<(String, Value)> {
        self.state
            .meta()
            .read(|tx| tx.outbox_after(0, 1000))
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.kind, serde_json::from_str(&r.payload).unwrap()))
            .collect()
    }

    async fn user(&self, name: &str, scopes: &[&str]) -> String {
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
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        body["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn new_branch_is_one_revision_sharing_the_source_tree() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    let (s, _) = h.commit("main", "r2", "a.txt", &a).await;
    assert_eq!(s, StatusCode::CREATED);
    let objects_before = count_files(&h.dir.path().join("depots"));

    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "feature/x", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "feature/x");
    assert_eq!(body["head_rev"], 3);
    assert_eq!(body["created_rev"], 3);
    assert_eq!(body["parent"], json!({ "branch": "main", "rev": 2 }));
    assert_eq!(body["state"], "active");
    assert_eq!(count_files(&h.dir.path().join("depots")), objects_before);

    let d = h.depot;
    let (r2, r3) = h
        .state
        .meta()
        .read(move |tx| Ok((tx.revision(d, 2)?.unwrap(), tx.revision(d, 3)?.unwrap())))
        .await
        .unwrap();
    assert_eq!(r3.parent_num, Some(2));
    assert_eq!(r3.root_tree, r2.root_tree);

    let (s, body) = h
        .send(
            "GET",
            "/api/v1/depots/game/branches/feature%2Fx",
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["head_rev"], 3);
}

#[tokio::test]
async fn history_includes_source_up_to_the_branch_point() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    h.commit("main", "r2", "a.txt", &a).await;
    h.commit("main", "r3", "b.txt", &a).await;
    // Branch from an older revision.
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "old", "from_branch": "main", "from_rev": 2 }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["head_rev"], 4);
    h.commit("main", "r5", "c.txt", &a).await;
    let (s, _) = h.commit("old", "r6", "d.txt", &a).await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, body) = h
        .send(
            "GET",
            "/api/v1/depots/game/revisions?branch=old",
            &h.admin,
            Value::Null,
        )
        .await;
    let nums: Vec<u64> = body["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_u64().unwrap())
        .collect();
    assert_eq!(nums, vec![6, 4, 2, 1]);
    // The branch's tree is the source's at r2 plus its own change.
    let (_, body) = h
        .send(
            "GET",
            "/api/v1/depots/game/tree?rev=6",
            &h.admin,
            Value::Null,
        )
        .await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["a.txt", "d.txt"]);

    // A revision that is not in the source's history.
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "bad", "from_branch": "main", "from_rev": 6 }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, _) = h
        .create(&h.admin, json!({ "name": "bad", "from_branch": "nope" }))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archive_blocks_commits_and_hides_the_branch() {
    let h = harness().await;
    let a = h.blob(b"alpha").await;
    h.create(&h.admin, json!({ "name": "old", "from_branch": "main" }))
        .await;
    let ana = h.user("ana", &["read", "write", "admin"]).await;
    h.send(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                "branch": "*", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    let (s, _) = h
        .send(
            "PATCH",
            "/api/v1/depots/game/branches/old",
            &ana,
            json!({ "state": "archived" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, body) = h
        .send(
            "PATCH",
            "/api/v1/depots/game/branches/old",
            &h.admin,
            json!({ "state": "archived" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "archived");

    let (s, body) = h.commit("old", "x", "a.txt", &a).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["code"], "branch_archived");

    let names = |b: &Value| -> Vec<String> {
        b["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, body) = h
        .send("GET", "/api/v1/depots/game/branches", &h.admin, Value::Null)
        .await;
    assert_eq!(names(&body), vec!["main"]);
    let (_, body) = h
        .send(
            "GET",
            "/api/v1/depots/game/branches?all=true",
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(names(&body), vec!["main", "old"]);
    // Still readable by name.
    let (s, _) = h
        .send(
            "GET",
            "/api/v1/depots/game/branches/old",
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn events_are_written_with_the_change() {
    let h = harness().await;
    let (s, _) = h
        .create(
            &h.admin,
            json!({ "name": "feature", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.send(
        "PATCH",
        "/api/v1/depots/game/branches/feature",
        &h.admin,
        json!({ "state": "archived" }),
    )
    .await;
    let events = h.events().await;
    let kinds: Vec<&str> = events.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["revision.created", "branch.created", "branch.archived"]
    );
    assert_eq!(
        events[0].1,
        json!({ "rev": 2, "branch": "feature", "author": "admin", "author_kind": "user",
                "submitter": "admin", "origin": "direct" })
    );
    assert_eq!(
        events[1].1,
        json!({ "branch": "feature", "rev": 2, "from_branch": "main", "from_rev": 1 })
    );
    assert_eq!(events[2].1, json!({ "branch": "feature" }));
    // Archiving twice writes nothing more.
    h.send(
        "PATCH",
        "/api/v1/depots/game/branches/feature",
        &h.admin,
        json!({ "state": "archived" }),
    )
    .await;
    assert_eq!(h.events().await.len(), 3);
}

#[tokio::test]
async fn names_and_permissions() {
    let h = harness().await;
    for bad in [
        "",
        "/lead",
        "trail/",
        "a//b",
        "sp ace",
        "ümlaut",
        "a/../b",
        "colon:x",
        &"x".repeat(201),
    ] {
        let (s, body) = h
            .create(&h.admin, json!({ "name": bad, "from_branch": "main" }))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad:?}: {body}");
    }
    let (s, _) = h
        .create(
            &h.admin,
            json!({ "name": "Release-1.0_rc/x", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "release-1.0_RC/X", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");

    // Write on the new name is needed, read on the source.
    let ana = h.user("ana", &["read", "write"]).await;
    h.send(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                "branch": "main", "path": "**", "effect": "allow", "level": "read" }),
    )
    .await;
    h.send(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                "branch": "ana/*", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    let (s, _) = h
        .create(&ana, json!({ "name": "other", "from_branch": "main" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, body) = h
        .create(&ana, json!({ "name": "ana/topic", "from_branch": "main" }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    // The source must be readable.
    let (s, _) = h
        .create(
            &ana,
            json!({ "name": "ana/two", "from_branch": "Release-1.0_rc/x" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A read-only token cannot create.
    let ro = h.user("ro", &["read"]).await;
    let (s, _) = h
        .create(&ro, json!({ "name": "x", "from_branch": "main" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Ana sees only branches she can read.
    let (_, body) = h
        .send("GET", "/api/v1/depots/game/branches", &ana, Value::Null)
        .await;
    let names: Vec<&str> = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["ana/topic", "main"]);
}
