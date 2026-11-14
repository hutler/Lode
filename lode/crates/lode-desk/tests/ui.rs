//! T130 acceptance: the Slint window against test servers, driven headless through Slint's
//! testing backend. Controls are found by their accessible labels, as a screen reader would.
//!
//! The testing backend can be set up once per process, so this is one test program
//! (`harness = false`) that runs its scenarios one after another on the UI thread.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use lode_desk::app::{self, AppWindow, ChangeRow, Dialog, Options};
use lode_desk::changes::{self, FileEntry, Pending};
use lode_desk::desk::Desk;
use lode_ops::output::{ProgressEvent, Ui};
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::PrincipalKind;
use lode_server::{OidcConfig, PasswordLogin};
use lode_testkit::oidc::{browse, TestProvider};
use lode_testkit::{TestServer, TestServerOptions};
use serde_json::json;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};
use tokio::runtime::Handle;

/// The servers every scenario uses.
struct World {
    /// A depot `game` and a user `alice` (password `correct horse`) who may write it.
    _server: TestServer,
    url: String,
    head: u64,
    /// A server that offers single sign-on as "Example Corp", password sign-in for everyone.
    sso: TestServer,
    /// A server that offers single sign-on, password sign-in for administrators only.
    sso_admins: TestServer,
    _provider: TestProvider,
    rt: Handle,
}

/// One app as a user has it: a home, the app's data and lode's config.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The window, with the folder picker answering `picked` and the browser played by the test.
    fn window(&self, world: &World, picked: Option<PathBuf>) -> AppWindow {
        let rt = world.rt.clone();
        let w = app::build(Options {
            home: self.path().to_path_buf(),
            data_dir: self.path().join("app"),
            config_dir: Some(self.path().join("config")),
            pick_folder: Rc::new(move |_| picked.clone()),
            open_browser: Arc::new(move |url| {
                let url = url.to_owned();
                rt.spawn(async move { browse(&url).await });
                true
            }),
        })
        .unwrap();
        w.show().unwrap();
        w
    }
}

async fn start(rt: Handle) -> World {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let head = server
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
    add_user(&server, "alice", Some("correct horse"), None).await;
    add_user(&server, "bob", Some("battery staple"), None).await;
    server
        .acl(
            "game",
            "bob",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
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
    let url = server.url().trim_end_matches('/').to_owned();

    let provider = TestProvider::start("lode-test", "s3cret").await;
    provider.set(|b| {
        b.claims.insert("email".into(), json!("ana@example.com"));
        b.claims.insert("email_verified".into(), json!(true));
    });
    let oidc = |password_login: PasswordLogin| {
        let mut c = OidcConfig::new(provider.issuer(), "lode-test")
            .unwrap()
            .with_client_secret("s3cret");
        c.label = "Example Corp".into();
        c.password_login = password_login;
        c
    };
    let cfg = oidc(PasswordLogin::Everyone);
    let sso = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| c.oidc = Some(cfg)),
    })
    .await;
    add_user(&sso, "ana", None, Some("ana@example.com")).await;
    let cfg = oidc(PasswordLogin::Admins);
    let sso_admins = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| c.oidc = Some(cfg)),
    })
    .await;
    World {
        _server: server,
        url,
        head,
        sso,
        sso_admins,
        _provider: provider,
        rt,
    }
}

async fn add_user(s: &TestServer, name: &str, password: Option<&str>, sso: Option<&str>) {
    s.admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: name.into(),
            display_name: None,
            owner: None,
            password: password.map(str::to_owned),
            sso: sso.map(str::to_owned),
        })
        .await
        .unwrap();
}

fn all(w: &AppWindow, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(w, label).collect()
}

/// The element labelled `label` with one of `roles` (a text that only names a control is
/// skipped).
fn find_as(w: &AppWindow, label: &str, roles: &[AccessibleRole]) -> ElementHandle {
    all(w, label)
        .into_iter()
        .find(|e| e.accessible_role().is_some_and(|r| roles.contains(&r)))
        .unwrap_or_else(|| panic!("nothing labelled {label:?} as {roles:?} on screen"))
}

