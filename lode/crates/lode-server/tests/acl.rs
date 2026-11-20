//! T012 acceptance through the HTTP API: access rule administration and the access epoch.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lode_core::DepotPath;
use lode_server::{init, open_state, AppState, Caller, Level, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

struct H {
    state: AppState,
    admin: String,
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
    state
        .meta()
        .write(|tx| tx.create_depot("game", 0).map(|_| ()))
        .await
        .unwrap();
    H {
        state,
        admin,
        _dir: dir,
    }
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
            .header("authorization", format!("Bearer {token}"));
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
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

    async fn user(&self, name: &str) -> String {
        let (s, _) = self
            .send(
                "POST",
                "/api/v1/admin/principals",
                &self.admin,
                Some(json!({ "kind": "user", "name": name })),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, body) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                Some(json!({ "name": "t", "scopes": ["read", "write", "admin"] })),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        body["token"].as_str().unwrap().to_owned()
    }

    async fn caller(&self, token: &str) -> Caller {
        let req = Request::get("/")
            .header("authorization", format!("Bearer {token}"))
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        <Caller as axum::extract::FromRequestParts<AppState>>::from_request_parts(
            &mut parts,
            &self.state,
        )
        .await
        .unwrap()
    }

    async fn level(&self, token: &str, branch: &str, path: &str) -> Level {
        let caller = self.caller(token).await;
        let depot = self
            .state
            .meta()
            .read(|tx| {
                use lode_meta::Reads;
                tx.depot_by_name("game")
            })
            .await
            .unwrap()
            .unwrap()
            .id;
        self.state
            .permissions()
            .depot(&caller, depot)
            .level(branch, &DepotPath::parse(path).unwrap())
    }
}

#[tokio::test]
async fn rules_are_added_listed_applied_and_removed() {
    let h = harness().await;
    let ana = h.user("ana").await;
    assert_eq!(h.level(&ana, "main", "a.txt").await, Level::None);
    let epoch0 = h.state.permissions().access_epoch();

    let rule = json!({
        "depot": "game",
        "subject": { "kind": "user", "name": "ana" },
        "branch": "*",
        "path": "**",
        "effect": "allow",
        "level": "write"
    });
    let (s, body) = h
        .send("POST", "/api/v1/admin/acl", &h.admin, Some(rule))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let id = body["id"].as_i64().unwrap();
    assert_eq!(h.state.permissions().access_epoch(), epoch0 + 1);
    assert_eq!(h.level(&ana, "main", "a.txt").await, Level::Write);

    let deny = json!({
        "subject": { "kind": "all" },
        "branch": "*",
        "path": "secret/**",
        "effect": "deny",
        "level": "read"
    });
    let (s, _) = h
        .send("POST", "/api/v1/admin/acl", &h.admin, Some(deny))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(h.level(&ana, "main", "secret/k").await, Level::None);

    let (s, body) = h.send("GET", "/api/v1/admin/acl", &h.admin, None).await;
    assert_eq!(s, StatusCode::OK);
    let rules = body["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0]["depot"], "game");
    assert_eq!(
        rules[0]["subject"],
        json!({ "kind": "user", "name": "ana" })
    );
    assert!(rules[1].get("depot").is_none());

    let (s, _) = h
        .send("DELETE", &format!("/api/v1/admin/acl/{id}"), &h.admin, None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(h.level(&ana, "main", "a.txt").await, Level::None);
    assert_eq!(h.state.permissions().access_epoch(), epoch0 + 3);
    let (s, _) = h
        .send("DELETE", &format!("/api/v1/admin/acl/{id}"), &h.admin, None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn group_membership_changes_access_and_epoch() {
    let h = harness().await;
    let ana = h.user("ana").await;
    h.send(
        "POST",
        "/api/v1/admin/groups",
        &h.admin,
        Some(json!({ "name": "artists" })),
    )
    .await;
    let rule = json!({
        "subject": { "kind": "group", "name": "artists" },
        "branch": "*", "path": "Art/**", "effect": "allow", "level": "read"
    });
    h.send("POST", "/api/v1/admin/acl", &h.admin, Some(rule))
        .await;
    assert_eq!(h.level(&ana, "main", "Art/a.psd").await, Level::None);
    let before = h.state.permissions().access_epoch();
    let (s, _) = h
        .send(
            "PUT",
            "/api/v1/admin/groups/artists/members/ana",
            &h.admin,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(h.state.permissions().access_epoch(), before + 1);
    assert_eq!(h.level(&ana, "main", "Art/a.psd").await, Level::Read);
}

#[tokio::test]
async fn invalid_rules_are_refused() {
    let h = harness().await;
    let base = |subject: Value, path: &str| json!({ "subject": subject, "branch": "*", "path": path, "effect": "allow", "level": "read" });
    let cases = [
        (
            base(json!({ "kind": "all" }), "a/[b"),
            StatusCode::BAD_REQUEST,
        ),
        (base(json!({ "kind": "all" }), ""), StatusCode::BAD_REQUEST),
        (
            base(json!({ "kind": "user", "name": "ghost" }), "**"),
            StatusCode::NOT_FOUND,
        ),
        (
            base(json!({ "kind": "group", "name": "ghost" }), "**"),
            StatusCode::NOT_FOUND,
        ),
        (
            base(json!({ "kind": "nobody" }), "**"),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (rule, want) in cases {
        let (s, body) = h
            .send("POST", "/api/v1/admin/acl", &h.admin, Some(rule.clone()))
            .await;
        assert_eq!(s, want, "{rule} -> {body}");
    }
    let mut on_missing_depot = base(json!({ "kind": "all" }), "**");
    on_missing_depot["depot"] = json!("nope");
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/admin/acl",
            &h.admin,
            Some(on_missing_depot),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_server_admins_manage_rules_and_a_global_admin_rule_makes_one() {
    let h = harness().await;
    let ana = h.user("ana").await;
    let (s, _) = h.send("GET", "/api/v1/admin/acl", &ana, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let rule = json!({
        "subject": { "kind": "user", "name": "ana" },
        "branch": "*", "path": "**", "effect": "allow", "level": "admin"
    });
    h.send("POST", "/api/v1/admin/acl", &h.admin, Some(rule))
        .await;
    let (s, _) = h.send("GET", "/api/v1/admin/acl", &ana, None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h.send("GET", "/api/v1/admin/principals", &ana, None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn rules_can_be_listed_for_one_depot_or_subject() {
    let h = harness().await;
    for name in ["art", "docs"] {
        h.state
            .meta()
            .write(move |tx| tx.create_depot(name, 0).map(|_| ()))
            .await
            .unwrap();
    }
    h.user("ana").await;
    let add = |depot: Option<&str>, subject: Value| {
        let mut rule = json!({
            "subject": subject, "branch": "**", "path": "**", "effect": "allow", "level": "read"
        });
        if let Some(d) = depot {
            rule["depot"] = json!(d);
        }
        rule
    };
    for rule in [
        add(Some("game"), json!({ "kind": "user", "name": "ana" })),
        add(None, json!({ "kind": "all" })),
        add(Some("art"), json!({ "kind": "user", "name": "ana" })),
        add(Some("docs"), json!({ "kind": "all" })),
    ] {
        let (s, b) = h
            .send("POST", "/api/v1/admin/acl", &h.admin, Some(rule))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    let depots = |v: &Value| -> Vec<String> {
        v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["depot"].as_str().unwrap_or("*").to_owned())
            .collect()
    };
    let (s, all) = h.send("GET", "/api/v1/admin/acl", &h.admin, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(depots(&all), ["game", "*", "art", "docs"]);
    let (_, art) = h
        .send("GET", "/api/v1/admin/acl?depot=art", &h.admin, None)
        .await;
    assert_eq!(
        depots(&art),
        ["*", "art"],
        "its own and the server-wide, in order"
    );
    let (_, ana) = h
        .send("GET", "/api/v1/admin/acl?subject=user:ana", &h.admin, None)
        .await;
    assert_eq!(depots(&ana), ["game", "art"]);
    let (_, both) = h
        .send(
            "GET",
            "/api/v1/admin/acl?depot=docs&subject=all",
            &h.admin,
            None,
        )
        .await;
    assert_eq!(depots(&both), ["*", "docs"]);
    let (s, _) = h
        .send("GET", "/api/v1/admin/acl?depot=nope", &h.admin, None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .send("GET", "/api/v1/admin/acl?subject=team:x", &h.admin, None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
