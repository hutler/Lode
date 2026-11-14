//! T115 acceptance: the Lode Desk guide (`docs/desk-guide.md`) followed section by section
//! against a real server, by two people in two apps, through the same commands the window calls.
//! What needs a person (installers, an update, a link clicked in another app) is in
//! `docs/desk-acceptance.md`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lode_desk::desk::{Desk, ShelveRequest, Side};
use lode_desk::links::{self, Link};
use lode_desk::update::{self, Settings};
use lode_ops::output::{ProgressEvent, Ui};
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::PrincipalKind;
use lode_testkit::TestServer;
use serde_json::{json, Value};

struct Quiet;

impl Ui for Quiet {
    fn line(&self, _text: &str) {}
    fn warn(&self, _text: &str) {}
    fn progress(&self, _e: &ProgressEvent<'_>) {}
}

/// One person's app, with its own home and lode config.
struct Person {
    desk: Desk,
    home: tempfile::TempDir,
}

impl Person {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let desk = Desk::new(
            home.path().to_path_buf(),
            &home.path().join("app"),
            Arc::new(Quiet),
        )
        .with_config_dir(home.path().join("config"));
        Self { desk, home }
    }

    fn dir(&self) -> PathBuf {
        self.home.path().join("work").join("game")
    }
}

async fn person(server: &TestServer, name: &str, password: &str) {
    server
        .admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: name.into(),
            display_name: None,
            owner: None,
            password: Some(password.into()),
        })
        .await
        .unwrap();
    server
        .acl(
            "game",
            name,
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
}

fn files(status: &Value) -> Vec<(String, String)> {
    status["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["status"] != "unchanged")
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| (*x).to_owned()).collect()
}

