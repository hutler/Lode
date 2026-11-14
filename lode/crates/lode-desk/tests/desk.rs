//! T091 acceptance: each command the window calls, run against a test server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lode_desk::desk::{Desk, RecentWorkspace};
use lode_ops::output::{ProgressEvent, Ui};
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::PrincipalKind;
use lode_testkit::TestServer;

#[derive(Default)]
struct Events(Mutex<Vec<(String, u64, u64, bool)>>);

impl Ui for Events {
    fn line(&self, _text: &str) {}
    fn warn(&self, _text: &str) {}
    fn progress(&self, e: &ProgressEvent<'_>) {
        self.0
            .lock()
            .unwrap()
            .push((e.label.to_owned(), e.done, e.total, e.finished));
    }
}

struct Setup {
    server: TestServer,
    home: tempfile::TempDir,
    desk: Desk,
    events: Arc<Events>,
}

/// A server with a depot `game` of three files and a user `alice` (password `correct horse`) who may
/// write it, and an app whose lode config lives in a temporary home.
async fn setup() -> Setup {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"apple\n"),
                ("b.txt", b"banana\n"),
                ("d/c.txt", b"cherry\n"),
            ],
        )
        .await;
    server
        .admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: "alice".into(),
            display_name: None,
            owner: None,
            password: Some("correct horse".into()),
        })
        .await
        .unwrap();
    server
        .acl(
            "game",
            "alice",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    let home = tempfile::tempdir().unwrap();
    let events = Arc::new(Events::default());
    let desk = Desk::new(
        home.path().to_path_buf(),
        &home.path().join("app"),
        events.clone(),
    )
    .with_config_dir(home.path().join("config"));
    Setup {
        server,
        home,
        desk,
        events,
    }
}

#[tokio::test]
async fn sign_in_check_out_open_and_status() {
    let s = setup().await;
    let url = s.server.url().trim_end_matches('/').to_owned();

    assert!(s.desk.signed_in(url.clone()).await.unwrap().is_null());

    let err = s
        .desk
        .sign_in(url.clone(), "alice".into(), "wrong".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "unauthenticated", "{err:?}");

    let me = s
        .desk
        .sign_in(url.clone(), "alice".into(), "correct horse".into())
        .await
        .unwrap();
    assert_eq!(me["name"], "alice");
    let who = s.desk.signed_in(url.clone()).await.unwrap();
    assert_eq!(who["name"], "alice");

    let dir = s.home.path().join("work").join("game");
    let out = s
        .desk
        .checkout(
            format!("{url}/game"),
            dir.display().to_string(),
            "main".into(),
        )
        .await
        .unwrap();
    assert_eq!(out["files"], 3, "{out}");
    assert_eq!(std::fs::read(dir.join("d/c.txt")).unwrap(), b"cherry\n");

    // Checkout progress reached the UI and ended with a finished event.
    {
        let seen = s.events.0.lock().unwrap();
        let fetch: Vec<_> = seen.iter().filter(|e| e.0 == "Fetching").collect();
        assert!(!fetch.is_empty(), "{seen:?}");
        assert!(fetch.last().unwrap().3);
    }

    // The new workspace is remembered first.
    let recent = s.desk.recent();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].depot, "game");
    assert_eq!(recent[0].branch, "main");
    let root = recent[0].path.clone();

    let info = s.desk.open_workspace(root.clone()).await.unwrap();
    assert_eq!(info["depot"], "game");
    assert_eq!(info["branch"], "main");
    assert_eq!(info["rev"], out["rev"]);
    assert_eq!(info["server"].as_str().unwrap().trim_end_matches('/'), url);

    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    let st = s.desk.status(root.clone()).await.unwrap();
    let files = st["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{st}");
    assert_eq!(files[0]["path"], "a.txt");
    assert_eq!(files[0]["status"], "modified");

    // The workspace is not held between operations: the CLI's own code can open it now.
    let ctx = lode_ops::context::Ctx::in_dir(
        lode_ops::context::Globals::default(),
        &dir,
        Arc::new(lode_ops::output::Silent),
    )
    .unwrap()
    .with_config_dir(s.home.path().join("config"));
    lode_ops::cmd::status::status(&ctx, Default::default())
        .await
        .unwrap();

    s.desk.forget(&root).unwrap();
    assert_eq!(s.desk.recent(), Vec::<RecentWorkspace>::new());
    assert!(
        dir.join("a.txt").exists(),
        "forgetting leaves the folder alone"
    );
}

#[tokio::test]
async fn errors_come_back_with_their_codes() {
    let s = setup().await;
    let url = s.server.url().trim_end_matches('/').to_owned();

    // Not signed in.
    let err = s
        .desk
        .checkout(
            format!("{url}/game"),
            s.home.path().join("w").display().to_string(),
            "main".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "unauthenticated", "{err:?}");

    // A folder that is not a workspace.
    let plain = s.home.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let err = s
        .desk
        .open_workspace(plain.display().to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_a_workspace", "{err:?}");

    // A folder that does not exist.
    let err = s
        .desk
        .open_workspace(s.home.path().join("nope").display().to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");
    assert!(s.desk.recent().is_empty());
}