fn button(w: &AppWindow, label: &str) -> ElementHandle {
    find_as(
        w,
        label,
        &[
            AccessibleRole::Button,
            AccessibleRole::Tab,
            AccessibleRole::Checkbox,
            AccessibleRole::ListItem,
        ],
    )
}

/// Clicks the button labelled `label` inside the dialog labelled `dialog`.
fn click_in(w: &AppWindow, dialog: &str, label: &str) {
    let label = label.to_owned();
    let d = find_as(w, dialog, &[AccessibleRole::Groupbox]);
    let b = d
        .query_descendants()
        .match_predicate(move |e| {
            e.accessible_label().is_some_and(|l| l == label.as_str())
                && e.accessible_role() == Some(AccessibleRole::Button)
        })
        .find_first()
        .unwrap_or_else(|| panic!("no such button in {dialog}"));
    b.invoke_accessible_default_action();
}

fn rows(w: &AppWindow) -> Vec<ChangeRow> {
    w.get_change_rows().iter().collect()
}

fn row(w: &AppWindow, key: &str, group: bool) -> Option<ChangeRow> {
    rows(w)
        .into_iter()
        .find(|r| r.key == key && r.is_group == group)
}

/// The group a file is listed under.
fn group_of(w: &AppWindow, path: &str) -> Option<String> {
    let mut group = None;
    for r in rows(w) {
        if r.is_group {
            group = Some(r.key.to_string());
        } else if r.key == path {
            return group;
        }
    }
    None
}

struct Quiet;

impl Ui for Quiet {
    fn line(&self, _text: &str) {}
    fn warn(&self, _text: &str) {}
    fn progress(&self, _event: &ProgressEvent<'_>) {}
}

/// Bob, in his own app and workspace of `game`.
async fn bob(world: &World) -> (tempfile::TempDir, Desk, String) {
    let home = tempfile::tempdir().unwrap();
    let desk = Desk::new(
        home.path().to_path_buf(),
        &home.path().join("app"),
        Arc::new(Quiet),
    )
    .with_config_dir(home.path().join("config"));
    desk.sign_in(world.url.clone(), "bob".into(), "battery staple".into())
        .await
        .unwrap();
    let dir = home.path().join("game");
    desk.checkout(
        format!("{}/game", world.url),
        dir.display().to_string(),
        "main".into(),
    )
    .await
    .unwrap();
    let root = desk.recent()[0].path.clone();
    (home, desk, root)
}

fn shown(w: &AppWindow, label: &str) -> bool {
    !all(w, label).is_empty()
}

fn click(w: &AppWindow, label: &str) {
    button(w, label).invoke_accessible_default_action();
}

fn type_into(w: &AppWindow, label: &str, text: &str) {
    find_as(w, label, &[AccessibleRole::TextInput]).set_accessible_value(text);
}

/// What the input, or the text, labelled `label` shows.
fn value(w: &AppWindow, label: &str) -> String {
    find_as(w, label, &[AccessibleRole::TextInput, AccessibleRole::Text])
        .accessible_value()
        .unwrap_or_default()
        .to_string()
}

async fn sleep(ms: u64) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    slint::Timer::single_shot(Duration::from_millis(ms), move || {
        let _ = tx.send(());
    });
    let _ = rx.await;
}

/// Waits for `done`, giving the operations behind it plenty of time.
async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..1500 {
        if done() {
            return;
        }
        sleep(20).await;
    }
    panic!("timed out waiting until {what}");
}

/// Presses a key, with the modifiers held down around it.
fn press(w: &AppWindow, modifiers: &[Key], key: impl Into<slint::SharedString>) {
    let key = key.into();
    let win = w.window();
    for m in modifiers {
        win.dispatch_event(WindowEvent::KeyPressed { text: (*m).into() });
    }
    win.dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
    win.dispatch_event(WindowEvent::KeyReleased { text: key });
    for m in modifiers.iter().rev() {
        win.dispatch_event(WindowEvent::KeyReleased { text: (*m).into() });
    }
}

