//! T153 acceptance: `loded migrate-db` moves a server's metadata (commits, locks, shelves,
//! webhooks, checks) from SQLite into Postgres; the result passes fsck and serves the same
//! history. Runs only with `LODE_TEST_POSTGRES`. The only test in this binary, because it
//! clears that variable so its source server uses SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_meta::Reads;
use lode_proto::checks::{
    CheckState, CheckTarget, CheckTargetKind, ChecksQuery, ReportCheckRequest,
};
use lode_proto::webhooks::CreateWebhookRequest;
use lode_server::maintenance::{fsck, FsckOptions};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::Value;

fn modules_on(c: &mut lode_server::ServerConfig) {
    c.modules.checks = true;
    c.modules.webhooks = true;
    c.webhooks.allow_http = true;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrate_db_moves_everything_and_serves_the_same_history() {
    let Some(url) = tokio::task::spawn_blocking(lode_meta::test_postgres_url)
        .await
        .unwrap()
    else {
        return;
    };
    let url = url.unwrap();
    std::env::remove_var("LODE_TEST_POSTGRES");

    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(modules_on),
    })
    .await;
    server.create_depot("game").await;
    let token = server
        .token_for(
            "admin",
            &["read", "write", "admin", "lock", "shelve", "checks"],
        )
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    for i in 0..3 {
        ws.write(&format!("src/f{i}.txt"), format!("file {i}"));
        ws.json(&["add", &format!("src/f{i}.txt")]);
        ws.json(&["commit", "-m", &format!("change {i}")]);
    }
    ws.json(&["lock", "src/f0.txt", "-m", "editing"]);
    ws.write("src/f1.txt", "work in progress");
    ws.json(&["shelve", "-m", "wip"]);
    let admin = server.client(&token);
    admin
        .create_webhook(
            "game",
            &CreateWebhookRequest {
                url: "http://127.0.0.1:9/hook".into(),
                events: vec!["revision.created".into()],
                branch: None,
                format: Default::default(),
            },
        )
        .await
        .unwrap();
    let rev = CheckTarget {
        kind: CheckTargetKind::Revision,
        id: 3,
        version: None,
    };
    admin
        .report_check(
            "game",
            &ReportCheckRequest {
                target: rev,
                name: "ci/build".into(),
                state: CheckState::Success,
                summary: Some("green".into()),
                url: None,
            },
        )
        .await
        .unwrap();
    let log_before = ws.json(&["log"]);
    let locks_before = ws.json(&["locks"])["locks"].clone();
    let shelves_before = ws.json(&["shelves"])["shelves"].clone();

    // The data directory's loded.toml, with the same modules as the running server.
    let data = server.data_dir();
    let config = data.join("loded.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("webhooks = false", "webhooks = true")
        .replace("checks = false", "checks = true");
    std::fs::write(&config, text).unwrap();
    let url_file = data.join("database-url.txt");
    std::fs::write(&url_file, format!("{url}\n")).unwrap();
    let report = lode_server::maintenance::migrate_db_to_postgres(
        &config,
        &url_file,
        lode_testkit::available_modules(),
    )
    .await
    .unwrap();
    let copied: Vec<&str> = report.copy.tables.iter().map(|(t, _)| t.as_str()).collect();
    for t in [
        "revision",
        "mod_locks_lock",
        "mod_shelves_shelf",
        "mod_webhooks_hook",
        "mod_checks_check",
    ] {
        assert!(copied.contains(&t), "{t} not copied: {copied:?}");
    }
    assert!(data.join("loded.toml.before-postgres").is_file());
    let switched = lode_server::ServerConfig::load(&config).unwrap();
    assert_eq!(
        switched.database.kind,
        lode_server::config::DatabaseKind::Postgres
    );

    // A second server on the same data directory, now with Postgres.
    let database = switched.database.clone();
    let moved = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| {
            modules_on(c);
            c.data_dir = data;
            c.database = database;
        }),
    })
    .await;
    assert_eq!(moved.state().meta().dialect(), lode_meta::Dialect::Postgres);
    let depot = moved
        .state()
        .meta()
        .read(|tx| tx.depot_by_name("game"))
        .await
        .unwrap()
        .unwrap();
    let check = fsck(moved.state(), depot.id, FsckOptions { full: true })
        .await
        .unwrap();
    assert!(check.problems.is_empty(), "{:?}", check.problems);

    let ws2 = TestWorkspace::new(&moved).with_token(&token);
    ws2.json(&["checkout", &format!("{}/game", moved.url()), "."]);
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("server");
        }
        v
    };
    assert_eq!(strip(&ws2.json(&["log"])), strip(&log_before));
    let locks_after = ws2.json(&["locks"])["locks"].clone();
    assert_eq!(
        locks_after.as_array().map(Vec::len),
        locks_before.as_array().map(Vec::len)
    );
    assert_eq!(locks_after[0]["path"], locks_before[0]["path"]);
    assert_eq!(
        ws2.json(&["shelves"])["shelves"][0]["description"],
        shelves_before[0]["description"]
    );
    let moved_admin = moved.client(&token);
    assert_eq!(
        moved_admin.webhooks("game").await.unwrap().webhooks.len(),
        1
    );
    let checks = moved_admin
        .checks(
            "game",
            &ChecksQuery {
                target_kind: CheckTargetKind::Revision,
                target_id: 3,
                target_version: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(checks.checks.len(), 1);
    // New writes continue after the copied ids.
    ws2.write("src/f9.txt", "after the move");
    ws2.json(&["add", "src/f9.txt"]);
    let out = ws2.json(&["commit", "-m", "on postgres"]);
    assert_eq!(out["rev"], 5, "{out}");
    let _ = rev;
}
