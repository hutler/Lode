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

/// Signs alice in and checks out `game` into `<home>/work/game`; returns the workspace root.
async fn checked_out(s: &Setup) -> String {
    let url = s.server.url().trim_end_matches('/').to_owned();
    s.desk
        .sign_in(url.clone(), "alice".into(), "correct horse".into())
        .await
        .unwrap();
    let dir = s.home.path().join("work").join("game");
    s.desk
        .checkout(
            format!("{url}/game"),
            dir.display().to_string(),
            "main".into(),
        )
        .await
        .unwrap();
    s.desk.recent()[0].path.clone()
}

fn paths(status: &serde_json::Value) -> Vec<(String, String)> {
    status["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn add_commit_and_revert_picked_files() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(dir.join("b.txt"), "blueberry\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();

    let added = s
        .desk
        .add(root.clone(), vec!["new.txt".into()])
        .await
        .unwrap();
    assert!(added.is_object(), "{added}");

    // Commit only the picked files; b.txt stays pending.
    let done = s
        .desk
        .commit(
            root.clone(),
            vec!["a.txt".into(), "new.txt".into()],
            "Two of three".into(),
        )
        .await
        .unwrap();
    assert_eq!(done["files"], 2, "{done}");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(paths(&st), [("b.txt".to_owned(), "modified".to_owned())]);

    // Nothing picked is refused before the operation runs.
    let err = s
        .desk
        .commit(root.clone(), vec![], "x".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "usage");

    s.desk
        .revert(root.clone(), vec!["b.txt".into()])
        .await
        .unwrap();
    assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"banana\n");
    let st = s.desk.status(root).await.unwrap();
    assert!(paths(&st).is_empty(), "{st}");
}

#[tokio::test]
async fn a_refused_commit_names_the_files() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    // Someone else commits a.txt first.
    s.server
        .commit_files("game", "main", &[("a.txt", b"avocado\n")])
        .await;
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    let err = s
        .desk
        .commit(root, vec!["a.txt".into()], "Mine".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "out_of_date", "{err:?}");
    assert_eq!(err.details["paths"][0], "a.txt", "{err:?}");
}

#[tokio::test]
async fn watching_reports_changes_once_per_burst() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut s = setup().await;
    let home = s.home.path().to_path_buf();
    s.desk = Desk::new(home.clone(), &home.join("app"), s.events.clone())
        .with_config_dir(home.join("config"))
        .with_change_hook(move |root| {
            let _ = tx.send(root);
        });
    let root = checked_out(&s).await;
    s.desk.watch(root.clone()).unwrap();
    s.desk.watch(root.clone()).unwrap();
    let dir = std::path::PathBuf::from(&root);
    for i in 0..20 {
        std::fs::write(dir.join(format!("n{i}.txt")), "x").unwrap();
    }
    let got = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, root);
    // Status, which only reads, does not count as a change.
    s.desk.status(root.clone()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    assert!(rx.try_recv().is_err(), "one burst, one call");

    s.desk.unwatch(&root);
    std::fs::write(dir.join("late.txt"), "x").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    assert!(rx.try_recv().is_err(), "no calls after unwatch");
}

#[tokio::test]
async fn history_pages_revisions_files_and_blame() {
    let s = setup().await;
    for i in 0..5 {
        let text = format!("apple\nline {i}\n");
        s.server
            .commit_files("game", "main", &[("a.txt", text.as_bytes())])
            .await;
    }
    let root = checked_out(&s).await;

    let first = s.desk.log(root.clone(), None, None, 3).await.unwrap();
    let revs: Vec<u64> = first["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_u64().unwrap())
        .collect();
    assert_eq!(revs.len(), 3);
    assert!(
        revs.windows(2).all(|w| w[0] > w[1]),
        "newest first: {revs:?}"
    );
    assert_eq!(first["more"], true);
    let second = s
        .desk
        .log(root.clone(), None, Some(revs[2]), 3)
        .await
        .unwrap();
    let older = second["revisions"][0]["rev"].as_u64().unwrap();
    assert_eq!(
        older,
        revs[2] - 1,
        "the next page starts below the last one"
    );

    // One revision with its changes.
    let one = s.desk.revision(root.clone(), revs[0]).await.unwrap();
    assert_eq!(one["rev"], revs[0]);
    assert_eq!(one["changes"][0]["path"], "a.txt");
    assert_eq!(one["changes"][0]["action"], "modify");
    let err = s.desk.revision(root.clone(), 9999).await.unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");

    // File history: only revisions that changed b.txt.
    let fh = s
        .desk
        .log(root.clone(), Some("b.txt".into()), None, 50)
        .await
        .unwrap();
    assert_eq!(fh["revisions"].as_array().unwrap().len(), 1, "{fh}");

    // Blame names the revision of each line.
    let b = s
        .desk
        .blame(root.clone(), "a.txt".into(), None)
        .await
        .unwrap();
    let lines = b["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["text"], "line 4");
    assert_eq!(lines[1]["rev"].as_u64(), Some(revs[0]));
    let old = s
        .desk
        .blame(root, "a.txt".into(), Some(revs[2]))
        .await
        .unwrap();
    assert_eq!(old["lines"][1]["text"], "line 2");
}
