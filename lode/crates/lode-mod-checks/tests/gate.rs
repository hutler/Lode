//! T062 acceptance: the submit gate of `.lodeconfig [checks]`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use common::{modules, setup, setup_with, H};
use serde_json::{json, Value};

const RULE: &[u8] = br#"[checks]
branches = ["main", "release/*"]
required = [{ name = "ci/build", reporter = "ci" }, { name = "ci/test", reporter = "ci" }]
"#;

struct G {
    h: H,
    ana: String,
    ci: String,
    mallory: String,
}

async fn gated() -> G {
    let h = setup().await;
    h.commit(&h.admin, "main", &[(".lodeconfig", RULE, None)])
        .await;
    let ana = h.user("ana", &["read", "write", "shelve"], "write").await;
    let ci = h.user("ci", &["read", "checks"], "read").await;
    let mallory = h.user("mallory", &["read", "checks"], "read").await;
    G {
        h,
        ana,
        ci,
        mallory,
    }
}

impl G {
    async fn pass(&self, token: &str, shelf: u64, version: u64, name: &str, state: &str) {
        let (s, v) = self
            .h
            .report(
                token,
                json!({ "kind": "shelf", "id": shelf, "version": version }),
                name,
                state,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    async fn refused(&self, shelf: u64, version: u64) -> Value {
        let (s, v) = self.h.submit(&self.ana, shelf, version).await;
        assert_eq!(s, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["code"], "checks_required", "{v}");
        v["details"]["missing"].clone()
    }
}

fn reasons(missing: &Value) -> Vec<(String, String)> {
    missing
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_owned(),
                m["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_shelf_with_every_required_check_passes() {
    let g = gated().await;
    let shelf = g.h.shelve(&g.ana, "main", &[("b.txt", b"b\n", None)]).await;
    g.pass(&g.ci, shelf, 1, "ci/build", "success").await;
    g.pass(&g.ci, shelf, 1, "ci/test", "success").await;
    // Extra checks do not matter.
    g.pass(&g.mallory, shelf, 1, "lint", "failure").await;
    let (s, v) = g.h.submit(&g.ana, shelf, 1).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["rev"].as_u64().is_some());
}

#[tokio::test]
async fn missing_pending_failed_foreign_and_stale_checks_refuse_a_submit() {
    let g = gated().await;
    let shelf = g.h.shelve(&g.ana, "main", &[("b.txt", b"b\n", None)]).await;

    // Nothing reported.
    let m = g.refused(shelf, 1).await;
    assert_eq!(
        reasons(&m),
        [
            ("ci/build".into(), "absent".into()),
            ("ci/test".into(), "absent".into())
        ]
    );

    // Pending and failure.
    g.pass(&g.ci, shelf, 1, "ci/build", "pending").await;
    g.pass(&g.ci, shelf, 1, "ci/test", "failure").await;
    let m = g.refused(shelf, 1).await;
    assert_eq!(
        reasons(&m),
        [
            ("ci/build".into(), "not_success".into()),
            ("ci/test".into(), "not_success".into())
        ]
    );
    assert_eq!(m[0]["state"], "pending");
    assert_eq!(m[1]["state"], "failure");

    // `success`, but from someone else: holding the scope is not enough.
    g.pass(&g.ci, shelf, 1, "ci/build", "success").await;
    g.pass(&g.mallory, shelf, 1, "ci/test", "success").await;
    let m = g.refused(shelf, 1).await;
    assert_eq!(reasons(&m), [("ci/test".into(), "wrong_reporter".into())]);
    assert_eq!(m[0]["reported_by"], "mallory");

    // Passed on version 1, but the shelf changed: version 2 must pass again.
    g.pass(&g.ci, shelf, 1, "ci/test", "success").await;
    let v2 =
        g.h.reshelve(&g.ana, shelf, "main", &[("b.txt", b"b2\n", None)])
            .await;
    let m = g.refused(shelf, v2).await;
    assert_eq!(m.as_array().unwrap().len(), 2);
    g.pass(&g.ci, shelf, v2, "ci/build", "success").await;
    g.pass(&g.ci, shelf, v2, "ci/test", "success").await;
    let (s, v) = g.h.submit(&g.ana, shelf, v2).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn a_shelf_cannot_remove_the_rule_it_must_meet() {
    let g = gated().await;
    let shelf =
        g.h.shelve(
            &g.ana,
            "main",
            &[(".lodeconfig", b"[checks]\nrequired = []\n", Some(RULE))],
        )
        .await;
    let m = g.refused(shelf, 1).await;
    assert_eq!(m.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn direct_commits_and_other_branches_are_not_gated() {
    let g = gated().await;
    // A direct commit, even one that claims a shelf in its extensions.
    g.h.commit(&g.ana, "main", &[("c.txt", b"c\n", None)]).await;
    let (s, v) =
        g.h.try_commit(
            &g.ana,
            "main",
            &[("d.txt", b"d\n", None)],
            json!({ "shelf_id": "1", "shelf_version": "1", "shelves.shelf_id": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // A branch outside `branches`.
    let (s, v) =
        g.h.send(
            "POST",
            "/api/v1/depots/game/branches",
            &g.h.admin,
            json!({ "name": "dev", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let shelf = g.h.shelve(&g.ana, "dev", &[("e.txt", b"e\n", None)]).await;
    let (s, v) = g.h.submit(&g.ana, shelf, 1).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // A branch inside the pattern is gated.
    let (s, _) =
        g.h.send(
            "POST",
            "/api/v1/depots/game/branches",
            &g.h.admin,
            json!({ "name": "release/1", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let shelf =
        g.h.shelve(&g.ana, "release/1", &[("f.txt", b"f\n", None)])
            .await;
    g.refused(shelf, 1).await;
}

#[tokio::test]
async fn a_required_check_without_a_reporter_cannot_be_committed() {
    let h = setup().await;
    let (s, v) = h
        .try_commit(
            &h.admin,
            "main",
            &[(
                ".lodeconfig",
                b"[checks]\nbranches = [\"main\"]\nrequired = [{ name = \"ci/build\" }]\n",
                None,
            )],
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains(".lodeconfig"),
        "{v}"
    );
}

#[tokio::test]
async fn without_the_checks_module_nothing_is_gated() {
    // Shelves on, checks off: the rule is in .lodeconfig but nobody enforces it.
    let h = setup_with(modules(false, true)).await;
    h.commit(&h.admin, "main", &[(".lodeconfig", RULE, None)])
        .await;
    let ana = h.user("ana", &["read", "write", "shelve"], "write").await;
    let shelf = h.shelve(&ana, "main", &[("b.txt", b"b\n", None)]).await;
    let (s, v) = h.submit(&ana, shelf, 1).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Checks on, shelves off: commits arrive only directly, and are not gated.
    let h = setup_with(modules(true, false)).await;
    h.commit(&h.admin, "main", &[(".lodeconfig", RULE, None)])
        .await;
    h.commit(&h.admin, "main", &[("b.txt", b"b\n", None)]).await;
}
