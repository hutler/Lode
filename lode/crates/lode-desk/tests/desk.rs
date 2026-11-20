//! T091 acceptance: each command the window calls, run against a test server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lode_desk::desk::{Desk, RecentWorkspace, ShelveRequest, Side, Using};
use lode_ops::output::{ProgressEvent, Ui};
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::checks::{CheckState, CheckTarget, CheckTargetKind, ReportCheckRequest};
use lode_proto::PrincipalKind;
use lode_testkit::{TestServer, TestServerOptions};

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
    setup_on(TestServer::start().await).await
}

/// [`setup`] on a server started by the caller.
async fn setup_on(server: TestServer) -> Setup {
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
            sso: None,
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

#[tokio::test]
async fn diffs_of_working_files_and_revisions() {
    let s = setup().await;
    s.server
        .commit_files("game", "main", &[("a.txt", b"apple\npear\n")])
        .await;
    s.server
        .commit_files("game", "main", &[("img.bin", b"\x89PNG\0\0\x01\x02")])
        .await;
    let root = checked_out(&s).await;
    let log = s.desk.log(root.clone(), None, None, 10).await.unwrap();
    let revs: Vec<u64> = log["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_u64().unwrap())
        .collect();
    let (img_rev, pear_rev) = (revs[0], revs[1]);
    let b_log = s
        .desk
        .log(root.clone(), Some("b.txt".into()), None, 10)
        .await
        .unwrap();
    let first_rev = b_log["revisions"][0]["rev"].as_u64().unwrap();

    // A working file against its base.
    std::fs::write(std::path::Path::new(&root).join("a.txt"), "apple\nplum\n").unwrap();
    let d = s.desk.diff(root.clone(), "a.txt".into()).await.unwrap();
    assert_eq!(d["status"], "modified");
    assert_eq!(d["binary"], false);
    assert_eq!(
        (d["insertions"].as_u64(), d["deletions"].as_u64()),
        (Some(1), Some(1))
    );
    let patch = d["patch"].as_str().unwrap();
    assert!(patch.contains("-pear\n+plum\n"), "{patch}");
    let unchanged = s.desk.diff(root.clone(), "b.txt".into()).await.unwrap();
    assert!(unchanged.is_null(), "{unchanged}");

    // A file as a revision changed it, against the parent revision.
    let d = s
        .desk
        .diff_revision(root.clone(), pear_rev, "a.txt".into())
        .await
        .unwrap();
    assert_eq!(d["status"], "modified");
    assert!(
        d["patch"].as_str().unwrap().contains(" apple\n+pear\n"),
        "{d}"
    );

    // An added file has no old side; binary content is reported by size.
    let d = s
        .desk
        .diff_revision(root.clone(), first_rev, "b.txt".into())
        .await
        .unwrap();
    assert_eq!(d["status"], "added");
    assert!(d["old_size"].is_null());
    assert!(d["patch"].as_str().unwrap().contains("+banana\n"), "{d}");
    let d = s
        .desk
        .diff_revision(root.clone(), img_rev, "img.bin".into())
        .await
        .unwrap();
    assert_eq!(d["binary"], true);
    assert_eq!(d["new_size"], 8);
    assert!(!d["patch"].as_str().unwrap().contains("@@"), "{d}");

    // A path the revision did not change.
    let err = s
        .desk
        .diff_revision(root, img_rev, "a.txt".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");
}

#[tokio::test]
async fn locks_are_taken_listed_refused_and_released() {
    let s = setup().await;
    let url = s.server.url().trim_end_matches('/').to_owned();
    s.server
        .admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: "bob".into(),
            display_name: None,
            owner: None,
            password: Some("battery staple".into()),
            sso: None,
        })
        .await
        .unwrap();
    s.server
        .acl(
            "game",
            "bob",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    let root = checked_out(&s).await;

    // Bob, in his own app and workspace, locks b.txt.
    let bob_home = tempfile::tempdir().unwrap();
    let bob = Desk::new(
        bob_home.path().to_path_buf(),
        &bob_home.path().join("app"),
        Arc::new(Events::default()),
    )
    .with_config_dir(bob_home.path().join("config"));
    bob.sign_in(url.clone(), "bob".into(), "battery staple".into())
        .await
        .unwrap();
    let bob_dir = bob_home.path().join("game");
    bob.checkout(
        format!("{url}/game"),
        bob_dir.display().to_string(),
        "main".into(),
    )
    .await
    .unwrap();
    let bob_root = bob.recent()[0].path.clone();
    bob.lock(
        bob_root.clone(),
        vec!["b.txt".into()],
        Some("door art".into()),
    )
    .await
    .unwrap();

    // Alice locks a.txt with a comment and sees both locks, hers marked.
    let out = s
        .desk
        .lock(root.clone(), vec!["a.txt".into()], Some("  ".into()))
        .await
        .unwrap();
    assert_eq!(out["locked"], serde_json::json!(["a.txt"]));
    let all = s.desk.locks(root.clone(), false).await.unwrap();
    let list = all["locks"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{all}");
    let b = list.iter().find(|l| l["path"] == "b.txt").unwrap();
    assert_eq!(b["owner"]["name"], "bob");
    assert_eq!(b["mine"], false);
    assert_eq!(b["comment"], "door art");
    let mine = s.desk.locks(root.clone(), true).await.unwrap();
    assert_eq!(mine["locks"].as_array().unwrap().len(), 1, "{mine}");
    assert!(
        mine["locks"][0].get("comment").is_none(),
        "a blank comment is left out: {mine}"
    );

    // b.txt is Bob's: the lock is refused and names the file.
    let err = s
        .desk
        .lock(root.clone(), vec!["b.txt".into()], None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "locked", "{err:?}");
    assert_eq!(err.details["paths"][0], "b.txt", "{err:?}");

    // Breaking it needs depot admin, which Alice is not.
    let err = s
        .desk
        .unlock(root.clone(), vec!["b.txt".into()], true)
        .await
        .unwrap_err();
    assert_eq!(err.code, "permission_denied", "{err:?}");

    let out = s
        .desk
        .unlock(root.clone(), vec!["a.txt".into()], false)
        .await
        .unwrap();
    assert_eq!(out["released"], serde_json::json!(["a.txt"]));
    let mine = s.desk.locks(root, true).await.unwrap();
    assert!(mine["locks"].as_array().unwrap().is_empty(), "{mine}");
}

#[tokio::test]
async fn changelists_group_and_commit_one_group() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(dir.join("b.txt"), "blueberry\n").unwrap();
    std::fs::write(dir.join("d/c.txt"), "cranberry\n").unwrap();

    s.desk
        .changelist_new(root.clone(), "fruit".into(), "More fruit".into())
        .await
        .unwrap();
    s.desk
        .changelist_new(root.clone(), "later".into(), String::new())
        .await
        .unwrap();
    let err = s
        .desk
        .changelist_new(root.clone(), "bad name".into(), String::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, "usage", "{err:?}");
    let moved = s
        .desk
        .changelist_move(
            root.clone(),
            "fruit".into(),
            vec!["a.txt".into(), "b.txt".into()],
        )
        .await
        .unwrap();
    assert_eq!(moved["moved"].as_array().unwrap().len(), 2, "{moved}");

    let lists = s.desk.changelists(root.clone()).await.unwrap();
    let names: Vec<&str> = lists["changelists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["default", "fruit", "later"], "{lists}");
    assert_eq!(
        lists["changelists"][0]["files"],
        serde_json::json!(["d/c.txt"])
    );
    assert_eq!(lists["changelists"][1]["description"], "More fruit");

    // Committing the changelist takes its description and only its files.
    let r = s
        .desk
        .commit_changelist(root.clone(), "fruit".into(), None)
        .await
        .unwrap();
    assert_eq!(r["files"], 2, "{r}");
    let log = s.desk.log(root.clone(), None, None, 1).await.unwrap();
    assert_eq!(log["revisions"][0]["message"], "More fruit");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(paths(&st), [("d/c.txt".to_owned(), "modified".to_owned())]);

    // The emptied list is gone; deleting the other returns nothing to move.
    let lists = s.desk.changelists(root.clone()).await.unwrap();
    assert_eq!(lists["changelists"].as_array().unwrap().len(), 2, "{lists}");
    let del = s
        .desk
        .changelist_delete(root.clone(), "later".into())
        .await
        .unwrap();
    assert_eq!(del["files"], 0, "{del}");
}

#[tokio::test]
async fn shelves_are_made_shown_unshelved_and_submitted() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(dir.join("b.txt"), "blueberry\n").unwrap();

    // Shelving one file reverts it and leaves the other alone.
    let made = s
        .desk
        .shelve(
            root.clone(),
            ShelveRequest {
                paths: vec!["a.txt".into()],
                message: Some("Apricot".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let id = made["shelf"].as_u64().unwrap();
    assert_eq!(
        (made["version"].as_u64(), made["files"].as_u64()),
        (Some(1), Some(1)),
        "{made}"
    );
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"apple\n");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(paths(&st), [("b.txt".to_owned(), "modified".to_owned())]);

    let mine = s.desk.shelves(root.clone(), true, None).await.unwrap();
    assert_eq!(mine["shelves"].as_array().unwrap().len(), 1, "{mine}");
    assert_eq!(mine["shelves"][0]["description"], "Apricot");
    assert_eq!(mine["shelves"][0]["owner"]["name"], "alice");

    let shown = s.desk.shelf(root.clone(), id).await.unwrap();
    assert_eq!(shown["changes"][0]["path"], "a.txt", "{shown}");
    assert_eq!(shown["changes"][0]["action"], "modify", "{shown}");
    let patch = shown["diff"][0]["patch"].as_str().unwrap();
    assert!(
        patch.contains("-apple") && patch.contains("+apricot"),
        "{patch}"
    );

    // Unshelving brings it back, with the outcome per file.
    let back = s.desk.unshelve(root.clone(), id).await.unwrap();
    assert_eq!(
        back["files"],
        serde_json::json!([{ "path": "a.txt", "outcome": "applied" }])
    );
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"apricot\n");

    // Updating the shelf, keeping the change here, raises its version.
    std::fs::write(dir.join("a.txt"), "avocado\n").unwrap();
    let updated = s
        .desk
        .shelve(
            root.clone(),
            ShelveRequest {
                paths: vec!["a.txt".into()],
                keep: true,
                update: Some(id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated["version"], 2, "{updated}");
    assert_eq!(updated["kept"], true);
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"avocado\n");

    // Submitting the version reviewed before the update is refused.
    let stale = s
        .desk
        .shelf_submit(root.clone(), id, 1, None)
        .await
        .unwrap_err();
    assert_eq!(stale.code, "conflict", "{stale:?}");
    assert!(stale.message.contains("version 2"), "{stale:?}");

    let done = s
        .desk
        .shelf_submit(root.clone(), id, 2, None)
        .await
        .unwrap();
    let rev = done["rev"].as_u64().unwrap();
    let log = s.desk.log(root.clone(), None, None, 1).await.unwrap();
    assert_eq!(log["revisions"][0]["rev"].as_u64(), Some(rev));
    assert_eq!(log["revisions"][0]["message"], "Apricot");
    let submitted = s
        .desk
        .shelves(root.clone(), true, Some("submitted".into()))
        .await
        .unwrap();
    assert_eq!(
        submitted["shelves"][0]["submitted_rev"].as_u64(),
        Some(rev),
        "{submitted}"
    );

    // A second shelf, deleted.
    let other = s
        .desk
        .shelve(
            root.clone(),
            ShelveRequest {
                paths: vec!["b.txt".into()],
                message: Some("Blueberry".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let other = other["shelf"].as_u64().unwrap();
    s.desk.shelf_delete(root.clone(), other).await.unwrap();
    let open = s.desk.shelves(root.clone(), false, None).await.unwrap();
    assert!(open["shelves"].as_array().unwrap().is_empty(), "{open}");
    let err = s.desk.unshelve(root.clone(), other).await.unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");
}

#[tokio::test]
async fn a_submit_refused_by_checks_lists_the_missing_checks() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
    let s = setup_on(server).await;
    s.server
        .commit_files(
            "game",
            "main",
            &[(
                ".lodeconfig",
                br#"[checks]
branches = ["main"]
required = [{ name = "ci/build", reporter = "ci" }, { name = "ci/test", reporter = "ci" }]
"#,
            )],
        )
        .await;
    let ci = s.server.create_user("ci", &["read", "checks"]).await;
    s.server
        .acl("game", "ci", "*", "**", AclEffect::Allow, AccessLevel::Read)
        .await;
    let root = checked_out(&s).await;
    std::fs::write(std::path::Path::new(&root).join("a.txt"), "apricot\n").unwrap();
    let made = s
        .desk
        .shelve(
            root.clone(),
            ShelveRequest {
                message: Some("Apricot".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let id = made["shelf"].as_u64().unwrap();
    s.server
        .client(&ci)
        .report_check(
            "game",
            &ReportCheckRequest {
                target: CheckTarget {
                    kind: CheckTargetKind::Shelf,
                    id,
                    version: Some(1),
                },
                name: "ci/test".into(),
                state: CheckState::Failure,
                summary: Some("2 tests failed".into()),
                url: None,
            },
        )
        .await
        .unwrap();

    let err = s
        .desk
        .shelf_submit(root.clone(), id, 1, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "checks_required", "{err:?}");
    let missing: Vec<(&str, &str)> = err.details["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap(),
                m["state"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(missing[0].0, "ci/build", "{err:?}");
    assert_eq!(missing[1], ("ci/test", "failure"), "{err:?}");
    assert!(
        err.message.contains("ci/build: not reported yet"),
        "{err:?}"
    );
}

#[tokio::test]
async fn checks_of_revisions_are_read_for_the_rows_on_screen() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
    let s = setup_on(server).await;
    let ci = s.server.create_user("ci", &["read", "checks"]).await;
    s.server
        .acl("game", "ci", "*", "**", AclEffect::Allow, AccessLevel::Read)
        .await;
    let root = checked_out(&s).await;
    let head = s.desk.log(root.clone(), None, None, 1).await.unwrap()["revisions"][0]["rev"]
        .as_u64()
        .unwrap();
    for (name, state, summary) in [
        ("ci/build", CheckState::Success, None),
        ("ci/test", CheckState::Failure, Some("2 tests failed")),
    ] {
        s.server
            .client(&ci)
            .report_check(
                "game",
                &ReportCheckRequest {
                    target: CheckTarget {
                        kind: CheckTargetKind::Revision,
                        id: head,
                        version: None,
                    },
                    name: name.into(),
                    state,
                    summary: summary.map(Into::into),
                    url: Some("https://ci.example.com/42".into()),
                },
            )
            .await
            .unwrap();
    }

    let r = s
        .desk
        .checks(root.clone(), vec![format!("r{head}"), "r999".into()])
        .await
        .unwrap();
    assert_eq!(r["available"], true, "{r}");
    let mut on_head: Vec<(String, String)> = r["checks"][format!("r{head}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    on_head.sort();
    assert_eq!(
        on_head,
        [
            ("ci/build".to_owned(), "success".to_owned()),
            ("ci/test".to_owned(), "failure".to_owned())
        ]
    );
    let test = r["checks"][format!("r{head}")]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ci/test")
        .unwrap()
        .clone();
    assert_eq!(test["summary"], "2 tests failed");
    assert_eq!(test["reporter"]["name"], "ci");
    assert_eq!(test["url"], "https://ci.example.com/42");
    // A revision nobody reported on has none.
    assert!(
        r["checks"]["r999"].as_array().is_none_or(|a| a.is_empty()),
        "{r}"
    );
}

#[tokio::test]
async fn without_the_checks_module_there_are_no_badges_and_no_error() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let r = s.desk.checks(root, vec!["r1".into()]).await.unwrap();
    assert_eq!(r, serde_json::json!({ "available": false, "checks": {} }));
}

#[tokio::test]
async fn image_sides_of_a_working_file_and_of_a_revision() {
    let s = setup().await;
    const DAY: &[u8] = b"\x89PNG\r\n\x1a\n\0day";
    const NIGHT: &[u8] = b"\x89PNG\r\n\x1a\n\0night";
    s.server
        .commit_files("game", "main", &[("ui/map.png", DAY)])
        .await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    std::fs::write(dir.join("ui/map.png"), NIGHT).unwrap();
    std::fs::write(dir.join("ui/new.png"), DAY).unwrap();
    s.desk
        .add(root.clone(), vec!["ui/new.png".into()])
        .await
        .unwrap();

    let side = |path: &str, rev: Option<u64>, side: Side| {
        s.desk.image_side(root.clone(), path.to_owned(), rev, side)
    };
    let old = side("ui/map.png", None, Side::Old).await.unwrap().unwrap();
    assert_eq!(
        (old.size, old.bytes.as_deref()),
        (DAY.len() as u64, Some(DAY))
    );
    let new = side("ui/map.png", None, Side::New).await.unwrap().unwrap();
    assert_eq!(new.bytes.as_deref(), Some(NIGHT));
    // A new file has no old side.
    assert!(side("ui/new.png", None, Side::Old).await.unwrap().is_none());

    let done = s
        .desk
        .commit(
            root.clone(),
            vec!["ui/map.png".into(), "ui/new.png".into()],
            "Night".into(),
        )
        .await
        .unwrap();
    let rev = done["rev"].as_u64().unwrap();
    let old = side("ui/map.png", Some(rev), Side::Old)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.bytes.as_deref(), Some(DAY));
    let new = side("ui/map.png", Some(rev), Side::New)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.bytes.as_deref(), Some(NIGHT));
    assert!(side("ui/new.png", Some(rev), Side::Old)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        side("ui/new.png", Some(rev), Side::New)
            .await
            .unwrap()
            .unwrap()
            .bytes
            .as_deref(),
        Some(DAY)
    );
    let err = s
        .desk
        .image_side(root.clone(), "a.txt".into(), Some(rev), Side::New)
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");
}

/// T110: the branch picker's list, creating with and without switching, a switch that carries a
/// local change and leaves a lock behind, and archiving.
#[tokio::test]
async fn branches_create_switch_and_archive() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    s.server
        .commit_files("game", "main", &[("b.txt", b"banana split\n")])
        .await;

    let list = s.desk.branches(root.clone(), false).await.unwrap();
    assert_eq!(list["current"], "main", "{list}");
    let names: Vec<_> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["main"]);

    // From the workspace's own revision (r2), not main's head (r3), and without switching.
    let made = s
        .desk
        .branch_create(root.clone(), "art".into(), Some(" ".into()), false, None)
        .await
        .unwrap();
    assert_eq!(made["name"], "art", "{made}");
    assert_eq!(made["parent"]["branch"], "main");
    assert_eq!(made["parent"]["rev"], 2, "{made}");
    assert!(made.get("switch").is_none());
    let info = s.desk.open_workspace(root.clone()).await.unwrap();
    assert_eq!(info["branch"], "main", "{info}");

    // From another branch at a revision, switching to it.
    let made = s
        .desk
        .branch_create(
            root.clone(),
            "hotfix".into(),
            Some("main@r3".into()),
            true,
            None,
        )
        .await
        .unwrap();
    assert_eq!(made["parent"]["rev"], 3, "{made}");
    assert_eq!(made["switch"]["branch"], "hotfix", "{made}");
    assert_eq!(made["switch"]["switched_from"], "main");
    assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"banana split\n");

    // Switch to `art` with a local change and a held lock: the change comes along, the lock stays.
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    s.desk
        .lock(root.clone(), vec!["d/c.txt".into()], None)
        .await
        .unwrap();
    let out = s
        .desk
        .switch(root.clone(), "art".into(), None)
        .await
        .unwrap();
    assert_eq!(out["branch"], "art", "{out}");
    assert_eq!(out["switched_from"], "hotfix");
    assert_eq!(out["locks_left"], serde_json::json!(["d/c.txt"]), "{out}");
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"apricot\n");
    assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"banana\n");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(paths(&st), [("a.txt".to_owned(), "modified".to_owned())]);
    let info = s.desk.open_workspace(root.clone()).await.unwrap();
    assert_eq!(info["branch"], "art", "{info}");

    // Update brings the workspace to the branch head; nothing new here.
    let up = s.desk.update(root.clone(), None).await.unwrap();
    assert_eq!(up["branch"], "art", "{up}");

    // Archiving needs depot admin.
    let err = s
        .desk
        .branch_archive(root.clone(), "hotfix".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "permission_denied", "{err:?}");
    s.server
        .acl(
            "game",
            "alice",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Admin,
        )
        .await;
    let gone = s
        .desk
        .branch_archive(root.clone(), "hotfix".into())
        .await
        .unwrap();
    assert_eq!(gone["state"], "archived", "{gone}");
    let list = s.desk.branches(root.clone(), false).await.unwrap();
    assert_eq!(list["current"], "art");
    assert!(
        !list["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["name"] == "hotfix"),
        "{list}"
    );
    let all = s.desk.branches(root, true).await.unwrap();
    let hotfix = all["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "hotfix")
        .unwrap();
    assert_eq!(hotfix["state"], "archived");
}

/// T111: a merge with conflicts, each conflict's three sides, resolving with mine, theirs and an
/// edited result, and abandoning the merge.
#[tokio::test]
async fn merge_conflicts_and_resolve() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    s.server
        .commit_files("game", "main", &[("img.bin", b"\x00\x01base")])
        .await;
    s.desk.update(root.clone(), None).await.unwrap();
    s.desk
        .branch_create(
            root.clone(),
            "feat".into(),
            Some("main".into()),
            false,
            None,
        )
        .await
        .unwrap();
    s.server
        .commit_files(
            "game",
            "feat",
            &[
                ("a.txt", b"apple pie\n"),
                ("b.txt", b"banana split\n"),
                ("d/c.txt", b"cherry tart\n"),
                ("img.bin", b"\x00\x01theirs!"),
                ("e.txt", b"elderberry\n"),
            ],
        )
        .await;
    std::fs::write(dir.join("a.txt"), "apple crumble\n").unwrap();
    std::fs::write(dir.join("b.txt"), "banana bread\n").unwrap();
    std::fs::write(dir.join("d/c.txt"), "cherry jam\n").unwrap();
    std::fs::write(dir.join("img.bin"), b"\x00\x01mine").unwrap();

    let out = s
        .desk
        .merge(root.clone(), "feat".into(), None)
        .await
        .unwrap();
    assert_eq!(out["source"], "feat", "{out}");
    let outcome = |p: &str| {
        out["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == p)
            .map(|f| f["outcome"].as_str().unwrap().to_owned())
    };
    for p in ["a.txt", "b.txt", "d/c.txt", "img.bin"] {
        assert_eq!(outcome(p).as_deref(), Some("conflicted"), "{p}: {out}");
    }
    assert_eq!(outcome("e.txt").as_deref(), Some("applied"), "{out}");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(st["merges"][0]["branch"], "feat", "{st}");

    // A text conflict: base, mine and theirs, and the file with its markers.
    let c = s.desk.conflict(root.clone(), "a.txt".into()).await.unwrap();
    assert_eq!(c["text"], true, "{c}");
    assert_eq!(c["base"]["text"], "apple\n");
    assert_eq!(c["mine"]["text"], "apple crumble\n");
    assert_eq!(c["theirs"]["text"], "apple pie\n");
    let result = c["result"]["text"].as_str().unwrap();
    assert!(result.contains("<<<<<<< mine"), "{result}");
    assert!(result.contains("apple pie"), "{result}");
    assert!(c["theirs_rev"].as_u64().is_some(), "{c}");

    // A binary conflict: sizes only, theirs beside the file.
    let c = s
        .desk
        .conflict(root.clone(), "img.bin".into())
        .await
        .unwrap();
    assert_eq!(c["text"], false, "{c}");
    assert_eq!(c["mine"]["size"], 6);
    assert_eq!(c["theirs"]["size"], 9);
    assert!(c["mine"]["text"].is_null());
    let beside = c["theirs_file"].as_str().unwrap();
    assert!(beside.starts_with("img.bin.theirs-r"), "{beside}");

    // Not in conflict.
    let err = s
        .desk
        .conflict(root.clone(), "e.txt".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{err:?}");

    // Commit is refused while conflicts remain, and a merge is committed whole.
    let err = s
        .desk
        .commit_all(root.clone(), "early".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "conflicted", "{err:?}");
    let err = s
        .desk
        .commit(root.clone(), vec!["e.txt".into()], "part".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "usage", "{err:?}");

    s.desk
        .resolve(root.clone(), vec!["a.txt".into()], Using::Mine)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "apple crumble\n"
    );
    s.desk
        .resolve(
            root.clone(),
            vec!["b.txt".into(), "img.bin".into()],
            Using::Theirs,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("b.txt")).unwrap(),
        "banana split\n"
    );
    assert_eq!(
        std::fs::read(dir.join("img.bin")).unwrap(),
        b"\x00\x01theirs!"
    );
    assert!(!dir.join(beside).exists());
    let out = s
        .desk
        .resolve_with(root.clone(), "d/c.txt".into(), "cherry jam tart\n".into())
        .await
        .unwrap();
    assert_eq!(out["resolved"], serde_json::json!(["d/c.txt"]), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("d/c.txt")).unwrap(),
        "cherry jam tart\n"
    );
    let st = s.desk.status(root.clone()).await.unwrap();
    assert!(
        !paths(&st).iter().any(|(_, status)| status == "conflicted"),
        "{st}"
    );

    let done = s
        .desk
        .commit_all(root.clone(), "Merge feat".into())
        .await
        .unwrap();
    let rev = s
        .desk
        .revision(root.clone(), done["rev"].as_u64().unwrap())
        .await
        .unwrap();
    assert_eq!(rev["merged_from"][0]["branch"], "feat", "{rev}");

    // Abandoning keeps the files but forgets the merge.
    s.server
        .commit_files("game", "feat", &[("f.txt", b"fig\n")])
        .await;
    s.desk
        .merge(root.clone(), "feat".into(), None)
        .await
        .unwrap();
    let st = s.desk.status(root.clone()).await.unwrap();
    assert_eq!(st["merges"].as_array().unwrap().len(), 1, "{st}");
    let out = s.desk.abandon_merge(root.clone()).await.unwrap();
    assert!(out.is_object(), "{out}");
    let st = s.desk.status(root.clone()).await.unwrap();
    assert!(st["merges"].as_array().unwrap().is_empty(), "{st}");
    assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "fig\n");
}

/// T112: the view's summary, the depot's folders, a preview, excluding and including, and the
/// refusal to drop a changed file without force.
#[tokio::test]
async fn view_show_preview_exclude_include_and_refuse() {
    let s = setup().await;
    let root = checked_out(&s).await;
    let dir = std::path::PathBuf::from(&root);
    let v = |x: &[&str]| x.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();

    let shown = s.desk.view(root.clone()).await.unwrap();
    assert_eq!(shown["view"]["include"], serde_json::json!([]), "{shown}");
    assert_eq!(shown["server"]["files"], 3);
    assert_eq!(shown["server"]["bytes"], 6 + 7 + 7);
    assert_eq!(shown["disk"]["files"], 3);

    let tree = s.desk.view_tree(root.clone(), None).await.unwrap();
    let names: Vec<_> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["kind"].as_str().unwrap()))
        .collect();
    assert!(names.contains(&("d", "dir")), "{tree}");

    // A preview changes nothing.
    let preview = s
        .desk
        .view_apply(root.clone(), v(&[]), v(&["d/**"]), false, true)
        .await
        .unwrap();
    assert_eq!(preview["remove"]["files"], 1, "{preview}");
    assert_eq!(preview["remove"]["paths"], serde_json::json!(["d/c.txt"]));
    assert_eq!(preview["fetch"]["files"], 0);
    assert!(dir.join("d/c.txt").exists());

    let out = s
        .desk
        .view_apply(root.clone(), v(&[" "]), v(&["d/**", ""]), false, false)
        .await
        .unwrap();
    assert_eq!(out["removed"], 1, "{out}");
    assert!(!dir.join("d/c.txt").exists());
    let shown = s.desk.view(root.clone()).await.unwrap();
    assert_eq!(
        shown["view"]["exclude"],
        serde_json::json!(["d/**"]),
        "{shown}"
    );
    assert_eq!(shown["server"]["files"], 2);
    assert_eq!(shown["disk"]["files"], 2);
    // The folder is still listed, so it can be ticked back in.
    let tree = s
        .desk
        .view_tree(root.clone(), Some("d".into()))
        .await
        .unwrap();
    assert_eq!(tree["entries"][0]["name"], "c.txt", "{tree}");

    // Back in: the preview says what comes.
    let preview = s
        .desk
        .view_apply(root.clone(), v(&[]), v(&[]), false, true)
        .await
        .unwrap();
    assert_eq!(preview["fetch"]["files"], 1, "{preview}");
    assert_eq!(preview["fetch"]["bytes"], 7);
    s.desk
        .view_apply(root.clone(), v(&[]), v(&[]), false, false)
        .await
        .unwrap();
    assert_eq!(std::fs::read(dir.join("d/c.txt")).unwrap(), b"cherry\n");

    // A changed file leaving the view stops it, named; force drops it.
    std::fs::write(dir.join("a.txt"), "apricot\n").unwrap();
    let preview = s
        .desk
        .view_apply(root.clone(), v(&["d/**"]), v(&[]), false, true)
        .await
        .unwrap();
    assert_eq!(
        preview["blocked"],
        serde_json::json!(["a.txt"]),
        "{preview}"
    );
    let err = s
        .desk
        .view_apply(root.clone(), v(&["d/**"]), v(&[]), false, false)
        .await
        .unwrap_err();
    assert_eq!(err.code, "local_changes", "{err:?}");
    assert_eq!(err.details["paths"], serde_json::json!(["a.txt"]));
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"apricot\n");
    let out = s
        .desk
        .view_apply(root.clone(), v(&["d/**"]), v(&[]), true, false)
        .await
        .unwrap();
    assert_eq!(out["removed"], 2, "{out}");
    assert!(!dir.join("a.txt").exists());

    // Patterns that do not compile are refused before anything moves.
    let err = s
        .desk
        .view_apply(root, v(&["[oops"]), v(&[]), false, true)
        .await
        .unwrap_err();
    assert_ne!(err.code, "internal", "{err:?}");
}