/// The error banner shows the message, the advice for its code, and the code.
fn assert_banner(w: &AppWindow, code: &str, message_part: &str) {
    let message = value(w, "Error");
    assert!(
        message.contains(message_part),
        "banner {message:?} should mention {message_part:?}"
    );
    let advice = app::next_step(code).unwrap();
    assert_eq!(value(w, "Next step"), advice);
}

async fn sign_in_with_a_password(world: &World) {
    let home = Home::new();
    let w = home.window(world, None);
    click(&w, "Sign in to a server…");
    assert_eq!(w.get_dialog(), Dialog::SignIn);
    assert_eq!(value(&w, "Server"), "https://");
    type_into(&w, "Server", &world.url);
    // The server is asked about single sign-on; this one offers none.
    sleep(800).await;
    assert!(!shown(&w, "Sign in with Example Corp"));
    assert!(w.get_sign_in().sso_label.is_empty());

    // A wrong password: the banner says so and what to do next.
    type_into(&w, "Name", "alice");
    type_into(&w, "Password", "wrong");
    click(&w, "Sign in");
    until("the sign-in is refused", || shown(&w, "Error")).await;
    assert!(
        value(&w, "Error").contains("password"),
        "{}",
        value(&w, "Error")
    );
    assert!(
        !shown(&w, "Next step"),
        "no advice to sign in, in the sign-in dialog"
    );
    assert_eq!(w.get_sign_in().error.code, "unauthenticated");
    assert!(!w.get_sign_in().busy);

    // The right one.
    type_into(&w, "Password", "correct horse");
    click(&w, "Sign in");
    until("signed in", || shown(&w, "Signed in")).await;
    assert_eq!(
        value(&w, "Signed in"),
        format!("Signed in to {} as alice.", world.url)
    );
    assert!(
        w.get_sign_in().password.is_empty(),
        "the password is not kept"
    );
    // The token is where `lode` keeps it.
    let stored = std::fs::read_dir(home.path().join("config"))
        .unwrap()
        .count();
    assert!(stored > 0, "lode's config holds the sign-in");
    click(&w, "Done");
    assert_eq!(w.get_dialog(), Dialog::None);
    w.hide().unwrap();
    println!("sign in with a password ... ok");
}

async fn single_sign_on_when_offered(world: &World) {
    let home = Home::new();
    let w = home.window(world, None);
    let url = world.sso.url().trim_end_matches('/').to_owned();
    click(&w, "Sign in to a server…");
    type_into(&w, "Server", &url);
    until("the single sign-on button", || {
        shown(&w, "Sign in with Example Corp")
    })
    .await;
    assert!(w.get_sign_in().password_login);

    // The test plays the browser the app opens.
    click(&w, "Sign in with Example Corp");
    assert!(w.get_sign_in().waiting);
    until("signed in through the browser", || shown(&w, "Signed in")).await;
    assert_eq!(
        value(&w, "Signed in"),
        format!("Signed in to {url} as ana.")
    );
    click(&w, "Done");

    // A server where only administrators may use a password says so.
    let admins = world.sso_admins.url().trim_end_matches('/').to_owned();
    click(&w, "Sign in to a server…");
    type_into(&w, "Server", &admins);
    until("the single sign-on button", || {
        shown(&w, "Sign in with Example Corp")
    })
    .await;
    assert!(!w.get_sign_in().password_login);

    // Editing the address to one without single sign-on takes the button away.
    type_into(&w, "Server", &world.url);
    assert!(!shown(&w, "Sign in with Example Corp"));
    sleep(800).await;
    assert!(!shown(&w, "Sign in with Example Corp"));
    press(&w, &[], Key::Escape);
    assert_eq!(w.get_dialog(), Dialog::None);
    w.hide().unwrap();
    println!("single sign-on when offered ... ok");
}