#[tokio::test]
async fn the_desk_guide_end_to_end() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("README.md", b"# Game\n"),
                ("Source/boat.rs", b"fn sail() {}\n"),
                ("Art/harbor.png", b"\x89PNG\r\n\x1a\n\0harbor v1"),
                ("Docs/notes.md", b"notes\n"),
            ],
        )
        .await;
    person(&server, "alice", "correct horse").await;
    person(&server, "bob", "battery staple").await;
    let url = server.url().trim_end_matches('/').to_owned();
    let alice = Person::new();
    let bob = Person::new();

    // ## Sign in. A wrong password says so; the right one names who is signed in.
    let err = alice
        .desk
        .sign_in(url.clone(), "alice".into(), "wrong".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "unauthenticated", "{err:?}");
    let me = alice
        .desk
        .sign_in(url.clone(), "alice".into(), "correct horse".into())
        .await
        .unwrap();
    assert_eq!(me["name"], "alice");
    bob.desk
        .sign_in(url.clone(), "bob".into(), "battery staple".into())
        .await
        .unwrap();

    // ## Check out a depot. The workspace is then on Home's recent list.
    for p in [&alice, &bob] {
        let out = p
            .desk
            .checkout(
                format!("{url}/game"),
                p.dir().display().to_string(),
                "main".into(),
            )
            .await
            .unwrap();
        assert_eq!(out["files"], 4, "{out}");
    }
    let a_root = alice.desk.recent()[0].path.clone();
    let b_root = bob.desk.recent()[0].path.clone();
    let a = PathBuf::from(&a_root);
    let b = PathBuf::from(&b_root);
    let info = alice.desk.open_workspace(a_root.clone()).await.unwrap();
    assert_eq!(
        (info["depot"].as_str(), info["branch"].as_str()),
        (Some("game"), Some("main"))
    );
    let err = alice
        .desk
        .open_workspace(alice.home.path().display().to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_a_workspace", "{err:?}");

    // ## The daily loop: changes, locks, changelists, commit.
    // Lock the binary before editing it; Bob is refused the same lock.
    alice
        .desk
        .lock(
            a_root.clone(),
            strings(&["Art/harbor.png"]),
            Some("night pass".into()),
        )
        .await
        .unwrap();
    let err = bob
        .desk
        .lock(b_root.clone(), strings(&["Art/harbor.png"]), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "locked", "{err:?}");
    let locks = bob.desk.locks(b_root.clone(), false).await.unwrap();
    assert_eq!(locks["locks"][0]["owner"]["name"], "alice", "{locks}");
    assert_eq!(locks["locks"][0]["comment"], "night pass");

    write(&a, "Source/boat.rs", "fn sail() { tilt(); }\n");
    std::fs::write(
        a.join("Art/harbor.png"),
        b"\x89PNG\r\n\x1a\n\0harbor v2 at night",
    )
    .unwrap();
    write(&a, "Docs/sailing.md", "How to sail\n");
    let st = alice.desk.status(a_root.clone()).await.unwrap();
    assert_eq!(
        files(&st),
        [
            ("Art/harbor.png".to_owned(), "modified".to_owned()),
            ("Docs/sailing.md".to_owned(), "untracked".to_owned()),
            ("Source/boat.rs".to_owned(), "modified".to_owned()),
        ],
        "{st}"
    );
    let d = alice
        .desk
        .diff(a_root.clone(), "Source/boat.rs".into())
        .await
        .unwrap();
    assert!(
        d["patch"]
            .as_str()
            .unwrap()
            .contains("+fn sail() { tilt(); }"),
        "{d}"
    );
    let side = alice
        .desk
        .image_side(a_root.clone(), "Art/harbor.png".into(), None, Side::New)
        .await
        .unwrap();
    assert!(side.is_some());

    // A changelist holds the docs for later.
    alice
        .desk
        .add(a_root.clone(), strings(&["Docs/sailing.md"]))
        .await
        .unwrap();
    alice
        .desk
        .changelist_new(a_root.clone(), "docs".into(), "Sailing notes".into())
        .await
        .unwrap();
    alice
        .desk
        .changelist_move(a_root.clone(), "docs".into(), strings(&["Docs/sailing.md"]))
        .await
        .unwrap();
    // Commit the picked files; the lock is released by the commit.
    let done = alice
        .desk
        .commit(
            a_root.clone(),
            strings(&["Source/boat.rs", "Art/harbor.png"]),
            "Boat tilts; night harbor".into(),
        )
        .await
        .unwrap();
    assert_eq!(done["files"], 2, "{done}");
    let boat_rev = done["rev"].as_u64().unwrap();
    let done = alice
        .desk
        .commit_changelist(a_root.clone(), "docs".into(), None)
        .await
        .unwrap();
    assert_eq!(done["files"], 1, "{done}");
    assert!(files(&alice.desk.status(a_root.clone()).await.unwrap()).is_empty());
    let mine = alice.desk.locks(a_root.clone(), true).await.unwrap();
    assert!(
        mine["locks"].as_array().unwrap().is_empty(),
        "the commit released the lock: {mine}"
    );

    // ## History, blame and diffs. Bob updates and reads what Alice did.
    let up = bob.desk.update(b_root.clone(), None).await.unwrap();
    assert_eq!(up["branch"], "main", "{up}");
    assert_eq!(read(&b, "Docs/sailing.md"), "How to sail\n");
    let log = bob.desk.log(b_root.clone(), None, None, 10).await.unwrap();
    assert_eq!(log["revisions"][0]["message"], "Sailing notes", "{log}");
    assert_eq!(log["revisions"][1]["rev"].as_u64(), Some(boat_rev));
    assert_eq!(log["revisions"][1]["author"]["name"], "alice");
    let rev = bob.desk.revision(b_root.clone(), boat_rev).await.unwrap();
    assert_eq!(rev["changes"].as_array().unwrap().len(), 2, "{rev}");
    let file_log = bob
        .desk
        .log(b_root.clone(), Some("Source/boat.rs".into()), None, 10)
        .await
        .unwrap();
    assert_eq!(
        file_log["revisions"].as_array().unwrap().len(),
        2,
        "{file_log}"
    );
    let blame = bob
        .desk
        .blame(b_root.clone(), "Source/boat.rs".into(), None)
        .await
        .unwrap();
    assert_eq!(blame["lines"][0]["rev"].as_u64(), Some(boat_rev), "{blame}");
    let d = bob
        .desk
        .diff_revision(b_root.clone(), boat_rev, "Art/harbor.png".into())
        .await
        .unwrap();
    assert_eq!(d["binary"], true, "{d}");

    // ## Shelves and review. Bob shelves a change, Alice reviews and unshelves it, Bob submits.
    write(&b, "README.md", "# Game\n\nA boat game.\n");
    let made = bob
        .desk
        .shelve(
            b_root.clone(),
            ShelveRequest {
                paths: strings(&["README.md"]),
                message: Some("Say what the game is".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let shelf = made["shelf"].as_u64().unwrap();
    assert_eq!(read(&b, "README.md"), "# Game\n", "shelving reverts here");
    let open = alice
        .desk
        .shelves(a_root.clone(), false, None)
        .await
        .unwrap();
    assert_eq!(open["shelves"][0]["owner"]["name"], "bob", "{open}");
    let shown = alice.desk.shelf(a_root.clone(), shelf).await.unwrap();
    assert!(
        shown["diff"][0]["patch"]
            .as_str()
            .unwrap()
            .contains("+A boat game."),
        "{shown}"
    );
    let back = alice.desk.unshelve(a_root.clone(), shelf).await.unwrap();
    assert_eq!(back["files"][0]["outcome"], "applied", "{back}");
    alice
        .desk
        .revert(a_root.clone(), strings(&["README.md"]))
        .await
        .unwrap();
    let err = bob
        .desk
        .shelf_submit(b_root.clone(), shelf, 2, None)
        .await
        .unwrap_err();
    assert_eq!(
        err.code, "conflict",
        "a version that was not reviewed: {err:?}"
    );
    let submitted = bob
        .desk
        .shelf_submit(b_root.clone(), shelf, 1, None)
        .await
        .unwrap();
    assert!(submitted["rev"].as_u64().is_some(), "{submitted}");

    // ## Branches and merges, with a conflict resolved in the three-way view.
    let made = alice
        .desk
        .branch_create(a_root.clone(), "tilt".into(), Some("main".into()), true)
        .await
        .unwrap();
    assert_eq!(made["switch"]["branch"], "tilt", "{made}");
    write(&a, "Source/boat.rs", "fn sail() { tilt(0.3); }\n");
    alice
        .desk
        .commit(
            a_root.clone(),
            strings(&["Source/boat.rs"]),
            "Tilt by the wave".into(),
        )
        .await
        .unwrap();
    bob.desk.update(b_root.clone(), None).await.unwrap();
    write(&b, "Source/boat.rs", "fn sail() { tilt(); steer(); }\n");
    bob.desk
        .commit(
            b_root.clone(),
            strings(&["Source/boat.rs"]),
            "Steer while sailing".into(),
        )
        .await
        .unwrap();
    let back = alice
        .desk
        .switch(a_root.clone(), "main".into(), None)
        .await
        .unwrap();
    assert_eq!(back["branch"], "main", "{back}");
    assert_eq!(read(&a, "README.md"), "# Game\n\nA boat game.\n", "{back}");
    let merged = alice
        .desk
        .merge(a_root.clone(), "tilt".into(), None)
        .await
        .unwrap();
    assert_eq!(merged["files"][0]["outcome"], "conflicted", "{merged}");
    let err = alice
        .desk
        .commit_all(a_root.clone(), "too early".into())
        .await
        .unwrap_err();
    assert_eq!(err.code, "conflicted", "{err:?}");
    let c = alice
        .desk
        .conflict(a_root.clone(), "Source/boat.rs".into())
        .await
        .unwrap();
    assert_eq!(c["base"]["text"], "fn sail() { tilt(); }\n", "{c}");
    assert_eq!(c["mine"]["text"], "fn sail() { tilt(); steer(); }\n");
    assert_eq!(c["theirs"]["text"], "fn sail() { tilt(0.3); }\n");
    alice
        .desk
        .resolve_with(
            a_root.clone(),
            "Source/boat.rs".into(),
            "fn sail() { tilt(0.3); steer(); }\n".into(),
        )
        .await
        .unwrap();
    let done = alice
        .desk
        .commit_all(a_root.clone(), "Merge tilt".into())
        .await
        .unwrap();
    let merge_rev = done["rev"].as_u64().unwrap();
    let rev = alice
        .desk
        .revision(a_root.clone(), merge_rev)
        .await
        .unwrap();
    assert_eq!(rev["merged_from"][0]["branch"], "tilt", "{rev}");
    // Bob gets the merge with his next update.
    bob.desk.update(b_root.clone(), None).await.unwrap();
    assert_eq!(
        read(&b, "Source/boat.rs"),
        "fn sail() { tilt(0.3); steer(); }\n"
    );
    let branches = bob.desk.branches(b_root.clone(), false).await.unwrap();
    assert_eq!(branches["current"], "main");

    // ## The workspace view: leave Art out, preview, apply, bring it back.
    let shown = bob.desk.view(b_root.clone()).await.unwrap();
    assert_eq!(shown["disk"]["files"], 5, "{shown}");
    let preview = bob
        .desk
        .view_apply(b_root.clone(), vec![], strings(&["Art/**"]), false, true)
        .await
        .unwrap();
    assert_eq!(
        preview["remove"]["paths"],
        json!(["Art/harbor.png"]),
        "{preview}"
    );
    bob.desk
        .view_apply(b_root.clone(), vec![], strings(&["Art/**"]), false, false)
        .await
        .unwrap();
    assert!(!b.join("Art/harbor.png").exists());
    bob.desk
        .view_apply(b_root.clone(), vec![], vec![], false, false)
        .await
        .unwrap();
    assert!(b.join("Art/harbor.png").exists());

    // ## Links: a link Alice copies opens in Bob's workspace of the same server and depot.
    let link = Link::File {
        server: info["server"].as_str().unwrap().to_owned(),
        depot: "game".into(),
        path: "Source/boat.rs".into(),
        rev: Some(merge_rev),
        branch: None,
    };
    let text = links::build(&link).unwrap();
    assert!(text.starts_with("lode://127.0.0.1:"), "{text}");
    assert!(
        text.ends_with(&format!("/game/Source/boat.rs@{merge_rev}?http=1")),
        "{text}"
    );
    let Link::File {
        server: s,
        depot,
        path,
        rev,
        ..
    } = links::parse(&text).unwrap()
    else {
        panic!("{text} is not a file link");
    };
    let bobs = &bob.desk.recent()[0];
    assert_eq!(
        (s.as_str(), depot.as_str()),
        (bobs.server.trim_end_matches('/'), bobs.depot.as_str())
    );
    let blame = bob.desk.blame(b_root.clone(), path, rev).await.unwrap();
    assert_eq!(
        blame["lines"][0]["rev"].as_u64(),
        Some(merge_rev),
        "{blame}"
    );
    let shelf_link = links::build(&Link::Shelf {
        server: url.clone(),
        depot: "game".into(),
        id: shelf,
    })
    .unwrap();
    assert!(matches!(links::parse(&shelf_link), Ok(Link::Shelf { id, .. }) if id == shelf));
    assert!(links::parse(&format!("{text}#x")).is_err());

    // ## Updates: on by default, off by the setting; the variable for managed machines.
    let settings = alice.home.path().join("app/settings.json");
    assert!(update::load_settings(&settings).check_updates);
    update::save_settings(
        &settings,
        &Settings {
            check_updates: false,
        },
    )
    .unwrap();
    assert!(update::off_reason(&update::load_settings(&settings), None, false).is_some());
    assert!(update::off_reason(&update::load_settings(&settings), None, true).is_none());
}
