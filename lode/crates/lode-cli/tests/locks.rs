//! T031 acceptance: lock commands, read-only lock-required files, and locks seen at commit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::{json, Value};

const CONFIG: &[u8] = b"[locks]\nrequired = [\"**/*.psd\"]\n";

struct Team {
    server: TestServer,
    ana: String,
    bob: String,
}

async fn team(options: TestServerOptions) -> Team {
    let server = TestServer::start_with(options).await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeconfig", CONFIG),
                ("art/door.psd", b"door 1"),
                ("notes.txt", b"notes\n"),
            ],
        )
        .await;
    let ana = server.create_user("ana", &["read", "write", "lock"]).await;
    let bob = server.create_user("bob", &["read", "write", "lock"]).await;
    for user in ["ana", "bob"] {
        server
            .acl(
                "game",
                user,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
    }
    Team { server, ana, bob }
}

fn checkout(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

fn read_only(ws: &TestWorkspace, path: &str) -> bool {
    std::fs::metadata(ws.root().join(path))
        .unwrap()
        .permissions()
        .readonly()
}

/// Writes a file the way a user would after clearing its read-only bit by hand.
fn force_write(ws: &TestWorkspace, path: &str, content: &str) {
    let p = ws.root().join(path);
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    ws.write(path, content);
}

fn status_of(ws: &TestWorkspace, path: &str) -> Value {
    ws.json(&["status"])["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .cloned()
        .unwrap_or(Value::Null)
}

fn lock_paths(ws: &TestWorkspace) -> Vec<(String, String)> {
    ws.json(&["locks"])["locks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["path"].as_str().unwrap().to_owned(),
                l["owner"]["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locks_b_is_refused_a_commits_b_can_lock() {
    let t = team(TestServerOptions::default()).await;
    let a = checkout(&t.server, &t.ana);
    let b = checkout(&t.server, &t.bob);
    // Lock-required files arrive read-only; others do not.
    assert!(read_only(&a, "art/door.psd"));
    assert!(!read_only(&a, "notes.txt"));

    let out = a.json(&["lock", "art/door.psd", "-m", "repainting"]);
    assert_eq!(out["locked"], json!(["art/door.psd"]));
    assert!(!read_only(&a, "art/door.psd"));
    assert_eq!(status_of(&a, "art/door.psd"), Value::Null);
    let locks = a.json(&["locks"]);
    assert_eq!(locks["locks"][0]["comment"], "repainting");
    assert_eq!(locks["locks"][0]["mine"], true);
    let text = String::from_utf8(a.lode(&["locks"]).get_output().stdout.clone()).unwrap();
    assert!(
        text.starts_with("art/door.psd  ana (this workspace)  "),
        "{text}"
    );

    // B: refused to lock, refused to commit, and told so by status.
    let (code, v) = b.json_status(&["lock", "art/door.psd"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("locked")));
    assert_eq!(b.json(&["locks"])["locks"][0]["mine"], false);
    force_write(&b, "art/door.psd", "door by bob");
    let st = status_of(&b, "art/door.psd");
    assert_eq!(st["needs_lock"], true);
    let text = without_branch_line(
        String::from_utf8(b.lode(&["status"]).get_output().stdout.clone()).unwrap(),
    );
    assert!(text.contains("M  art/door.psd  (needs lock)"), "{text}");
    let (code, v) = b.json_status(&["commit", "-m", "mine"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("locked")));

    // A commits; the lock goes and the file is read-only again.
    a.write("art/door.psd", "door 2");
    a.json(&["status"]);
    let line = without_branch_line(
        String::from_utf8(a.lode(&["status"]).get_output().stdout.clone()).unwrap(),
    );
    assert_eq!(line, "ML art/door.psd\n");
    a.json(&["commit", "-m", "new door"]);
    assert!(read_only(&a, "art/door.psd"));
    assert!(lock_paths(&a).is_empty());

    // B: out of date until updated, then the lock is B's.
    b.json(&["revert", "art/door.psd"]);
    assert!(read_only(&b, "art/door.psd"));
    let (code, v) = b.json_status(&["lock", "art/door.psd"]);
    assert_eq!(
        (code, v["error"]["code"].clone()),
        (1, json!("out_of_date"))
    );
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("lode update"));
    b.json(&["update"]);
    assert_eq!(b.read("art/door.psd"), b"door 2");
    assert!(read_only(&b, "art/door.psd"));
    b.json(&["lock", "art/door.psd"]);
    assert!(!read_only(&b, "art/door.psd"));
    assert_eq!(lock_paths(&a), vec![("art/door.psd".into(), "bob".into())]);

    // Unlock makes it read-only again.
    let out = b.json(&["unlock", "art/door.psd"]);
    assert_eq!(out["released"], json!(["art/door.psd"]));
    assert!(read_only(&b, "art/door.psd"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_locks_and_a_broken_lock() {
    let t = team(TestServerOptions::default()).await;
    let a = checkout(&t.server, &t.ana);
    a.json(&["lock", "art/door.psd"]);
    a.write("art/door.psd", "door 2");
    a.json(&["commit", "-m", "first pass", "--keep-locks"]);
    assert!(!read_only(&a, "art/door.psd"));
    assert_eq!(status_of(&a, "art/door.psd"), Value::Null);
    assert_eq!(lock_paths(&a), vec![("art/door.psd".into(), "ana".into())]);

    // An administrator breaks the lock; A notices at the next update.
    let admin_token = t
        .server
        .token_for("admin", &["read", "write", "admin", "lock"])
        .await;
    let admin = checkout(&t.server, &admin_token);
    let out = admin.json(&["unlock", "--force", "art/door.psd"]);
    assert_eq!(out["released"], json!(["art/door.psd"]));
    let run = a.lode(&["update"]);
    let stderr = String::from_utf8(run.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("your lock on art/door.psd was released"),
        "{stderr}"
    );
    assert!(read_only(&a, "art/door.psd"));
    let line = without_branch_line(
        String::from_utf8(a.lode(&["status"]).get_output().stdout.clone()).unwrap(),
    );
    assert_eq!(line, "");

    // Without force, nobody but the holder can unlock.
    a.json(&["lock", "art/door.psd"]);
    let (code, v) = admin.json_status(&["unlock", "art/door.psd"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("locked")));
    // Files not yet on the server cannot be locked.
    a.write("art/new.psd", "new");
    a.json(&["add", "art/new.psd"]);
    let (code, v) = a.json_status(&["lock", "art/new.psd"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("not_found")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_module_is_reported_plainly() {
    let options = TestServerOptions {
        configure: Box::new(|c| c.modules.locks = false),
    };
    let t = team_without_lock_scope(options).await;
    let a = checkout(&t.server, &t.ana);
    // No module, no read-only files either way: the rule is the server's to enforce.
    for args in [
        &["lock", "art/door.psd"][..],
        &["locks"][..],
        &["unlock", "art/door.psd"][..],
    ] {
        let (code, v) = a.json_status(args);
        assert_eq!(code, 1, "{args:?}: {v}");
        assert_eq!(v["error"]["code"], "module_disabled", "{args:?}");
    }
    // Commits ignore `[locks]`.
    force_write(&a, "art/door.psd", "door 2");
    a.json(&["commit", "-m", "no locks here"]);
}

/// Like [`team`], for a server where `lock` is not a registered scope.
async fn team_without_lock_scope(options: TestServerOptions) -> Team {
    let server = TestServer::start_with(options).await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[(".lodeconfig", CONFIG), ("art/door.psd", b"door 1")],
        )
        .await;
    let ana = server.create_user("ana", &["read", "write"]).await;
    server
        .acl(
            "game",
            "ana",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    Team {
        server,
        bob: String::new(),
        ana,
    }
}

/// Human `status` output without its first line, `On branch … at r…`.
fn without_branch_line(text: String) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
    assert!(first.starts_with("On branch main at r"), "{text}");
    rest.to_owned()
}
