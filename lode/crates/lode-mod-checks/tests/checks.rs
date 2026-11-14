//! T061 acceptance: checks reported, replaced and listed for revisions and shelf versions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use common::{modules, open, setup, setup_with, H};
use lode_meta::Reads;
use serde_json::{json, Value};

fn names(v: &Value) -> Vec<(String, String, String)> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["state"].as_str().unwrap().to_owned(),
                c["reporter"]["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

async fn events(h: &H) -> Vec<Value> {
    h.state
        .meta()
        .read(|tx| {
            let rows = tx.db().query(
                "SELECT payload FROM outbox WHERE kind = 'check.updated' ORDER BY id",
                lode_meta::vals![],
                |r| r.get::<String>(0),
            )?;
            Ok(rows
                .iter()
                .map(|r| serde_json::from_str(r).unwrap())
                .collect())
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn report_update_and_list_for_revisions_and_shelf_versions() {
    let h = setup().await;
    let ci = h.user("ci", &["read", "checks"], "read").await;
    let ana = h.user("ana", &["read", "write", "shelve"], "write").await;
    let rev = h.head("main").await;

    // A revision.
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/checks",
            &ci,
            json!({ "target": { "kind": "revision", "id": rev }, "name": "ci/build",
                    "state": "pending", "summary": "queued", "url": "https://ci.example/b/1" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["target"], json!({ "kind": "revision", "id": rev }));
    assert_eq!(v["reporter"]["name"], "ci");
    h.clock.advance_ms(1000);
    let (s, v) = h
        .report(
            &ci,
            json!({ "kind": "revision", "id": rev }),
            "ci/build",
            "success",
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Replaced: no summary or url any more.
    assert!(v.get("summary").is_none() && v.get("url").is_none(), "{v}");
    h.clock.advance_ms(1000);
    h.report(
        &ci,
        json!({ "kind": "revision", "id": rev }),
        "ci/lint",
        "failure",
    )
    .await;
    let (s, list) = h
        .list(&ana, &format!("target_kind=revision&target_id={rev}"))
        .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    // Newest first, with reporter name and kind.
    assert_eq!(
        names(&list),
        [
            ("ci/lint".into(), "failure".into(), "ci".into()),
            ("ci/build".into(), "success".into(), "ci".into())
        ]
    );
    assert_eq!(list["checks"][0]["reporter"]["kind"], "user");

    // Two versions of one shelf keep their own results.
    let shelf = h.shelve(&ana, "main", &[("b.txt", b"b1\n", None)]).await;
    let v2 = h
        .reshelve(&ana, shelf, "main", &[("b.txt", b"b2\n", None)])
        .await;
    assert_eq!(v2, 2);
    for (version, state) in [(1, "failure"), (2, "success")] {
        let (s, v) = h
            .report(
                &ci,
                json!({ "kind": "shelf", "id": shelf, "version": version }),
                "ci/build",
                state,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    for (version, state) in [(1, "failure"), (2, "success")] {
        let (_, list) = h
            .list(
                &ana,
                &format!("target_kind=shelf&target_id={shelf}&target_version={version}"),
            )
            .await;
        assert_eq!(
            names(&list),
            [("ci/build".into(), state.into(), "ci".into())],
            "v{version}"
        );
        assert_eq!(list["checks"][0]["target"]["version"], version);
    }
    // Shelf targets are not validated against the shelves module.
    let (s, _) = h
        .report(
            &ci,
            json!({ "kind": "shelf", "id": 999, "version": 1 }),
            "ci/build",
            "pending",
        )
        .await;
    assert_eq!(s, StatusCode::OK);

    // One event per report.
    let ev = events(&h).await;
    assert_eq!(ev.len(), 6);
    assert_eq!(
        ev[0],
        json!({ "target_kind": "revision", "target_id": rev, "target_version": 0,
                "name": "ci/build", "state": "pending", "reporter": "ci" })
    );
    assert_eq!(ev[4]["target_version"], 2);
}

#[tokio::test]
async fn reports_are_checked() {
    let h = setup().await;
    let ci = h.user("ci", &["read", "checks"], "read").await;
    let ana = h.user("ana", &["read", "write"], "write").await;
    let rev = h.head("main").await;
    let target = json!({ "kind": "revision", "id": rev });

    // Without the scope.
    let (s, v) = h.report(&ana, target.clone(), "ci/build", "success").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["code"], "permission_denied");

    let bad: Vec<Value> = vec![
        json!({ "target": target, "name": "CI/Build", "state": "success" }),
        json!({ "target": target, "name": "", "state": "success" }),
        json!({ "target": target, "name": "x".repeat(101), "state": "success" }),
        json!({ "target": target, "name": "ci build", "state": "success" }),
        json!({ "target": target, "name": "ci/build", "state": "passed" }),
        json!({ "target": target, "name": "ci/build", "state": "success", "summary": "s".repeat(1025) }),
        json!({ "target": target, "name": "ci/build", "state": "success", "url": "ftp://x/y" }),
        json!({ "target": target, "name": "ci/build", "state": "success", "url": "javascript:alert(1)" }),
        json!({ "target": target, "name": "ci/build", "state": "success",
                "url": format!("https://x/{}", "a".repeat(2048)) }),
        json!({ "target": { "kind": "shelf", "id": 1 }, "name": "ci/build", "state": "success" }),
        json!({ "target": { "kind": "shelf", "id": 1, "version": 0 }, "name": "ci/build", "state": "success" }),
        json!({ "target": { "kind": "revision", "id": rev, "version": 2 }, "name": "ci/build", "state": "success" }),
        json!({ "target": { "kind": "commit", "id": rev }, "name": "ci/build", "state": "success" }),
    ];
    for b in bad {
        let (s, v) = h
            .send("POST", "/api/v1/depots/game/checks", &ci, b.clone())
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}: {v}");
        assert_eq!(v["code"], "invalid_request", "{b}");
    }
    // A revision that does not exist.
    let (s, v) = h
        .report(
            &ci,
            json!({ "kind": "revision", "id": rev + 10 }),
            "ci/build",
            "success",
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    // Longest valid values pass.
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/checks",
            &ci,
            json!({ "target": target, "name": "a.b_c/d-0".repeat(10) ,
                    "state": "cancelled", "summary": "s".repeat(1024),
                    "url": format!("http://x/{}", "a".repeat(2038)) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(events(&h).await.len() == 1);
}

#[tokio::test]
async fn a_checks_token_can_do_nothing_else() {
    let h = setup().await;
    // Write access on the depot, but the token carries only `read` and `checks`.
    let ci = h.user("ci", &["read", "checks"], "admin").await;
    let (s, _) = h
        .try_commit(&ci, "main", &[("c.txt", b"c\n", None)], json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let changes = h.changes(&[("c.txt", b"c\n", None)]).await;
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots/game/shelves",
            &ci,
            json!({ "branch": "main", "base_rev": h.head("main").await, "description": "d",
                    "changes": changes }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots/game/locks",
            &ci,
            json!({ "branch": "main", "workspace": "w", "paths": [] }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .send("POST", "/api/v1/depots", &ci, json!({ "name": "other" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // And reading works.
    let (s, _) = h
        .send("GET", "/api/v1/depots/game/revisions", &ci, Value::Null)
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_disabled_module_has_no_endpoints_and_the_scope_does_no_harm() {
    let h = setup().await;
    let ci = h.user("ci", &["read", "checks"], "read").await;
    let rev = h.head("main").await;
    let H {
        state,
        clock,
        dir,
        admin,
    } = h;
    drop(state);
    let state = open(dir.path(), &clock, modules(false, true)).await;
    let h = H {
        state,
        clock,
        admin,
        dir,
    };
    let (s, _) = h
        .report(
            &ci,
            json!({ "kind": "revision", "id": rev }),
            "ci/build",
            "success",
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .list(&ci, &format!("target_kind=revision&target_id={rev}"))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = h
        .send("GET", "/api/v1/depots/game/revisions", &ci, Value::Null)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, info) = h.send("GET", "/api/v1/info", &ci, Value::Null).await;
    assert!(!info["modules"]
        .as_array()
        .unwrap()
        .contains(&json!("checks")));

    // A fresh server without the module has no table.
    let fresh = setup_with(modules(false, true)).await;
    let tables: i64 = fresh
        .state
        .meta()
        .read(|tx| {
            tx.db()
                .tables()
                .map(|t| t.iter().filter(|n| n.starts_with("mod_checks_")).count() as i64)
        })
        .await
        .unwrap();
    assert_eq!(tables, 0);
}
