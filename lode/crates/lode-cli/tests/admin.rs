//! T053 acceptance: a server set up entirely with `lode admin`, then checked by behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

/// `lode --json args…` with `stdin`, which must succeed.
fn json_in(ws: &TestWorkspace, args: &[&str], stdin: &str) -> Value {
    let out = ws
        .cmd()
        .arg("--json")
        .args(args)
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "lode {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn as_user(server: &TestServer, token: &str) -> TestWorkspace {
    TestWorkspace::new(server).with_token(token)
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripted_setup_then_behaviour() {
    let server = TestServer::start().await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());

    admin.json(&["admin", "depot", "create", "game"]);
    assert_eq!(
        names(&admin.json(&["admin", "depot", "list"]), "depots"),
        ["game"]
    );
    json_in(
        &admin,
        &["admin", "user", "create", "ana", "--password-stdin"],
        "password-ana\n",
    );
    json_in(
        &admin,
        &[
            "admin",
            "user",
            "create",
            "bob",
            "--display-name",
            "Bob B",
            "--password-stdin",
        ],
        "password-bob\n",
    );
    admin.json(&["admin", "group", "create", "devs"]);
    admin.json(&["admin", "group", "add", "devs", "ana"]);
    let groups = admin.json(&["admin", "group", "list"]);
    assert_eq!(groups["groups"][0]["members"], serde_json::json!(["ana"]));
    let bot = admin.json(&["admin", "agent", "create", "bot", "--owner", "ana"]);
    assert_eq!(bot["owner"], "ana");
    let users = names(&admin.json(&["admin", "user", "list"]), "principals");
    assert!(
        users.contains(&"ana".to_owned()) && users.contains(&"bob".to_owned()),
        "{users:?}"
    );
    assert!(!users.contains(&"bot".to_owned()));
    assert_eq!(
        names(&admin.json(&["admin", "agent", "list"]), "principals"),
        ["bot"]
    );

    let token = |who: &str, scopes: &str| -> String {
        let mut args = vec!["admin", "token", "create", "--for", who];
        if !scopes.is_empty() {
            args.extend(["--scope", scopes]);
        }
        admin.json(&args)["token"].as_str().unwrap().to_owned()
    };
    let ana = token("ana", "read,write");
    let bob = token("bob", "read,write");
    let bot = token("bot", "");

    // Rules: devs write everything; bob reads all but secret/; bot reads.
    admin.json(&[
        "admin",
        "acl",
        "add",
        "allow",
        "write",
        "group:devs",
        "--depot",
        "game",
    ]);
    admin.json(&[
        "admin", "acl", "add", "allow", "read", "bob", "--depot", "game",
    ]);
    let deny = admin.json(&[
        "admin",
        "acl",
        "add",
        "deny",
        "read",
        "user:bob",
        "--depot",
        "game",
        "--path",
        "secret/**",
        "--check",
        "bob",
        "main",
        "secret/key.txt",
    ]);
    assert_eq!(deny["check"]["level"], Value::Null);
    let allow = admin.json(&[
        "admin", "acl", "add", "allow", "read", "user:bot", "--depot", "game", "--check", "bot",
        "main", "a.txt",
    ]);
    assert_eq!(allow["check"]["level"], "read");
    let check = admin.json(&[
        "admin",
        "acl",
        "check",
        "ana",
        "game",
        "main",
        "secret/key.txt",
    ]);
    assert_eq!(check["level"], "write");
    assert_eq!(
        admin.json(&["admin", "acl", "list"])["rules"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    // Bad patterns never reach the server.
    let (code, err) = admin.json_status(&[
        "admin", "acl", "add", "allow", "read", "all", "--path", "a/[", "--depot", "game",
    ]);
    assert_eq!((code, err["error"]["code"].as_str()), (2, Some("usage")));

    server
        .commit_files(
            "game",
            "main",
            &[("a.txt", b"a\n"), ("secret/key.txt", b"k\n")],
        )
        .await;

    // ana commits; bob sees no secret and cannot commit; the agent reads.
    let wa = as_user(&server, &ana);
    wa.json(&["checkout", &format!("{}/game", server.url()), "."]);
    wa.write("a.txt", "ana\n");
    wa.json(&["commit", "-m", "by ana"]);
    let wb = as_user(&server, &bob);
    wb.json(&["checkout", &format!("{}/game", server.url()), "."]);
    assert!(wb.exists("a.txt") && !wb.exists("secret/key.txt"));
    wb.write("a.txt", "bob\n");
    let (code, err) = wb.json_status(&["commit", "-m", "by bob"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "permission_denied");
    let wbot = as_user(&server, &bot);
    wbot.json(&["checkout", &format!("{}/game", server.url()), "."]);
    assert_eq!(wbot.read("a.txt"), b"ana\n");

    // Tokens: listed without values, revoked; a disabled user is shut out.
    let listed = admin.json(&["admin", "token", "list", "--for", "ana"]);
    let tokens = listed["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(tokens[0].get("token").is_none());
    let id = tokens[0]["id"].as_i64().unwrap().to_string();
    admin.json(&["admin", "token", "revoke", &id, "--for", "ana"]);
    let (code, _) = wa.json_status(&["whoami"]);
    assert_ne!(code, 0);
    admin.json(&["admin", "user", "disable", "bob"]);
    let (code, _) = wb.json_status(&["whoami"]);
    assert_ne!(code, 0);
    admin.json(&["admin", "user", "disable", "bob", "--enable"]);
    assert_eq!(wb.json(&["whoami"])["name"], "bob");
    json_in(
        &admin,
        &["admin", "user", "passwd", "ana", "--password-stdin"],
        "new-password\n",
    );

    // Human output prints a new token alone on stdout.
    let out = admin
        .cmd()
        .args([
            "admin", "token", "create", "--for", "ana", "--scope", "read",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.starts_with("lode_"), "{stdout}");

    // Maintenance.
    let gc = admin.json(&["admin", "gc", "game", "--dry-run"]);
    assert_eq!(gc["dry_run"], true);
    let fsck = admin.json(&["admin", "fsck", "game"]);
    assert_eq!(fsck["objects_affected"], 0);
}
