//! T200 acceptance: `.lodeconfig [branches] shelf_only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use common::{modules, setup, setup_with, H};
use lode_meta::Reads;
use serde_json::{json, Value};

const RULE: &[u8] = b"[branches]\nshelf_only = [\"main\", \"release/*\"]\n";

async fn shelf_only() -> (H, String) {
    let h = setup().await;
    h.commit(&h.admin, "main", &[(".lodeconfig", RULE, None)])
        .await;
    let ana = h.user("ana", &["read", "write", "shelve"], "write").await;
    (h, ana)
}

fn refused(s: StatusCode, v: &Value) {
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "shelf_required", "{v}");
    assert_eq!(v["details"]["branch"], "main", "{v}");
}

#[tokio::test]
async fn direct_commits_are_refused_and_shelf_submits_pass() {
    let (h, ana) = shelf_only().await;
    let (s, v) = h
        .try_commit(&ana, "main", &[("n1.txt", b"n\n", None)], json!({}))
        .await;
    refused(s, &v);
    // The server admin token is no exception without an override.
    let (s, v) = h
        .try_commit(&h.admin, "main", &[("n1.txt", b"n\n", None)], json!({}))
        .await;
    refused(s, &v);

    let shelf = h.shelve(&ana, "main", &[("n1.txt", b"n\n", None)]).await;
    let (s, v) = h.submit(&ana, shelf, 1).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Other branches are unaffected.
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches",
            &h.admin,
            json!({ "name": "dev", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = h
        .try_commit(&ana, "dev", &[("b.txt", b"b\n", None)], json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn a_shelf_is_judged_by_the_head_rule_not_its_own() {
    let (h, ana) = shelf_only().await;
    // Lifting the rule takes a shelf; once submitted, direct commits work again.
    let (s, v) = h
        .try_commit(
            &ana,
            "main",
            &[(".lodeconfig", b"", Some(RULE))],
            json!({}),
        )
        .await;
    refused(s, &v);
    let shelf = h
        .shelve(&ana, "main", &[(".lodeconfig", b"", Some(RULE))])
        .await;
    let (s, v) = h.submit(&ana, shelf, 1).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .try_commit(&ana, "main", &[("n1.txt", b"n\n", None)], json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn a_depot_admin_can_override_with_a_reason() {
    let (h, ana) = shelf_only().await;
    let boss = h
        .user("boss", &["read", "write", "admin"], "admin")
        .await;
    let why = json!({ "override_shelf_only": "checks server is down" });

    // A writer cannot.
    let (s, v) = h
        .try_commit(&ana, "main", &[("n1.txt", b"n\n", None)], why.clone())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");

    // An empty reason is not a reason.
    let (s, v) = h
        .try_commit(
            &boss,
            "main",
            &[("n1.txt", b"n\n", None)],
            json!({ "override_shelf_only": "  " }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    let (s, v) = h
        .try_commit(&boss, "main", &[("n1.txt", b"n\n", None)], why)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let rev = v["rev"].as_u64().unwrap();
    let (s, view) = h
        .send(
            "GET",
            &format!("/api/v1/depots/game/revisions/{rev}"),
            &ana,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{view}");
    assert_eq!(
        view["provenance"]["shelf_only_override"],
        json!({ "by": "boss", "reason": "checks server is down" })
    );
    let events = h
        .state
        .meta()
        .read(|tx| tx.outbox_after(0, 1000))
        .await
        .unwrap();
    let ev = events
        .iter()
        .rev()
        .find(|e| e.kind == "revision.created")
        .unwrap();
    let data: Value = serde_json::from_str(&ev.payload).unwrap();
    assert_eq!(data["shelf_only_override"], true, "{data}");

    // A client cannot plant the record itself.
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches",
            &h.admin,
            json!({ "name": "dev", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let changes = h.changes(&[("c.txt", b"c\n", None)]).await;
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/dev/commits",
            &ana,
            json!({ "request_id": "planted", "message": "m", "changes": changes,
                    "provenance": { "shelf_only_override": { "by": "boss", "reason": "x" } } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let rev = v["rev"].as_u64().unwrap();
    let (_, view) = h
        .send(
            "GET",
            &format!("/api/v1/depots/game/revisions/{rev}"),
            &ana,
            Value::Null,
        )
        .await;
    assert!(view["provenance"].get("shelf_only_override").is_none(), "{view}");
}

#[tokio::test]
async fn an_agent_cannot_override() {
    let (h, _ana) = shelf_only().await;
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/admin/principals",
            &h.admin,
            json!({ "kind": "agent", "name": "bot", "owner": "ana" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/admin/acl",
            &h.admin,
            json!({ "depot": "game", "subject": { "kind": "agents" },
                    "branch": "**", "path": "**", "effect": "allow", "level": "admin" }),
        )
        .await;
    assert!(s.is_success(), "{v}");
    let bot = h.token("bot", &["read", "write", "admin"]).await;
    let (s, v) = h
        .try_commit(
            &bot,
            "main",
            &[("n1.txt", b"n\n", None)],
            json!({ "override_shelf_only": "I am sure" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
}

#[tokio::test]
async fn without_the_shelves_module_the_refusal_says_so() {
    let h = setup_with(modules(true, false)).await;
    h.commit(&h.admin, "main", &[(".lodeconfig", RULE, None)])
        .await;
    let (s, v) = h
        .try_commit(&h.admin, "main", &[("n1.txt", b"n\n", None)], json!({}))
        .await;
    refused(s, &v);
    assert!(
        v["message"].as_str().unwrap().contains("shelves module is off"),
        "{v}"
    );
}

/// T201: a `.lodeconfig` may only name validation scripts that exist in the new revision.
#[tokio::test]
async fn validate_scripts_must_exist_in_the_revision() {
    let h = setup().await;
    let cfg: &[u8] =
        b"[[validate.script]]\nname = \"json\"\nrun = \"tools/v/json.py\"\npaths = [\"**/*.json\"]\n";
    let (s, v) = h
        .try_commit(&h.admin, "main", &[(".lodeconfig", cfg, None)], json!({}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().contains("tools/v/json.py"), "{v}");
    assert_eq!(v["details"]["paths"], json!([".lodeconfig", "tools/v/json.py"]));
    // A bad setting is refused like any other invalid file.
    let (s, v) = h
        .try_commit(
            &h.admin,
            "main",
            &[(".lodeconfig", b"[validate]\npython = \"2.7\"\n", None)],
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    // With the script in the same commit, it lands.
    let (s, v) = h
        .try_commit(
            &h.admin,
            "main",
            &[
                (".lodeconfig", cfg, None),
                ("tools/v/json.py", b"import sys\n", None),
            ],
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}