/// Signs `home` in as alice through the window.
async fn signed_in(world: &World, w: &AppWindow) {
    click(w, "Sign in to a server…");
    type_into(w, "Server", &world.url);
    type_into(w, "Name", "alice");
    type_into(w, "Password", "correct horse");
    click(w, "Sign in");
    until("signed in", || shown(w, "Signed in")).await;
    click(w, "Done");
}

async fn check_out_with_progress_then_reopen(world: &World) {
    let home = Home::new();
    let folder = home.path().join("game-ws");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;

    // Ctrl+Shift+O opens the dialog; the folder comes from the picker.
    press(&w, &[Key::Control, Key::Shift], "O");
    assert_eq!(w.get_dialog(), Dialog::CheckOut);
    assert_eq!(value(&w, "Branch"), "main");
    type_into(&w, "Depot", &format!("{}/game", world.url));
    click(&w, "Browse…");
    assert_eq!(value(&w, "Folder"), folder.to_string_lossy());
    click(&w, "Check out");
    assert!(w.get_checkout().busy);
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;

    // Progress was shown while it ran.
    let state = w.get_checkout();
    assert!(
        state.progress_text.contains(" of "),
        "{:?}",
        state.progress_text
    );
    assert!(state.progress >= 0.0);
    assert!(!state.error.shown);

    // The workspace has its tab and sidebar.
    assert_eq!(w.get_current_tab(), 1);
    // A tab named after the depot.
    button(&w, "game");
    assert_eq!(value(&w, "Branch"), "main");
    assert_eq!(value(&w, "Revision"), world.head.to_string());
    assert_eq!(value(&w, "Server"), world.url.trim_start_matches("http://"));
    assert!(folder.join("a.txt").is_file());

    // Closing the tab goes back Home, where the workspace is listed.
    click(&w, "Close game");
    assert_eq!(w.get_current_tab(), 0);
    let root = std::fs::canonicalize(&folder).unwrap();
    w.hide().unwrap();

    // A new start lists it under recent workspaces and opens it from there.
    let w = home.window(world, None);
    let recent = w.get_recent();

    assert_eq!(recent.row_count(), 1);
    let row = recent.row_data(0).unwrap();
    assert_eq!(row.depot, "game");
    assert_eq!(row.branch, "main");
    let label = format!("Open game at {}", row.path);
    assert_eq!(
        std::fs::canonicalize(row.path.as_str()).unwrap(),
        root,
        "the recent entry is the workspace root"
    );
    click(&w, &label);
    until("the recent workspace opens", || w.get_current_tab() == 1).await;
    assert_eq!(value(&w, "Revision"), world.head.to_string());

    // Opening it again selects its tab instead of adding one.
    click(&w, "Home");
    click(&w, &label);
    until("its tab is selected", || w.get_current_tab() == 1).await;
    assert_eq!(w.get_workspaces().row_count(), 1);

    // Forget takes it off the list, and leaves the folder alone.
    click(&w, "Home");
    click(&w, &format!("Forget {}", row.path));
    assert_eq!(w.get_recent().row_count(), 0);
    assert!(folder.join("a.txt").is_file());
    w.hide().unwrap();
    println!("check out with progress, then reopen ... ok");
}

