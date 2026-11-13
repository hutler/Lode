//! An in-process server with the locks, shelves and checks modules, for the checks tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use lode_core::blob_id;
use lode_server::module::Module;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

/// `(path, new content, old content for a modification)`.
pub type FileEdit<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

pub struct H {
    pub state: AppState,
    pub clock: ManualClock,
    pub admin: String,
    pub dir: tempfile::TempDir,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn modules(checks: bool, shelves: bool) -> Vec<Arc<dyn Module>> {
    let mut m: Vec<Arc<dyn Module>> = vec![lode_mod_locks::Locks::module()];
    if shelves {
        m.push(lode_mod_shelves::Shelves::module());
    }
    if checks {
        m.push(lode_mod_checks::Checks::module());
    }
    m
}

pub async fn open(
    dir: &std::path::Path,
    clock: &ManualClock,
    modules: Vec<Arc<dyn Module>>,
) -> AppState {
    let config = ServerConfig::with_data_dir(dir);
    open_state(config, Arc::new(clock.clone()), modules)
        .await
        .unwrap()
}

/// Depot `game` with one revision (`a.txt`).
pub async fn setup_with(modules: Vec<Arc<dyn Module>>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let clock = ManualClock::new(now_ms());
    let state = open(dir.path(), &clock, modules).await;
    let h = H {
        state,
        clock,
        admin,
        dir,
    };
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.commit(&h.admin, "main", &[("a.txt", b"a\n", None)]).await;
    h
}

pub async fn setup() -> H {
    setup_with(modules(true, true)).await
}

fn next() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

pub async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let body = if body.is_null() {
        Body::empty()
    } else {
        b = b.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let resp = lode_server::router(state.clone())
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

impl H {
    pub async fn send(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        send(&self.state, method, uri, token, body).await
    }

    /// A user with a token carrying `scopes`, allowed `level` on the whole depot.
    pub async fn user(&self, name: &str, scopes: &[&str], level: &str) -> String {
        let (s, v) = self
            .send(
                "POST",
                "/api/v1/admin/principals",
                &self.admin,
                json!({ "kind": "user", "name": name }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let (s, v) = self
            .send(
                "POST",
                "/api/v1/admin/acl",
                &self.admin,
                json!({ "depot": "game", "subject": { "kind": "user", "name": name },
                        "branch": "**", "path": "**", "effect": "allow", "level": level }),
            )
            .await;
        assert!(s.is_success(), "{v}");
        self.token(name, scopes).await
    }

    pub async fn token(&self, name: &str, scopes: &[&str]) -> String {
        let (s, v) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    pub async fn upload(&self, content: &[u8]) -> String {
        let id = blob_id(content).unwrap().to_string();
        let req = axum::http::Request::put(format!("/api/v1/depots/game/blobs/{id}"))
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

    pub async fn changes(&self, files: &[FileEdit<'_>]) -> Vec<Value> {
        let mut out = Vec::new();
        for (path, content, old) in files {
            let id = self.upload(content).await;
            let mut c =
                json!({ "path": path, "new_id": id, "kind": "file", "size": content.len() });
            match old {
                Some(o) => {
                    c["action"] = json!("modify");
                    c["base_id"] = json!(blob_id(*o).unwrap().to_string());
                }
                None => c["action"] = json!("add"),
            }
            out.push(c);
        }
        out
    }

    /// A direct commit; panics unless it succeeds.
    pub async fn commit(&self, token: &str, branch: &str, files: &[FileEdit<'_>]) -> u64 {
        let (s, v) = self.try_commit(token, branch, files, json!({})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["rev"].as_u64().unwrap()
    }

    pub async fn try_commit(
        &self,
        token: &str,
        branch: &str,
        files: &[FileEdit<'_>],
        extensions: Value,
    ) -> (StatusCode, Value) {
        let changes = self.changes(files).await;
        self.send(
            "POST",
            &format!(
                "/api/v1/depots/game/branches/{}/commits",
                branch.replace('/', "%2F")
            ),
            token,
            json!({ "request_id": format!("r{}", next()), "message": "m",
                    "changes": changes, "extensions": extensions }),
        )
        .await
    }

    pub async fn head(&self, branch: &str) -> u64 {
        let (s, v) = self
            .send(
                "GET",
                &format!(
                    "/api/v1/depots/game/branches/{}",
                    branch.replace('/', "%2F")
                ),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["head_rev"].as_u64().unwrap()
    }

    /// A new shelf on `branch` at its head; returns its id.
    pub async fn shelve(&self, token: &str, branch: &str, files: &[FileEdit<'_>]) -> u64 {
        let changes = self.changes(files).await;
        let base = self.head(branch).await;
        let (s, v) = self
            .send(
                "POST",
                "/api/v1/depots/game/shelves",
                token,
                json!({ "branch": branch, "base_rev": base, "description": "d", "changes": changes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_u64().unwrap()
    }

    /// Replaces a shelf's content; returns the new version.
    pub async fn reshelve(
        &self,
        token: &str,
        id: u64,
        branch: &str,
        files: &[FileEdit<'_>],
    ) -> u64 {
        let changes = self.changes(files).await;
        let base = self.head(branch).await;
        let (s, v) = self
            .send(
                "PUT",
                &format!("/api/v1/depots/game/shelves/{id}"),
                token,
                json!({ "base_rev": base, "description": "d", "changes": changes }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["version"].as_u64().unwrap()
    }

    pub async fn submit(&self, token: &str, id: u64, version: u64) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/v1/depots/game/shelves/{id}/submit"),
            token,
            json!({ "expected_version": version }),
        )
        .await
    }

    pub async fn report(
        &self,
        token: &str,
        target: Value,
        name: &str,
        state: &str,
    ) -> (StatusCode, Value) {
        self.send(
            "POST",
            "/api/v1/depots/game/checks",
            token,
            json!({ "target": target, "name": name, "state": state }),
        )
        .await
    }

    pub async fn list(&self, token: &str, query: &str) -> (StatusCode, Value) {
        self.send(
            "GET",
            &format!("/api/v1/depots/game/checks?{query}"),
            token,
            Value::Null,
        )
        .await
    }
}
