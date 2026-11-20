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
    let filtered = admin.json(&["admin", "acl", "list", "--depot", "game", "--subject", "bob"]);
    assert_eq!(filtered["rules"].as_array().unwrap().len(), 2, "{filtered}");
    let (code, err) = admin.json_status(&["admin", "acl", "list", "--depot", "nope"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "not_found");
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

/// T060: `lode admin webhook` against a server whose worker really sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhooks_from_the_command_line() {
    use std::sync::{Arc, Mutex};

    let server = TestServer::start_with(lode_testkit::TestServerOptions {
        configure: Box::new(|c| {
            c.modules.webhooks = true;
            c.webhooks.allow_http = true;
        }),
    })
    .await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());
    admin.json(&["admin", "depot", "create", "game"]);

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = seen.clone();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let record = record.clone();
        async move {
            let event = req
                .headers()
                .get("x-lode-event")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            record.lock().unwrap().push(event);
            axum::http::StatusCode::NO_CONTENT
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Human output prints the secret alone on stdout.
    let out = admin
        .cmd()
        .args([
            "admin",
            "webhook",
            "add",
            "game",
            &url,
            "--events",
            "revision.created,branch.created",
            "--branch",
            "main",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim().len(), 64, "{stdout}");

    let list = admin.json(&["admin", "webhook", "list", "game"]);
    let hook = &list["webhooks"][0];
    assert_eq!(hook["url"], url.as_str());
    assert_eq!(hook["branch"], "main");
    assert!(!list.to_string().contains(stdout.trim()));
    let id = hook["id"].as_i64().unwrap().to_string();

    let ping = admin.json(&["admin", "webhook", "ping", "game", &id]);
    assert_eq!(ping["event"], "ping");
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let d = admin.json(&["admin", "webhook", "deliveries", "game", &id]);
        let ok = d["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["state"] == "ok")
            .count();
        if ok == 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{d}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut got = seen.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, ["ping", "revision.created"]);

    // An unknown event is a usage error reported by the server.
    let (code, err) =
        admin.json_status(&["admin", "webhook", "add", "game", &url, "--events", "x.y"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "invalid_request");

    admin.json(&["admin", "webhook", "remove", "game", &id]);
    let list = admin.json(&["admin", "webhook", "list", "game"]);
    assert_eq!(list["webhooks"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn users_and_depots_are_deleted() {
    let server = TestServer::start().await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());
    admin.json(&["admin", "depot", "create", "game"]);
    admin.json(&["admin", "depot", "create", "art"]);
    json_in(
        &admin,
        &["admin", "user", "create", "ana", "--password-stdin"],
        "password-ana\n",
    );
    admin.json(&["admin", "agent", "create", "bot", "--owner", "ana"]);

    // Without a terminal, --yes is required.
    let (code, err) = admin.json_status(&["admin", "user", "delete", "ana"]);
    assert_eq!(code, 2, "{err}");
    let r = admin.json(&["admin", "user", "delete", "ana", "--yes"]);
    assert_eq!(r["name"], "ana");
    assert_eq!(r["agents"][0], "bot");
    assert!(names(&admin.json(&["admin", "user", "list"]), "principals")
        .iter()
        .all(|n| n != "ana"));

    // A depot goes to the trash and comes back.
    let (code, _) = admin.json_status(&["admin", "depot", "delete", "game"]);
    assert_eq!(code, 2);
    let r = admin.json(&["admin", "depot", "delete", "game", "--yes"]);
    assert_eq!(r["purged"], false);
    assert!(r["purge_after"].is_string());
    assert_eq!(
        names(&admin.json(&["admin", "depot", "trash"]), "depots"),
        ["game"]
    );
    assert_eq!(
        names(&admin.json(&["admin", "depot", "list"]), "depots"),
        ["art"]
    );
    admin.json(&["admin", "depot", "restore", "game"]);
    assert_eq!(
        names(&admin.json(&["admin", "depot", "list"]), "depots"),
        ["art", "game"]
    );

    // --force erases at once.
    let r = admin.json(&["admin", "depot", "delete", "art", "--force", "--yes"]);
    assert_eq!(r["purged"], true);
    assert!(names(&admin.json(&["admin", "depot", "trash"]), "depots").is_empty());
    assert_eq!(
        names(&admin.json(&["admin", "depot", "list"]), "depots"),
        ["game"]
    );
    let (code, err) = admin.json_status(&["admin", "depot", "restore", "art"]);
    assert_eq!(code, 1, "{err}");
}