async fn check_out_before_signing_in(world: &World) {
    let home = Home::new();
    let w = home.window(world, None);
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/game", world.url));
    type_into(&w, "Folder", &home.path().join("ws").to_string_lossy());
    click(&w, "Check out");
    until("the check out is refused", || shown(&w, "Error")).await;
    assert_banner(&w, "unauthenticated", "");
    assert_eq!(w.get_checkout().sign_in_server, world.url);

    // The dialog offers to sign in to that server and comes back afterwards.
    let sign_in = format!("Sign in to {}", world.url);
    click(&w, &sign_in);
    assert_eq!(w.get_dialog(), Dialog::SignIn);
    assert_eq!(value(&w, "Server"), world.url);
    type_into(&w, "Name", "alice");
    type_into(&w, "Password", "correct horse");
    click(&w, "Sign in");
    until("signed in", || shown(&w, "Signed in")).await;
    click(&w, "Done");
    assert_eq!(w.get_dialog(), Dialog::CheckOut);
    assert!(!shown(&w, "Error"), "the old error is cleared");
    assert!(!shown(&w, &sign_in));
    click(&w, "Check out");
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;
    assert_eq!(w.get_current_tab(), 1);

    // An incomplete form is refused before anything runs.
    click(&w, "Home");
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", "game");
    click(&w, "Check out");
    assert_eq!(w.get_checkout().error.code, "usage");
    assert!(!w.get_checkout().busy);
    click(&w, "Cancel");
    assert_eq!(w.get_dialog(), Dialog::None);
    w.hide().unwrap();
    println!("check out before signing in ... ok");
}

