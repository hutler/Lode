//! T011 acceptance: principals, passwords, tokens and scopes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lode_server::{init, open_state, ClientInfo, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

const T0: i64 = 1_800_000_000_000;

struct H {
    router: Router,
    clock: ManualClock,
    admin: String,
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
}

async fn harness() -> H {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let report = init(&data).await.unwrap();
    let clock = ManualClock::new(T0);
    let state = open_state(
        ServerConfig::with_data_dir(&data),
        Arc::new(clock.clone()),
        vec![],
    )
    .await
    .unwrap();
    H {
        router: lode_server::router(state),
        clock,
        admin: report.admin_token,
        _dir: dir,
        data,
    }
}

struct Resp {
    status: StatusCode,
    body: Value,
    text: String,
}

impl H {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
        addr: Option<&str>,
    ) -> Resp {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let mut req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        if let Some(a) = addr {
            let a: SocketAddr = a.parse().unwrap();
            req.extensions_mut().insert(ConnectInfo(ClientInfo(a)));
        }
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Resp { status, body, text }
    }

    async fn get(&self, uri: &str, token: &str) -> Resp {
        self.send("GET", uri, Some(token), None, None).await
    }

    async fn post(&self, uri: &str, token: &str, body: Value) -> Resp {
        self.send("POST", uri, Some(token), Some(body), None).await
    }

    async fn login(&self, name: &str, password: &str, addr: &str) -> Resp {
        self.send(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "name": name, "password": password })),
            Some(addr),
        )
        .await
    }

    async fn add_user(&self, name: &str, password: &str) {
        let r = self
            .post(
                "/api/v1/admin/principals",
                &self.admin,
                json!({ "kind": "user", "name": name, "password": password }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }

    async fn token_with(&self, from: &str, scopes: &[&str]) -> Resp {
        self.post(
            "/api/v1/tokens",
            from,
            json!({ "name": "t", "scopes": scopes }),
        )
        .await
    }
}

const ADDR: &str = "10.0.0.1:5000";

#[tokio::test]
async fn init_token_has_core_scopes() {
    let h = harness().await;
    let r = h.get("/api/v1/auth/whoami", &h.admin).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["principal"]["name"], "admin");
    assert_eq!(r.body["scopes"], json!(["admin", "read", "write"]));
}

#[tokio::test]
async fn login_with_right_and_wrong_password() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;

    let r = h.login("ana", "wrong horse", ADDR).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["code"], "unauthenticated");
    let r = h.login("nobody", "correct horse", ADDR).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let r = h.login("ana", "correct horse", ADDR).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let token = r.body["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("lode_"));
    assert_eq!(r.body["principal"]["name"], "ana");
    let who = h.get("/api/v1/auth/whoami", &token).await;
    assert_eq!(who.body["principal"]["name"], "ana");
    assert_eq!(who.body["principal"]["kind"], "user");
}

