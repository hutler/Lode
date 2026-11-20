//! T160 acceptance: stream types and flow rules.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

impl H {
    /// A commit on `branch` adding `path`, recording `merged` as `merged_from`.
    async fn merge_commit(
        &self,
        token: &str,
        branch: &str,
        path: &str,
        merged: Value,
        extensions: Value,
    ) -> (StatusCode, Value) {
        let id = self.blob(path.as_bytes()).await;
        self.send(
            "POST",
            &format!("/api/v1/depots/game/branches/{branch}/commits"),
            token,
            json!({ "request_id": path, "message": path, "changes": [
                { "path": path, "action": "add", "new_id": id, "kind": "file",
                  "size": path.len() }
            ], "merged_from": merged, "extensions": extensions }),
        )
        .await
    }

    async fn add(&self, branch: &str, path: &str) -> u64 {
        let id = self.blob(path.as_bytes()).await;
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/depots/game/branches/{branch}/commits"),
                &self.admin,
                json!({ "request_id": path, "message": path, "changes": [
                    { "path": path, "action": "add", "new_id": id, "kind": "file",
                      "size": path.len() }
                ] }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["rev"].as_u64().unwrap()
    }

    async fn info(&self, target: &str, source: &str) -> Value {
        let (s, body) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/branches/{target}/merge-info?source={source}"),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        body
    }

    /// The full pending range of `source` into `target`.
    async fn range(&self, target: &str, source: &str) -> Value {
        let i = self.info(target, source).await;
        json!([{ "branch": source, "from_rev": i["base"].as_u64().unwrap() + 1,
                 "to_rev": i["source_head"] }])
    }

    async fn set_stream(&self, branch: &str, stream: &str) -> (StatusCode, Value) {
        self.send(
            "PATCH",
            &format!("/api/v1/depots/game/branches/{branch}"),
            &self.admin,
            json!({ "stream": stream }),
        )
        .await
    }
}

fn refused(s: StatusCode, body: &Value, direction: &str) {
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["details"]["reason"], "stream_flow", "{body}");
    assert_eq!(body["details"]["direction"], direction, "{body}");
}

#[tokio::test]
async fn stream_types_and_their_parent_rules() {
    let h = harness().await;
    let (s, body) = h.set_stream("main", "mainline").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["stream"], "mainline");
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "dev", "from_branch": "main", "stream": "development" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(body["stream"], "development");
    // A mainline cannot have a parent.
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "m2", "from_branch": "main", "stream": "mainline" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["details"]["reason"], "stream_parent");
    // A stream needs a stream as parent.
    h.create(&h.admin, json!({ "name": "plain", "from_branch": "main" }))
        .await;
    let (s, body) = h
        .create(
            &h.admin,
            json!({ "name": "r", "from_branch": "plain", "stream": "release" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, _) = h.set_stream("plain", "release").await;
    assert_eq!(s, StatusCode::OK);
    // A parent cannot drop its type while a child stream depends on it.
    let (s, body) = h.set_stream("main", "none").await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["details"]["reason"], "stream_parent");
    let (s, body) = h.set_stream("dev", "none").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.get("stream").is_none(), "{body}");
    // Listed with the type; events carry it.
    let (_, list) = h
        .send("GET", "/api/v1/depots/game/branches", &h.admin, Value::Null)
        .await;
    let types: Vec<(String, Value)> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["name"].as_str().unwrap().to_owned(), b["stream"].clone()))
        .collect();
    assert!(
        types.contains(&("main".into(), json!("mainline"))),
        "{types:?}"
    );
    assert!(
        types.contains(&("plain".into(), json!("release"))),
        "{types:?}"
    );
    assert!(types.contains(&("dev".into(), Value::Null)), "{types:?}");
    let events = h.events().await;
    assert!(events.iter().any(|(k, d)| k == "branch.created"
        && d["branch"] == "dev"
        && d["stream"] == "development"));
    assert!(events.iter().any(|(k, d)| k == "branch.stream_changed"
        && d["branch"] == "dev"
        && d["stream"].is_null()));
}

