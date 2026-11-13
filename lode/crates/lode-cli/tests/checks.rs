//! T063 acceptance: `lode checks` and how `lode shelf submit` reports unmet checks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::checks::{CheckState, CheckTarget, CheckTargetKind, ReportCheckRequest};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::Value;

/// 2026-11-12 10:30:00 UTC.
const T0: i64 = 1_794_479_400_000;

const RULE: &[u8] = br#"[checks]
branches = ["main"]
required = [{ name = "ci/build", reporter = "ci" }, { name = "ci/test", reporter = "ci" }]
"#;

async fn server() -> (TestServer, String) {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
    server.clock().set_ms(T0);
    server.create_depot("game").await;
    let ci = server.create_user("ci", &["read", "checks"]).await;
    server
        .acl("game", "ci", "*", "**", AclEffect::Allow, AccessLevel::Read)
        .await;
    (server, ci)
}

async fn report(
    server: &TestServer,
    token: &str,
    target: CheckTarget,
    name: &str,
    state: CheckState,
    summary: Option<&str>,
    url: Option<&str>,
) {
    server
        .client(token)
        .report_check(
            "game",
            &ReportCheckRequest {
                target,
                name: name.into(),
                state,
                summary: summary.map(Into::into),
                url: url.map(Into::into),
            },
        )
        .await
        .unwrap();
    server.clock().advance_ms(60_000);
}

fn rev(n: u64) -> CheckTarget {
    CheckTarget {
        kind: CheckTargetKind::Revision,
        id: n,
        version: None,
    }
}

fn shelf(id: u64, version: u64) -> CheckTarget {
    CheckTarget {
        kind: CheckTargetKind::Shelf,
        id,
        version: Some(version),
    }
}

fn text(ws: &TestWorkspace, args: &[&str]) -> String {
    let out = ws.lode(args).success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checks_of_a_revision_and_a_shelf() {
    let (server, ci) = server().await;
    let r = server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    report(
        &server,
        &ci,
        rev(r),
        "ci/build",
        CheckState::Success,
        Some("built in 41 s"),
        Some("https://ci.example/b/7"),
    )
    .await;
    report(
        &server,
        &ci,
        rev(r),
        "ci/test",
        CheckState::Failure,
        Some("2 failed\nmore"),
        None,
    )
    .await;

    // Without a workspace.
    let bare = TestWorkspace::new(&server).with_token(server.admin_token());
    let rs = format!("r{r}");
    let args = ["--server", server.url(), "checks", &rs, "--depot", "game"];
    insta::assert_snapshot!("checks_revision", text(&bare, &args));
    insta::assert_snapshot!("checks_revision_json", pretty(&bare.json(&args)));

    // A shelf in a workspace; without a version, its current one.
    let shelver = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&shelver);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.write("b.txt", "b\n");
    ws.json(&["add", "b.txt"]);
    let id = ws.json(&["shelve", "-m", "b"])["shelf"].as_u64().unwrap();
    report(
        &server,
        &ci,
        shelf(id, 1),
        "ci/build",
        CheckState::Pending,
        None,
        None,
    )
    .await;
    let ids = id.to_string();
    let with_version = format!("{id}@1");
    assert_eq!(
        text(&ws, &["checks", &ids]),
        text(&ws, &["checks", &with_version])
    );
    insta::assert_snapshot!("checks_shelf", text(&ws, &["checks", &ids]));
    let v = ws.json(&["checks", &ids]);
    assert_eq!(v["target"]["version"], 1);
    assert_eq!(v["checks"][0]["state"], "pending");

    // Nothing reported yet.
    let none = format!("{id}@2");
    assert_eq!(
        text(&ws, &["checks", &none]).trim(),
        format!("No checks on shelf {id} version 2.")
    );

    // Bad targets are usage errors.
    let (code, err) = ws.json_status(&["checks", "x1"]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "usage");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_submit_names_the_unmet_checks() {
    let (server, ci) = server().await;
    server
        .commit_files("game", "main", &[(".lodeconfig", RULE)])
        .await;
    let shelver = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&shelver);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.write("b.txt", "b\n");
    ws.json(&["add", "b.txt"]);
    let id = ws.json(&["shelve", "-m", "b"])["shelf"].as_u64().unwrap();
    report(
        &server,
        &ci,
        shelf(id, 1),
        "ci/test",
        CheckState::Failure,
        None,
        None,
    )
    .await;

    let spec = format!("{id}@1");
    let out = ws.cmd().args(["shelf", "submit", &spec]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    insta::assert_snapshot!(
        "submit_refused",
        stderr.replace(&format!("shelf {id} "), "shelf [id] ")
    );

    let (code, v) = ws.json_status(&["shelf", "submit", &spec]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "checks_required");
    let names: Vec<&str> = v["error"]["details"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ci/build", "ci/test"]);

    // Once both pass, the submit goes through.
    report(
        &server,
        &ci,
        shelf(id, 1),
        "ci/build",
        CheckState::Success,
        None,
        None,
    )
    .await;
    report(
        &server,
        &ci,
        shelf(id, 1),
        "ci/test",
        CheckState::Success,
        None,
        None,
    )
    .await;
    let v = ws.json(&["shelf", "submit", &spec]);
    assert!(v["rev"].as_u64().is_some(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_module_the_message_says_so() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let bare = TestWorkspace::new(&server).with_token(server.admin_token());
    let (code, v) = bare.json_status(&["checks", "r1", "--depot", "game"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "module_disabled");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("checks module"));
}