#[tokio::test]
async fn lockout_after_repeated_failures_and_recovery() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;
    for _ in 0..5 {
        assert_eq!(
            h.login("ana", "nope nope", ADDR).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Locked: even the right password is refused, from any address.
    let r = h.login("ana", "correct horse", "10.9.9.9:1").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.body["details"]["retry_after_seconds"].as_i64().unwrap() > 0);

    h.clock.advance_ms(15 * 60 * 1000);
    let r = h.login("ana", "correct horse", ADDR).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn lockout_per_address_spans_names() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;
    for i in 0..20 {
        let r = h.login(&format!("guess{i}"), "nope nope", ADDR).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = h.login("ana", "correct horse", ADDR).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.body["details"]["retry_after_seconds"].is_number());
    let r = h.login("ana", "correct horse", "10.0.0.2:5000").await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let h = harness().await;
    let expiry = time::OffsetDateTime::from_unix_timestamp(T0 / 1000 + 60)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let r = h
        .post(
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "short", "scopes": ["read"], "expires_at": expiry }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let t = r.body["token"].as_str().unwrap().to_owned();
    assert_eq!(
        h.get("/api/v1/auth/whoami", &t).await.status,
        StatusCode::OK
    );
    h.clock.advance_ms(61_000);
    let r = h.get("/api/v1/auth/whoami", &t).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["code"], "unauthenticated");

    // An expiry in the past is refused at creation.
    let r = h
        .post(
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "x", "scopes": ["read"], "expires_at": "2000-01-01T00:00:00Z" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deleted_token_is_rejected_and_others_tokens_are_invisible() {
    let h = harness().await;
    let r = h.token_with(&h.admin, &["read"]).await;
    let t = r.body["token"].as_str().unwrap().to_owned();
    let id = r.body["info"]["id"].as_i64().unwrap();

    h.add_user("ana", "correct horse").await;
    let ana = h.login("ana", "correct horse", ADDR).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = h
        .send(
            "DELETE",
            &format!("/api/v1/tokens/{id}"),
            Some(&ana),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        h.get("/api/v1/auth/whoami", &t).await.status,
        StatusCode::OK
    );

    let r = h
        .send(
            "DELETE",
            &format!("/api/v1/tokens/{id}"),
            Some(&h.admin),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        h.get("/api/v1/auth/whoami", &t).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = h
        .send(
            "DELETE",
            "/api/v1/tokens/not-a-number",
            Some(&h.admin),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["code"], "invalid_request");
}

#[tokio::test]
async fn disabled_principal_tokens_are_rejected() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;
    let ana = h.login("ana", "correct horse", ADDR).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = h
        .send(
            "PATCH",
            "/api/v1/admin/principals/ana",
            Some(&h.admin),
            Some(json!({ "disabled": true })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["disabled"], true);
    assert_eq!(
        h.get("/api/v1/auth/whoami", &ana).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Nor can a disabled user sign in.
    assert_eq!(
        h.login("ana", "correct horse", ADDR).await.status,
        StatusCode::UNAUTHORIZED
    );
    h.send(
        "PATCH",
        "/api/v1/admin/principals/ana",
        Some(&h.admin),
        Some(json!({ "disabled": false })),
        None,
    )
    .await;
    assert_eq!(
        h.get("/api/v1/auth/whoami", &ana).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn token_cannot_mint_scopes_it_lacks() {
    let h = harness().await;
    let read = h.token_with(&h.admin, &["read"]).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = h.token_with(&read, &["write"]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "permission_denied");
    let r = h.token_with(&read, &["read", "write"]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = h.token_with(&read, &["read"]).await;
    assert_eq!(r.status, StatusCode::CREATED);

    let r = h.token_with(&h.admin, &["teleport"]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["code"], "invalid_request");
    let r = h.token_with(&h.admin, &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn missing_or_bad_bearer_is_unauthenticated() {
    let h = harness().await;
    let r = h.send("GET", "/api/v1/auth/whoami", None, None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["code"], "unauthenticated");
    for bad in [
        "garbage",
        "lode_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let r = h.get("/api/v1/auth/whoami", bad).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}");
    }
}

#[tokio::test]
async fn last_used_is_written_at_most_once_a_minute() {
    let h = harness().await;
    let last_used = |r: &Resp| {
        r.body["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "init")
            .unwrap()["last_used_at"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let first = last_used(&h.get("/api/v1/tokens", &h.admin).await);
    h.clock.advance_ms(30_000);
    let r = h.get("/api/v1/tokens", &h.admin).await;
    assert_eq!(last_used(&r), first);
    h.clock.advance_ms(31_000);
    h.get("/api/v1/auth/whoami", &h.admin).await;
    let r = h.get("/api/v1/tokens", &h.admin).await;
    assert_ne!(last_used(&r), first);
}

#[tokio::test]
async fn administration_needs_admin_scope_and_standing() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;
    let ana = h.login("ana", "correct horse", ADDR).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    // Ana's login token carries the admin scope but Ana is not an administrator.
    let r = h.get("/api/v1/admin/principals", &ana).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // The admin user with a token lacking the scope is refused too.
    let rw = h.token_with(&h.admin, &["read", "write"]).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = h.get("/api/v1/admin/principals", &rw).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = h.get("/api/v1/admin/principals", &h.admin).await;
    assert_eq!(r.status, StatusCode::OK);
    let names: Vec<_> = r.body["principals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["admin", "ana"]);
}

#[tokio::test]
async fn agents_users_and_groups() {
    let h = harness().await;
    h.add_user("ana", "correct horse").await;
    let p = "/api/v1/admin/principals";

    let r = h
        .post(p, &h.admin, json!({ "kind": "agent", "name": "bot" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "owner required");
    let r = h
        .post(
            p,
            &h.admin,
            json!({ "kind": "agent", "name": "bot", "owner": "ghost" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "owner must exist");
    let r = h
        .post(
            p,
            &h.admin,
            json!({ "kind": "agent", "name": "bot", "owner": "ana", "password": "12345678" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "no agent passwords");
    let r = h
        .post(
            p,
            &h.admin,
            json!({ "kind": "agent", "name": "bot", "owner": "ana" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["owner"], "ana");
    assert_eq!(r.body["kind"], "agent");
    let r = h
        .post(
            p,
            &h.admin,
            json!({ "kind": "agent", "name": "bot2", "owner": "bot" }),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "an agent cannot own an agent"
    );

    let r = h
        .post(p, &h.admin, json!({ "kind": "user", "name": "ana" }))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["code"], "conflict");
    let r = h
        .post(p, &h.admin, json!({ "kind": "user", "name": "bad name" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = h
        .post(
            p,
            &h.admin,
            json!({ "kind": "user", "name": "bo", "password": "short" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // The admin issues a token for the agent; agents cannot log in with a password.
    let r = h
        .post(
            "/api/v1/admin/principals/bot/tokens",
            &h.admin,
            json!({ "name": "ci", "scopes": ["read"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let bot = r.body["token"].as_str().unwrap().to_owned();
    let who = h.get("/api/v1/auth/whoami", &bot).await;
    assert_eq!(who.body["principal"]["kind"], "agent");
    assert_eq!(who.body["principal"]["owner"], "ana");
    assert_eq!(who.body["scopes"], json!(["read"]));

    // Setting a password lets a user without one sign in.
    let r = h
        .send(
            "PATCH",
            "/api/v1/admin/principals/admin",
            Some(&h.admin),
            Some(json!({ "password": "admin password" })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        h.login("admin", "admin password", ADDR).await.status,
        StatusCode::OK
    );

    let g = "/api/v1/admin/groups";
    assert_eq!(
        h.post(g, &h.admin, json!({ "name": "artists" }))
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(
        h.post(g, &h.admin, json!({ "name": "artists" }))
            .await
            .status,
        StatusCode::CONFLICT
    );
    for who in ["ana", "bot"] {
        let r = h
            .send(
                "PUT",
                &format!("{g}/artists/members/{who}"),
                Some(&h.admin),
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    let r = h
        .send(
            "PUT",
            &format!("{g}/nope/members/ana"),
            Some(&h.admin),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h.get(g, &h.admin).await;
    assert_eq!(
        r.body,
        json!({ "groups": [{ "name": "artists", "members": ["ana", "bot"] }] })
    );
    h.send(
        "DELETE",
        &format!("{g}/artists/members/bot"),
        Some(&h.admin),
        None,
        None,
    )
    .await;
    let r = h.get(g, &h.admin).await;
    assert_eq!(r.body["groups"][0]["members"], json!(["ana"]));
}

#[tokio::test]
async fn reset_admin_revokes_and_reissues() {
    let h = harness().await;
    let r = h
        .send(
            "PATCH",
            "/api/v1/admin/principals/admin",
            Some(&h.admin),
            Some(json!({ "disabled": true })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let fresh = lode_server::reset_admin(&ServerConfig::with_data_dir(&h.data))
        .await
        .unwrap();
    assert_eq!(
        h.get("/api/v1/auth/whoami", &h.admin).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = h.get("/api/v1/auth/whoami", &fresh).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["scopes"], json!(["admin", "read", "write"]));
}
