//! T170 acceptance: `lode passwd`, and `lode login` meeting a password that must be changed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_server::Clock as _;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn run(ws: &TestWorkspace, args: &[&str], stdin: &str) -> (bool, Value) {
    let out = ws
        .cmd()
        .arg("--json")
        .args(args)
        .write_stdin(stdin)
        .output()
        .unwrap();
    (
        out.status.success(),
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passwd_changes_own_password_and_login_reports_a_must_change_one() {
    let server = TestServer::start().await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());
    let (ok, v) = run(
        &admin,
        &["admin", "user", "create", "ana", "--password-stdin"],
        "first password\n",
    );
    assert!(ok, "{v}");
    let ws = TestWorkspace::detached();
    let url = server.url().to_owned();

    let (ok, v) = run(
        &ws,
        &["passwd", &url, "--name", "ana", "--password-stdin"],
        "wrong password\nsecond password\n",
    );
    assert!(!ok);
    assert_eq!(v["error"]["code"], "unauthenticated", "{v}");
    let (ok, v) = run(
        &ws,
        &["passwd", &url, "--name", "ana", "--password-stdin"],
        "first password\nsecond password\n",
    );
    assert!(ok, "{v}");
    assert_eq!(v["name"], "ana");
    let (ok, v) = run(
        &ws,
        &["login", &url, "--name", "ana", "--password-stdin"],
        "second password\n",
    );
    assert!(ok, "{v}");

    // Signed in: `passwd` finds the name by itself.
    let (ok, v) = run(
        &ws,
        &["passwd", &url, "--password-stdin"],
        "second password\nthird password\n",
    );
    assert!(ok, "{v}");
    assert_eq!(v["name"], "ana");

    // A must-change password: without a terminal, login says what to do.
    let by = server.clock().now_ms() + 60_000;
    server
        .state()
        .meta()
        .write(move |tx| {
            let p = lode_meta::Reads::principal_by_name(tx, "ana")?.unwrap();
            tx.set_password(p.id, p.password_hash.as_deref(), Some(by))
        })
        .await
        .unwrap();
    let (ok, v) = run(
        &ws,
        &["login", &url, "--name", "ana", "--password-stdin"],
        "third password\n",
    );
    assert!(!ok);
    assert_eq!(
        v["error"]["details"]["reason"], "password_change_required",
        "{v}"
    );
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("lode passwd"));
}
