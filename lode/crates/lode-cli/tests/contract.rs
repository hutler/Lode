//! T028 acceptance: with `--json` every command, succeeding or failing, prints exactly one JSON
//! document with the exit code of SPEC §9; unknown commands run `lode-<name>` from PATH.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};

/// Runs `lode --json args` and checks the envelope against the exit code.
fn check(ws: &TestWorkspace, args: &[&str], want: i32) {
    // `json_status` fails the test unless stdout is exactly one JSON document.
    let (code, v) = ws.json_status(args);
    assert_eq!(code, want, "lode {args:?}: {v}");
    assert_eq!(v["ok"], code == 0, "lode {args:?}: {v}");
    if code != 0 {
        assert!(v["error"]["code"].is_string(), "lode {args:?}: {v}");
        assert!(v["error"]["message"].is_string(), "lode {args:?}: {v}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_command_prints_one_document_with_the_right_exit_code() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n"), ("b.txt", b"b\n")])
        .await;
    let url = format!("{}/game", server.url());
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let bare = TestWorkspace::new(&server).with_token(server.admin_token());

    // Before a workspace exists.
    check(&ws, &["--version"], 0);
    check(&ws, &["whoami"], 0);
    check(&ws, &["frobnicate"], 2);
    check(&ws, &["status"], 1);
    check(&ws, &["log"], 2);
    check(
        &ws,
        &["checkout", &format!("{}/nope", server.url()), "."],
        1,
    );
    assert!(
        !ws.exists(".lode"),
        "a failed checkout left a workspace behind"
    );
    check(&ws, &["checkout", "http://example.com/game"], 2);
    check(&ws, &["login", server.url()], 2);
    check(&ws, &["checkout", &url, "."], 0);
    check(&ws, &["checkout", &url, "."], 1);

    // Local commands.
    check(&ws, &["status"], 0);
    check(&ws, &["view", "show"], 0);
    check(&ws, &["view", "exclude", "x/**"], 0);
    check(&ws, &["view", "exclude", "--remove", "x/**"], 0);
    check(&ws, &["view", "include", "[bad"], 2);
    ws.write("new.txt", "n\n");
    check(&ws, &["add", "new.txt"], 0);
    check(&ws, &["add", "nope.txt"], 1);
    check(&ws, &["mv", "new.txt", "new2.txt"], 0);
    check(&ws, &["mv", "nope.txt", "x.txt"], 1);
    check(&ws, &["cp", "new2.txt", "new3.txt"], 0);
    check(&ws, &["rm", "--keep", "new3.txt"], 0);
    check(&ws, &["rm", "nope.txt"], 1);
    check(&ws, &["revert", "new2.txt"], 0);
    check(&ws, &["revert"], 2);
    check(&ws, &["revert", "--all", "a.txt"], 2);
    ws.write("a.txt", "changed\n");
    check(&ws, &["diff"], 0);
    check(&ws, &["diff", "--stat"], 0);
    check(&ws, &["clean", "--dry-run"], 0);
    check(&ws, &["clean"], 0);
    check(&ws, &["resolve", "a.txt", "--mine"], 1);
    check(&ws, &["resolve", "a.txt"], 2);

    // Server commands.
    check(&ws, &["commit", "-m", "change a"], 0);
    check(&ws, &["commit", "-m", "again"], 1);
    check(&ws, &["update"], 0);
    check(&ws, &["update", "-r", "999"], 1);
    check(&ws, &["log", "-n", "2", "-v"], 0);
    check(&ws, &["cat", "a.txt"], 0);
    check(&ws, &["cat", "nope.txt"], 1);
    check(&ws, &["ls", "-l"], 0);
    check(&bare, &["log", "--depot", "game"], 0);
    check(&bare, &["ls", "--depot", "nope"], 1);

    // Authentication and network.
    let stranger = TestWorkspace::new(&server).with_token("not-a-token");
    check(&stranger, &["whoami"], 3);
    check(&stranger, &["log", "--depot", "game"], 3);
    let reader = server.create_user("reader", &["read"]).await;
    let reader_ws = TestWorkspace::new(&server).with_token(&reader);
    check(&reader_ws, &["ls", "--depot", "game"], 1);
    check(&ws, &["--server", "http://127.0.0.1:9", "whoami"], 4);
    check(&ws, &["logout"], 0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_commands_run_lode_name_with_the_context() {
    use std::os::unix::fs::PermissionsExt;

    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("lode-hello");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s\\n' \"$LODE_SERVER\" \"$LODE_DEPOT\" \"$LODE_WORKSPACE\" \"$LODE_TOKEN_FILE\" \"$LODE_JSON\" \"$*\"\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = ws
        .cmd()
        .env("PATH", path)
        .args(["--json", "hello", "world", "--flag"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    let line = String::from_utf8(out.stdout).unwrap();
    let parts: Vec<&str> = line.trim_end().split('|').collect();
    assert_eq!(parts[0].trim_end_matches('/'), server.url());
    assert_eq!(parts[1], "game");
    assert_eq!(
        std::fs::canonicalize(parts[2]).unwrap(),
        std::fs::canonicalize(ws.root()).unwrap()
    );
    // A path to the token, never the token.
    assert!(std::path::Path::new(parts[3]).is_file());
    assert_ne!(parts[3], server.admin_token());
    assert_eq!(parts[4], "1");
    assert_eq!(parts[5], "world --flag");
}
