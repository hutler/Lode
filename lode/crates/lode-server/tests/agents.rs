//! T040 acceptance: agent identities, their tokens, provenance and the revision filters.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lode_server::module::{Module, ScopeDef};
use lode_server::{init, open_state, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Declares module scopes the way the locks, shelves and checks modules do.
struct Scopes;

impl Module for Scopes {
    fn name(&self) -> &'static str {
        "scopes"
    }

    fn scopes(&self) -> &'static [ScopeDef] {
        &[
            ScopeDef {
                name: "shelve",
                agent_default: true,
            },
            ScopeDef {
                name: "lock",
                agent_default: true,
            },
            ScopeDef {
                name: "checks",
                agent_default: false,
            },
        ]
    }
}

struct H {
    router: Router,
    admin: String,
    _dir: tempfile::TempDir,
}

impl H {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("user-agent", "lode-cli/9.9");
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn ok(&self, method: &str, uri: &str, token: &str, body: Value) -> Value {
        let (s, v) = self.send(method, uri, token, Some(body)).await;
        assert!(s.is_success(), "{method} {uri}: {s} {v}");
        v
    }

    async fn token_for(&self, by: &str, name: &str, scopes: &[&str]) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/v1/admin/principals/{name}/tokens"),
            by,
            Some(json!({ "name": "t", "scopes": scopes })),
        )
        .await
    }

    async fn upload(&self, content: &[u8]) -> String {
        let id = lode_core::blob_id(content).unwrap().to_string();
        let req = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        assert!(resp.status().is_success());
        id
    }

    /// Adds `path` on main as `token`, with `provenance`.
    async fn commit(&self, token: &str, path: &str, provenance: Value) -> (StatusCode, Value) {
        let content = format!("{path}\n");
        let id = self.upload(content.as_bytes()).await;
        self.send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            token,
            Some(json!({
                "request_id": path,
                "message": format!("add {path}"),
                "changes": [{ "path": path, "action": "add", "new_id": id, "kind": "file",
                              "size": content.len() }],
                "provenance": provenance,
            })),
        )
        .await
    }

    async fn revs(&self, query: &str) -> Vec<u64> {
        let (s, v) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/revisions{query}"),
                &self.admin,
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rev"].as_u64().unwrap())
            .collect()
    }
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let modules: Vec<Arc<dyn Module>> = vec![Arc::new(Scopes)];
    let state = open_state(
        ServerConfig::with_data_dir(dir.path()),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        modules,
    )
    .await
    .unwrap();
    let h = H {
        router: lode_server::router(state),
        admin,
        _dir: dir,
    };
    h.ok(
        "POST",
        "/api/v1/depots",
        &h.admin,
        json!({ "name": "game" }),
    )
    .await;
    for name in ["ana", "bob"] {
        h.ok(
            "POST",
            "/api/v1/admin/principals",
            &h.admin,
            json!({ "kind": "user", "name": name }),
        )
        .await;
    }
    h.ok(
        "POST",
        "/api/v1/admin/principals",
        &h.admin,
        json!({ "kind": "agent", "name": "bot", "owner": "ana" }),
    )
    .await;
    h.ok(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "ana" },
                "branch": "*", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    h
}