async fn changes_pick_commit_revert_and_changelists(world: &World) {
    let home = Home::new();
    let folder = home.path().join("ws");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/game", world.url));
    click(&w, "Browse…");
    click(&w, "Check out");
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;
    until("its changes load", || w.get_changes().loaded).await;
    assert_eq!(w.get_changes().total, 0);
    let root = std::fs::canonicalize(&folder).unwrap();

    // Edits show up by themselves: the app watches the folder.
    std::fs::write(root.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(root.join("notes.md"), "todo\n").unwrap();
    until("the edits show", || w.get_changes().total == 2).await;
    let a = row(&w, "a.txt", false).unwrap();
    assert_eq!(
        (a.status.as_str(), a.file_kind.as_str()),
        ("modified", "text")
    );
    assert_eq!(group_of(&w, "a.txt").as_deref(), Some("default"));
    assert_eq!(
        group_of(&w, "notes.md").as_deref(),
        Some(changes::NEW_FILES)
    );

    // Pick one file and commit it.
    click(&w, "Pick a.txt");
    assert_eq!(w.get_changes().picked, 1);
    assert!(row(&w, "a.txt", false).unwrap().picked);
    type_into(&w, "Commit message", "Apricot instead");
    click(&w, "Commit 1 file");
    until("the commit is done", || {
        value_or_empty(&w, "Done").starts_with("Committed r")
    })
    .await;
    assert_eq!(
        value(&w, "Done"),
        format!("Committed r{} (1 file).", world.head + 1)
    );
    assert!(w.get_commit_message().is_empty(), "the message is cleared");
    until("the list is loaded again", || w.get_changes().total == 1).await;
    assert_eq!(w.get_changes().picked, 0);

    // The message is remembered.
    click(&w, "Recent messages…");
    click(&w, "Use message Apricot instead");
    assert_eq!(w.get_commit_message(), "Apricot instead");
    assert_eq!(w.get_dialog(), Dialog::None);
    w.set_commit_message("".into());

    // Add a new file from the details panel.
    click(&w, "notes.md");
    assert_eq!(value(&w, "Status"), "New, not added");
    click(&w, "Add");
    until("the file is added", || {
        row(&w, "notes.md", false).is_some_and(|r| r.status == "added")
    })
    .await;

    // A new changelist takes the picked file, then is renamed and committed.
    std::fs::write(root.join("d/c.txt"), "cranberry\n").unwrap();
    until("the edit shows", || row(&w, "d/c.txt", false).is_some()).await;
    click(&w, "Pick d/c.txt");
    click(&w, "New changelist…");
    type_into(&w, "Name", "two words");
    click_in(&w, "New changelist", "Create");
    assert_eq!(
        w.get_list_dialog().error.code,
        "usage",
        "a bad name is refused"
    );
    type_into(&w, "Name", "ui");
    type_into(&w, "Description", "Cranberry");
    click_in(&w, "New changelist", "Create");
    until("the changelist shows", || {
        group_of(&w, "d/c.txt").as_deref() == Some("ui")
    })
    .await;
    assert_eq!(w.get_dialog(), Dialog::None);
    assert_eq!(row(&w, "ui", true).unwrap().description, "Cranberry");

    click(&w, "Rename changelist ui");
    type_into(&w, "New name", "fruit");
    click_in(&w, "Rename changelist", "Rename");
    until("it is renamed", || {
        group_of(&w, "d/c.txt").as_deref() == Some("fruit")
    })
    .await;
    assert!(row(&w, "ui", true).is_none());
    assert_eq!(row(&w, "fruit", true).unwrap().description, "Cranberry");

    click(&w, "Commit changelist fruit");
    assert_eq!(
        value(&w, "Message"),
        "Cranberry",
        "the description is the message"
    );
    click_in(&w, "Commit changelist", "Commit 1 file");
    until("the changelist is committed", || {
        value_or_empty(&w, "Done").ends_with("from fruit.")
    })
    .await;
    until("it leaves the list", || row(&w, "d/c.txt", false).is_none()).await;

    // Revert asks first.
    std::fs::write(root.join("b.txt"), "blueberry\n").unwrap();
    until("the edit shows", || row(&w, "b.txt", false).is_some()).await;
    click(&w, "b.txt");
    click(&w, "Revert…");
    assert_eq!(w.get_dialog(), Dialog::Revert);
    assert!(value(&w, "Question").contains("b.txt"));
    click_in(&w, "Revert changes?", "Revert");
    until("the file is back", || row(&w, "b.txt", false).is_none()).await;
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "banana\n"
    );

    // Move to a changelist, then delete the changelist: the file goes back to Changes.
    std::fs::write(root.join("b.txt"), "blackberry\n").unwrap();
    until("the edit shows", || row(&w, "b.txt", false).is_some()).await;
    click(&w, "New changelist…");
    type_into(&w, "Name", "later");
    click_in(&w, "New changelist", "Create");
    until("the empty changelist shows", || {
        row(&w, "later", true).is_some()
    })
    .await;
    click(&w, "b.txt");
    click(&w, "Move to changelist");
    click(&w, "Move to later");
    until("the file moves", || {
        group_of(&w, "b.txt").as_deref() == Some("later")
    })
    .await;
    click(&w, "Delete changelist later");
    assert!(value(&w, "Question").contains("1 file goes back"));
    click_in(&w, "Delete changelist later?", "Delete");
    until("the changelist goes", || row(&w, "later", true).is_none()).await;
    assert_eq!(group_of(&w, "b.txt").as_deref(), Some("default"));

    // A commit the server refuses names the files: Bob holds the lock on b.txt.
    let (_bob_home, bob, bob_root) = bob(world).await;
    bob.lock(bob_root.clone(), vec!["b.txt".into()], None)
        .await
        .unwrap();
    click(&w, "Pick b.txt");
    type_into(&w, "Commit message", "Blackberry");
    click(&w, "Commit 1 file");
    until("the commit is refused", || w.get_changes().error.shown).await;
    assert_eq!(w.get_changes().error.code, "locked");
    assert_eq!(value(&w, "Files the server refused"), "b.txt");
    assert_eq!(
        w.get_commit_message(),
        "Blackberry",
        "the message stays for another try"
    );
    click(&w, "Dismiss");
    assert!(!w.get_changes().error.shown);
    bob.unlock(bob_root.clone(), vec!["b.txt".into()], false)
        .await
        .unwrap();
    click(&w, "b.txt");
    click(&w, "Revert…");
    click_in(&w, "Revert changes?", "Revert");
    until("the file is back", || row(&w, "b.txt", false).is_none()).await;
    w.set_commit_message("".into());

    // A conflict stops the commit and says why.
    std::fs::write(Path::new(&bob_root).join("a.txt"), "avocado\n").unwrap();
    bob.commit(bob_root.clone(), vec!["a.txt".into()], "Avocado".into())
        .await
        .unwrap();
    std::fs::write(root.join("a.txt"), "almond\n").unwrap();
    let alice = Desk::new(
        home.path().to_path_buf(),
        &home.path().join("app"),
        Arc::new(Quiet),
    )
    .with_config_dir(home.path().join("config"));
    let _ = alice.update(root.display().to_string(), None).await;
    click(&w, "Refresh");
    until("the conflict shows", || w.get_changes().conflicted == 1).await;
    assert_eq!(row(&w, "a.txt", false).unwrap().status, "conflicted");
    assert_eq!(
        value(&w, "Why commit is off"),
        "Resolve the conflicted file before you commit."
    );
    click(&w, "Pick a.txt");
    type_into(&w, "Commit message", "Almond");
    assert_eq!(
        button(&w, "Commit 1 file").accessible_enabled(),
        Some(false),
        "commit is off while a conflict remains"
    );
    w.hide().unwrap();
    println!("changes: pick, commit, revert and changelists ... ok");
}

