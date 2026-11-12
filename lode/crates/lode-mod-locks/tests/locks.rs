//! T030 acceptance: locks taken, listed and released over HTTP, and enforced at commit time.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::blob_id;
use lode_meta::{Reads, TokenId};
use lode_mod_locks::Locks;
use lode_server::module::Module;
use lode_server::{init, open_state, AppState, Caller, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

const CONFIG: &[u8] = b"[locks]\nrequired = [\"**/*.psd\"]\n";

struct H {
    state: AppState,
    admin: String,
    ana: String,
    bob: String,
    _dir: tempfile::TempDir,
}

/// `(path, old content or None for an add, new content or None for a delete)`.
type FileChange<'a> = (&'a str, Option<&'a [u8]>, Option<&'a [u8]>);

fn id(content: &[u8]) -> String {
    blob_id(content).unwrap().to_string()
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

    async fn blob(&self, content: &[u8]) -> String {
        let id = id(content);
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

    /// Commits `files` as `token`: `(path, old content or None for an add, new content or
    /// None for a delete)`.
    async fn commit(
        &self,
        token: &str,
        files: &[FileChange<'_>],
        extensions: Value,
    ) -> (StatusCode, Value) {
        let mut changes = Vec::new();
        for (path, old, new) in files {
            let mut c = json!({ "path": path, "kind": "file" });
            match (old, new) {
                (None, Some(n)) => {
                    c["action"] = json!("add");
                    c["new_id"] = json!(self.blob(n).await);
                    c["size"] = json!(n.len());
                }
                (Some(o), Some(n)) => {
                    c["action"] = json!("modify");
                    c["base_id"] = json!(id(o));
                    c["new_id"] = json!(self.blob(n).await);
                    c["size"] = json!(n.len());
                }
                (Some(o), None) => {
                    c["action"] = json!("delete");
                    c["base_id"] = json!(id(o));
                    c.as_object_mut().unwrap().remove("kind");
                }
                (None, None) => unreachable!(),
            }
            changes.push(c);
        }
        let mut body = json!({
            "request_id": format!("r-{}", rand_id()),
            "message": "change",
            "changes": changes,
        });
        if !extensions.is_null() {
            body["extensions"] = extensions;
        }
        self.send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            token,
            body,
        )
        .await
    }

    async fn lock(&self, token: &str, ws: &str, paths: &[(&str, &[u8])]) -> (StatusCode, Value) {
        let paths: Vec<Value> = paths
            .iter()
            .map(|(p, c)| json!({ "path": p, "base_id": id(c) }))
            .collect();
        self.send(
            "POST",
            "/api/v1/depots/game/locks",
            token,
            json!({ "branch": "main", "workspace": ws, "comment": "mine", "paths": paths }),
        )
        .await
    }

    async fn unlock(&self, token: &str, paths: &[&str], force: bool) -> (StatusCode, Value) {
        self.send(
            "DELETE",
            "/api/v1/depots/game/locks",
            token,
            json!({ "branch": "main", "paths": paths, "force": force }),
        )
        .await
    }

    async fn list(&self, query: &str) -> Vec<(String, String)> {
        let (s, body) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/locks{query}"),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        body["locks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                (
                    l["path"].as_str().unwrap().to_owned(),
                    l["owner"]["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    async fn events(&self, kind: &str) -> Vec<Value> {
        let kind = kind.to_owned();
        self.state
            .meta()
            .read(move |tx| tx.outbox_after(0, 10_000))
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect()
    }

    async fn principal(&self, name: &str, kind: &str, scopes: &[&str]) -> String {
        let mut req = json!({ "kind": kind, "name": name });
        if kind == "agent" {
            req["owner"] = json!("ana");
        }
        let (s, body) = self
            .send("POST", "/api/v1/admin/principals", &self.admin, req)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        self.token(name, scopes).await
    }

    async fn token(&self, name: &str, scopes: &[&str]) -> String {
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

/// A server with depot `game`: `.lodeconfig` requiring locks on `**/*.psd`, `a.psd`, `b.psd`,
/// `c.txt` and `dir/d.txt`; users `ana` and `bob` with write access everywhere.
async fn setup(enabled: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let modules: Vec<Arc<dyn Module>> = if enabled {
        vec![Locks::module()]
    } else {
        Vec::new()
    };
    let state = open_state(
        ServerConfig::with_data_dir(dir.path()),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        modules,
    )
    .await
    .unwrap();
    let mut h = H {
        state,
        admin,
        ana: String::new(),
        bob: String::new(),
        _dir: dir,
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
    let scopes: &[&str] = if enabled {
        &["read", "write", "lock"]
    } else {
        &["read", "write"]
    };
    h.ana = h.principal("ana", "user", scopes).await;
    h.bob = h.principal("bob", "user", scopes).await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/admin/acl",
            &h.admin,
            json!({ "depot": "game", "subject": { "kind": "all" }, "branch": "*", "path": "**",
                    "effect": "allow", "level": "write" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let admin = h.admin.clone();
    let (s, body) = h
        .commit(
            &admin,
            &[
                (".lodeconfig", None, Some(CONFIG)),
                ("a.psd", None, Some(b"a1")),
                ("b.psd", None, Some(b"b1")),
                ("c.txt", None, Some(b"c1")),
                ("dir/d.txt", None, Some(b"d1")),
            ],
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    h
}

#[tokio::test]
async fn lock_list_unlock_and_holder_details() {
    let h = setup(true).await;
    let (s, body) = h.lock(&h.ana, "ws-ana", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["locks"][0]["path"], "a.psd");
    assert_eq!(body["locks"][0]["owner"]["name"], "ana");
    assert_eq!(body["locks"][0]["owner"]["kind"], "user");
    assert_eq!(body["locks"][0]["workspace"], "ws-ana");
    assert_eq!(body["locks"][0]["comment"], "mine");
    assert_eq!(body["locks"][0]["branch"], "main");
    assert_eq!(h.list("").await, vec![("a.psd".into(), "ana".into())]);

    // Someone else: refused, with the holder.
    let (s, body) = h.lock(&h.bob, "ws-bob", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "locked");
    assert_eq!(body["details"]["paths"], json!(["a.psd"]));
    assert_eq!(body["details"]["locks"][0]["owner"]["name"], "ana");
    assert_eq!(body["details"]["locks"][0]["workspace"], "ws-ana");

    // The same owner and workspace again: nothing changes.
    let (s, body) = h.lock(&h.ana, "ws-ana", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(h.events("lock.acquired").await.len(), 1);
    // The same owner in another workspace: refused.
    let (s, body) = h.lock(&h.ana, "ws-laptop", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "locked");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("another workspace"));

    // Filters.
    h.lock(&h.bob, "ws-bob", &[("dir/d.txt", b"d1")]).await;
    assert_eq!(h.list("?owner=bob").await.len(), 1);
    assert_eq!(h.list("?prefix=dir/").await[0].0, "dir/d.txt");
    assert_eq!(h.list("?branch=main").await.len(), 2);
    assert!(h.list("?owner=nobody").await.is_empty());
    let (s, _) = h
        .send(
            "GET",
            "/api/v1/depots/game/locks?branch=nope",
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Only the holder releases without force.
    let (s, body) = h.unlock(&h.bob, &["a.psd"], false).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "locked");
    let (s, body) = h.unlock(&h.ana, &["a.psd", "c.txt"], false).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["released"], json!(["a.psd"]));
    assert_eq!(h.list("").await, vec![("dir/d.txt".into(), "bob".into())]);

    // Events carry identifiers and counts, never paths.
    let acquired = h.events("lock.acquired").await;
    assert_eq!(
        acquired[0],
        json!({ "branch": "main", "owner": "ana", "count": 1 })
    );
    let released = h.events("lock.released").await;
    assert_eq!(
        released,
        vec![json!({ "branch": "main", "owner": "ana", "count": 1 })]
    );
}

#[tokio::test]
async fn all_or_nothing() {
    let h = setup(true).await;
    h.lock(&h.ana, "ws-ana", &[("b.psd", b"b1")]).await;
    let (s, body) = h
        .lock(&h.bob, "ws-bob", &[("a.psd", b"a1"), ("b.psd", b"b1")])
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["details"]["paths"], json!(["b.psd"]));
    assert_eq!(h.list("").await, vec![("b.psd".into(), "ana".into())]);

    // A path that is not a file at the head.
    for missing in ["nope.psd", "dir"] {
        let (s, body) = h
            .lock(&h.bob, "ws-bob", &[("a.psd", b"a1"), (missing, b"x")])
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["details"]["paths"], json!([missing]));
    }
    assert_eq!(h.list("").await.len(), 1);

    // Duplicates and empty lists are malformed.
    let (s, _) = h
        .lock(&h.bob, "ws-bob", &[("a.psd", b"a1"), ("a.psd", b"a1")])
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.lock(&h.bob, "ws-bob", &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stale_base_is_out_of_date() {
    let h = setup(true).await;
    let (s, body) = h.lock(&h.ana, "ws-ana", &[("a.psd", b"old")]).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "out_of_date");
    assert_eq!(body["details"]["paths"], json!(["a.psd"]));
    assert!(h.list("").await.is_empty());
}

#[tokio::test]
async fn commits_respect_locks() {
    let h = setup(true).await;
    let (s, body) = h.lock(&h.ana, "ws-ana", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::OK, "{body}");

    // Not the holder.
    let (s, body) = h
        .commit(&h.bob, &[("a.psd", Some(b"a1"), Some(b"a2"))], Value::Null)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "locked");
    assert_eq!(body["details"]["locks"][0]["owner"]["name"], "ana");
    let (s, body) = h
        .commit(&h.bob, &[("a.psd", Some(b"a1"), None)], Value::Null)
        .await;
    assert_eq!(body["code"], "locked", "{s} {body}");

    // Required but not locked; also when the same commit drops the rule, which is judged
    // by the configuration at the head.
    let (s, body) = h
        .commit(&h.bob, &[("b.psd", Some(b"b1"), Some(b"b2"))], Value::Null)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "lock_required");
    assert_eq!(body["details"]["paths"], json!(["b.psd"]));
    let (s, body) = h
        .commit(
            &h.bob,
            &[
                (".lodeconfig", Some(CONFIG), Some(b"")),
                ("b.psd", Some(b"b1"), Some(b"b2")),
            ],
            Value::Null,
        )
        .await;
    assert_eq!(body["code"], "lock_required", "{s} {body}");

    // Adds are exempt; paths outside the rule need nothing.
    let (s, body) = h
        .commit(
            &h.bob,
            &[
                ("new.psd", None, Some(b"n")),
                ("c.txt", Some(b"c1"), Some(b"c2")),
            ],
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");

    // The holder commits and keeps the lock, then commits and releases it.
    let (s, body) = h
        .commit(
            &h.ana,
            &[("a.psd", Some(b"a1"), Some(b"a2"))],
            json!({ "locks.keep": true }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert_eq!(h.list("").await.len(), 1);
    assert!(h.events("lock.released").await.is_empty());
    let (s, body) = h
        .commit(&h.ana, &[("a.psd", Some(b"a2"), Some(b"a3"))], Value::Null)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert!(h.list("").await.is_empty());
    assert_eq!(
        h.events("lock.released").await,
        vec![json!({ "branch": "main", "owner": "ana", "count": 1 })]
    );

    // Now bob can lock it at the new version, and only at that one.
    let (s, _) = h.lock(&h.bob, "ws-bob", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, body) = h.lock(&h.bob, "ws-bob", &[("a.psd", b"a3")]).await;
    assert_eq!(s, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn force_unlock_needs_admin() {
    let h = setup(true).await;
    h.lock(&h.ana, "ws-ana", &[("a.psd", b"a1")]).await;
    let (s, body) = h.unlock(&h.bob, &["a.psd"], true).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");
    assert_eq!(h.list("").await.len(), 1);

    // A server administrator can grant itself the module scope and break the lock.
    let admin = h.token("admin", &["read", "write", "admin", "lock"]).await;
    let (s, body) = h.unlock(&admin, &["a.psd"], true).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["released"], json!(["a.psd"]));
    assert_eq!(
        h.events("lock.released").await,
        vec![json!({ "branch": "main", "owner": "ana", "count": 1 })]
    );
}

#[tokio::test]
async fn scopes_and_the_tool() {
    let h = setup(true).await;
    assert_eq!(h.state.scopes().agent_defaults(), vec!["lock"]);
    let no_lock = h.token("bob", &["read", "write"]).await;
    let (s, body) = h.lock(&no_lock, "ws", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");

    // An agent of ana's: its lock is its own, not ana's.
    let agent = h.principal("bot", "agent", &["read", "lock"]).await;
    let (s, body) = h.lock(&agent, "ws-bot", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["locks"][0]["owner"]["kind"], "agent");
    let (s, body) = h
        .commit(&h.ana, &[("a.psd", Some(b"a1"), Some(b"a2"))], Value::Null)
        .await;
    assert_eq!(body["code"], "locked", "{s} {body}");

    let tool = h.state.tools().get("list_locks").unwrap();
    assert_eq!(tool.scope, "read");
    let principal = h
        .state
        .meta()
        .read(|tx| tx.principal_by_name("bob"))
        .await
        .unwrap()
        .unwrap();
    let caller = Caller {
        principal,
        scopes: vec!["read".into()],
        token_id: TokenId(0),
        groups: Vec::new(),
    };
    let out = (tool.handler)(caller.clone(), json!({ "depot": "game", "owner": "bot" }))
        .await
        .unwrap();
    assert_eq!(out["locks"][0]["path"], "a.psd");
    let err = (tool.handler)(caller, json!({ "branch": "main" }))
        .await
        .unwrap_err();
    assert_eq!(err.body().code, "invalid_request");
}

#[tokio::test]
async fn disabled_module_has_no_endpoints_and_ignores_the_rule() {
    let h = setup(false).await;
    let (s, _) = h.lock(&h.ana, "ws-ana", &[("a.psd", b"a1")]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .send("GET", "/api/v1/depots/game/locks", &h.admin, Value::Null)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(h.state.tools().get("list_locks").is_none());
    let (s, body) = h
        .commit(&h.bob, &[("b.psd", Some(b"b1"), Some(b"b2"))], Value::Null)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
}