fn token(v: &Value) -> String {
    v["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn agent_tokens_scopes_and_access() {
    let h = harness().await;
    let (_, v) = h.token_for(&h.admin, "ana", &["read", "write"]).await;
    let ana = token(&v);
    let (_, v) = h.token_for(&h.admin, "bob", &["read", "write"]).await;
    let bob = token(&v);

    // The owner mints the agent's tokens; without scopes they get the computed defaults.
    let (s, v) = h.token_for(&ana, "bot", &[]).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["info"]["scopes"], json!(["read", "lock", "shelve"]));
    let bot = token(&v);
    for scope in ["write", "admin", "checks"] {
        let (s, v) = h.token_for(&ana, "bot", &[scope]).await;
        assert_eq!(
            (s, v["code"].clone()),
            (StatusCode::FORBIDDEN, json!("permission_denied")),
            "{scope}"
        );
    }
    // Nobody else's agent, and users cannot be given tokens this way.
    let (s, _) = h.token_for(&bob, "bot", &[]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h.token_for(&ana, "bob", &["read"]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = h.token_for(&h.admin, "bob", &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    // By default an agent cannot commit.
    let (s, v) = h.commit(&bot, "a.txt", Value::Null).await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::FORBIDDEN, json!("permission_denied"))
    );
    // With `write` from an admin it may, but only where it has access of its own.
    let (s, v) = h.token_for(&h.admin, "bot", &["read", "write"]).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let bot_w = token(&v);
    let (s, _) = h.commit(&bot_w, "a.txt", Value::Null).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the owner's access is not inherited"
    );
    h.ok(
        "POST",
        "/api/v1/admin/acl",
        &h.admin,
        json!({ "depot": "game", "subject": { "kind": "user", "name": "bot" },
                "branch": "*", "path": "**", "effect": "allow", "level": "write" }),
    )
    .await;
    let agent = json!({ "model": "model-x", "session": "s-1", "tool": "fixer", "ticket": "T-9" });
    let (s, v) = h
        .commit(&bot_w, "a.txt", json!({ "agent": agent, "via": "mcp" }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let rev = v["rev"].as_u64().unwrap();
    let (_, view) = h
        .send(
            "GET",
            &format!("/api/v1/depots/game/revisions/{rev}"),
            &ana,
            None,
        )
        .await;
    assert_eq!(
        view["author"],
        json!({ "name": "bot", "kind": "agent", "owner": "ana" })
    );
    assert_eq!(view["submitter"]["kind"], "agent");
    // `via` and `client` are the server's; `agent` is stored verbatim.
    assert_eq!(
        view["provenance"],
        json!({ "client": "lode-cli/9.9", "via": "api", "agent": agent })
    );

    // Provenance must be an object of strings, and small.
    let (s, v) = h
        .commit(&bot_w, "b.txt", json!({ "agent": { "model": 5 } }))
        .await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_request"))
    );
    let (s, v) = h.commit(&bot_w, "b.txt", json!({ "agent": ["x"] })).await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_request"))
    );
    let big = "x".repeat(5000);
    let (s, v) = h
        .commit(&bot_w, "b.txt", json!({ "agent": { "note": big } }))
        .await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_request"))
    );

    // The revision list filters by author and kind.
    let (s, v) = h.commit(&ana, "c.txt", Value::Null).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let ana_rev = v["rev"].as_u64().unwrap();
    assert_eq!(h.revs("?author_kind=agent").await, vec![rev]);
    assert_eq!(h.revs("?author=ana").await, vec![ana_rev]);
    assert_eq!(h.revs("?author=nobody").await, Vec::<u64>::new());
    assert!(!h.revs("?author_kind=user").await.contains(&rev));
    let (s, _) = h
        .send(
            "GET",
            "/api/v1/depots/game/revisions?author_kind=robot",
            &h.admin,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // The owner lists and revokes the agent's tokens.
    let (s, v) = h
        .send("GET", "/api/v1/admin/principals/bot/tokens", &ana, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["tokens"].as_array().unwrap().len(), 2);
    let first = v["tokens"][0]["id"].as_i64().unwrap();
    let (s, _) = h
        .send("GET", "/api/v1/admin/principals/bot/tokens", &bob, None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/admin/principals/bot/tokens/{first}"),
            &ana,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = h.send("GET", "/api/v1/auth/whoami", &bot, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Disabling the owner disables the agent; enabling them again restores it.
    let (s, _) = h.send("GET", "/api/v1/auth/whoami", &bot_w, None).await;
    assert_eq!(s, StatusCode::OK);
    h.ok(
        "PATCH",
        "/api/v1/admin/principals/ana",
        &h.admin,
        json!({ "disabled": true }),
    )
    .await;
    let (s, v) = h.send("GET", "/api/v1/auth/whoami", &bot_w, None).await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::UNAUTHORIZED, json!("unauthenticated"))
    );
    h.ok(
        "PATCH",
        "/api/v1/admin/principals/ana",
        &h.admin,
        json!({ "disabled": false }),
    )
    .await;
    let (s, _) = h.send("GET", "/api/v1/auth/whoami", &bot_w, None).await;
    assert_eq!(s, StatusCode::OK);
}