#[tokio::test]
async fn merges_follow_the_flow_rules() {
    let h = harness().await;
    h.set_stream("main", "mainline").await;
    for (name, t) in [
        ("dev", "development"),
        ("rel", "release"),
        ("dev2", "development"),
    ] {
        let (s, body) = h
            .create(
                &h.admin,
                json!({ "name": name, "from_branch": "main", "stream": t }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
    }
    h.create(&h.admin, json!({ "name": "plain", "from_branch": "main" }))
        .await;
    h.add("main", "m1.txt").await;
    let m2 = h.add("main", "m2.txt").await;
    h.add("dev", "d1.txt").await;
    h.add("dev", "d2.txt").await;
    h.add("rel", "fix1.txt").await;
    h.add("rel", "fix2.txt").await;

    // Merge down into development: allowed.
    let i = h.info("dev", "main").await;
    assert_eq!(i["flow"]["direction"], "merge_down");
    assert_eq!(i["flow"]["allowed"], true);
    let range = h.range("dev", "main").await;
    let (s, body) = h
        .merge_commit(&h.admin, "dev", "down.txt", range, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // Merge down into release: a pick only.
    let i = h.info("rel", "main").await;
    assert_eq!(i["flow"]["allowed"], false);
    assert_eq!(i["flow"]["pick_allowed"], true);
    let range = h.range("rel", "main").await;
    let (s, body) = h
        .merge_commit(&h.admin, "rel", "rel-down.txt", range, json!({}))
        .await;
    refused(s, &body, "merge_down");
    let pick = json!([{ "branch": "main", "from_rev": m2, "to_rev": m2 }]);
    let (s, body) = h
        .merge_commit(&h.admin, "rel", "rel-pick.txt", pick, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // Merge up from release: allowed.
    let i = h.info("main", "rel").await;
    assert_eq!(i["flow"]["direction"], "merge_up");
    let range = h.range("main", "rel").await;
    let (s, body) = h
        .merge_commit(&h.admin, "main", "up.txt", range, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // Copy up from development: refused while main has revisions dev lacks.
    let i = h.info("main", "dev").await;
    assert_eq!(i["flow"]["direction"], "copy_up");
    assert_eq!(i["flow"]["allowed"], false);
    let behind = i["flow"]["behind"].as_u64().unwrap();
    assert!(behind >= 1, "{i}");
    let range = h.range("main", "dev").await;
    let (s, body) = h
        .merge_commit(&h.admin, "main", "copy1.txt", range.clone(), json!({}))
        .await;
    refused(s, &body, "copy_up");
    assert_eq!(body["details"]["behind"], behind);
    // After merging down, the copy up goes through.
    let down = h.range("dev", "main").await;
    let (s, body) = h
        .merge_commit(&h.admin, "dev", "down2.txt", down, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let i = h.info("main", "dev").await;
    assert_eq!(i["flow"]["allowed"], true, "{i}");
    assert_eq!(i["flow"]["behind"], 0);
    let range = h.range("main", "dev").await;
    let (s, body) = h
        .merge_commit(&h.admin, "main", "copy2.txt", range, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // Sideways: refused, picks too.
    let i = h.info("dev2", "dev").await;
    assert_eq!(i["flow"]["direction"], "sideways");
    assert_eq!(i["flow"]["pick_allowed"], false);
    let range = h.range("dev2", "dev").await;
    let (s, body) = h
        .merge_commit(&h.admin, "dev2", "side.txt", range.clone(), json!({}))
        .await;
    refused(s, &body, "sideways");

    // An ordinary branch on either side keeps the v1 rules.
    let i = h.info("plain", "dev").await;
    assert_eq!(i["flow"]["direction"], "untyped");
    let r = h.range("plain", "dev").await;
    let (s, body) = h
        .merge_commit(&h.admin, "plain", "plain.txt", r, json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // force_flow: refused for a writer, recorded for an admin.
    let ana = h.user("ana", &["read", "write"]).await;
    h.send(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                "branch": "**", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    let force = json!({ "force_flow": true });
    let (s, body) = h
        .merge_commit(&ana, "dev2", "side-ana.txt", range.clone(), force.clone())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    let (s, body) = h
        .merge_commit(&ana, "dev2", "side-ana2.txt", range.clone(), json!({}))
        .await;
    refused(s, &body, "sideways");
    let (s, body) = h
        .merge_commit(&h.admin, "dev2", "side-forced.txt", range, force)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let rev = body["rev"].as_u64().unwrap();
    let events = h.events().await;
    let ev = events
        .iter()
        .find(|(k, d)| k == "revision.created" && d["rev"] == rev)
        .unwrap();
    assert_eq!(ev.1["forced_flow"], true, "{ev:?}");
    let plain_rev = events
        .iter()
        .find(|(k, d)| {
            k == "revision.created" && d["branch"] == "plain" && d["rev"].as_u64() > Some(m2)
        })
        .unwrap();
    assert!(plain_rev.1.get("forced_flow").is_none());
}