/// What the text labelled `label` says, or nothing when it is not on screen.
fn value_or_empty(w: &AppWindow, label: &str) -> String {
    all(w, label)
        .into_iter()
        .find(|e| e.accessible_role() == Some(AccessibleRole::Text))
        .and_then(|e| e.accessible_value())
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// 20 000 changed files become rows and reach the window in under a second.
async fn twenty_thousand_rows(world: &World) {
    let home = Home::new();
    let w = home.window(world, None);
    let pending = Pending {
        branch: "main".into(),
        rev: Some(1),
        files: (0..20_000)
            .map(|i| FileEntry {
                path: format!("Content/Level{:02}/Mesh_{i:05}.uasset", i % 40),
                status: if i % 7 == 0 { "added" } else { "modified" }.into(),
                changelist: (i % 3 == 0).then(|| "art".to_owned()),
                copy_from: None,
                moved: false,
                locked: false,
                needs_lock: false,
                lock_required: false,
            })
            .collect(),
        lists: Vec::new(),
        merges: Vec::new(),
    };
    let picked = std::collections::BTreeSet::new();
    let started = std::time::Instant::now();
    let r = changes::rows(&pending, &picked, None);
    w.set_change_rows(slint::ModelRc::new(slint::VecModel::from(r)));
    let took = started.elapsed();
    assert_eq!(w.get_change_rows().row_count(), 20_000 + 2);
    assert!(took < Duration::from_secs(1), "took {took:?}");
    w.hide().unwrap();
    println!("20 000 rows in {took:?} ... ok");
}

async fn opening_a_folder_that_is_not_a_workspace(world: &World) {
    let home = Home::new();
    let plain = home.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let w = home.window(world, Some(plain));
    // Ctrl+O asks for a folder.
    press(&w, &[Key::Control], "o");
    until("the error shows", || shown(&w, "Error")).await;
    assert_banner(&w, "not_a_workspace", "");
    assert_eq!(w.get_home_error().code, "not_a_workspace");
    assert_eq!(w.get_workspaces().row_count(), 0);
    click(&w, "Dismiss");
    assert!(!shown(&w, "Error"));

    // About names Slint and its licence.
    click(&w, "About Lode Desk");
    assert_eq!(w.get_dialog(), Dialog::About);
    click(&w, "Close");
    assert_eq!(w.get_dialog(), Dialog::None);
    w.hide().unwrap();
    println!("opening a folder that is not a workspace ... ok");
}

fn main() -> std::process::ExitCode {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let world = rt.block_on(start(rt.handle().clone()));
    slint::spawn_local(async move {
        sign_in_with_a_password(&world).await;
        single_sign_on_when_offered(&world).await;
        check_out_with_progress_then_reopen(&world).await;
        check_out_before_signing_in(&world).await;
        opening_a_folder_that_is_not_a_workspace(&world).await;
        changes_pick_commit_revert_and_changelists(&world).await;
        twenty_thousand_rows(&world).await;
        let _ = slint::quit_event_loop();
    })
    .unwrap();
    slint::run_event_loop_until_quit().unwrap();
    println!("ui: all scenarios passed");
    std::process::ExitCode::SUCCESS
}
