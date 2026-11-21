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
use lode_desk::app::{
    self, AppWindow, BlameLine, BranchRow, Branching, ChangeRow, CheckRow, Dialog, DiffRow,
    FolderRow, Links, LockRow, Options, Pane, Reading, RevRow, ShelfRow, Span, Team, Updates,
};
use lode_desk::changes::{self, FileEntry, Pending};
use lode_desk::desk::Desk;
use lode_desk::links::{self, Link};
use lode_desk::update::Updater;
use lode_ops::output::{ProgressEvent, Ui};
use lode_proto::admin::{AccessLevel, AclEffect, CreatePrincipalRequest};
use lode_proto::checks::{CheckState, CheckTarget, CheckTargetKind, ReportCheckRequest};
use lode_proto::depot::CreateBranchRequest;
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
    /// A depot `game` and a user `alice` (password `correct horse`) who may write it. The
    /// checks module is on.
    server: TestServer,
    url: String,
    head: u64,
    /// Depot `story`: more than a page of history, a text file and an image changed twice.
    story: Story,
    /// A token of user `ci`, who reports checks on depot `harbor`.
    ci: String,
    /// A server that offers single sign-on as "Example Corp", password sign-in for everyone.
    sso: TestServer,
    /// A server that offers single sign-on, password sign-in for administrators only.
    sso_admins: TestServer,
    _provider: TestProvider,
    rt: Handle,
}

/// The revisions of depot `story`.
struct Story {
    first: u64,
    head: u64,
    total: usize,
}

/// A 4×4 PNG of one color.
fn png_of(rgb: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 4, 4);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..16).flat_map(|_| rgb).collect();
        enc.write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
    out
}

async fn story(server: &TestServer) -> Story {
    server.create_depot("story").await;
    let red = png_of([200, 30, 30]);
    let first = server
        .commit_files(
            "story",
            "main",
            &[
                ("notes.txt", &b"line one\nspeed = 10\nend\n"[..]),
                ("pic.png", &red[..]),
            ],
        )
        .await;
    for i in 0..104 {
        let text = format!("entry {i}\n");
        server
            .commit_files("story", "main", &[("log.txt", text.as_bytes())])
            .await;
    }
    let blue = png_of([30, 30, 200]);
    let head = server
        .commit_files(
            "story",
            "main",
            &[
                ("notes.txt", &b"line one\nspeed = 12\nend\n"[..]),
                ("pic.png", &blue[..]),
            ],
        )
        .await;
    // The depot's own first revision, then the ones above.
    let total = usize::try_from(head).unwrap();
    Story { first, head, total }
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
        // A build without an update key: it never looks for updates.
        let updater = Updater {
            pubkey: String::new(),
            feed: String::new(),
            current: "0.1.0".into(),
            platforms: Vec::new(),
            settings_file: self.path().join("app/settings.json"),
        };
        self.window_with(world, picked, updater)
    }

    /// The window, looking for updates as `updater` says.
    fn window_with(&self, world: &World, picked: Option<PathBuf>, updater: Updater) -> AppWindow {
        let rt = world.rt.clone();
        let w = app::build(Options {
            updater,
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
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
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
    let story = story(&server).await;
    add_user(&server, "alice", Some("correct horse"), None).await;
    add_user(&server, "bob", Some("battery staple"), None).await;
    server.create_depot("artist").await;
    server
        .commit_files(
            "artist",
            "main",
            &[
                ("Content/Art/BigRock.uasset", &rock_package()),
                ("Content/Art/Unknown.uasset", b"no thumbnail"),
                (".lodeconfig", b"[locks]\nrequired = [\"**/*.uasset\"]\n"),
            ],
        )
        .await;
    for who in ["alice", "bob"] {
        server
            .acl(
                "artist",
                who,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
    }
    server.create_depot("validation").await;
    server
        .commit_files(
            "validation",
            "main",
            &[
                ("a.txt", b"one\ntwo\n"),
                (
                    ".lodeconfig",
                    br#"[[validate.script]]
name = "text"
run = "check.py"
paths = ["*.txt"]
"#,
                ),
                (
                    "check.py",
                    br#"import json, sys
from pathlib import Path
data = json.load(sys.stdin)
for f in data["files"]:
    if "BAD" in Path(f["path"]).read_text():
        print(json.dumps({"level":"error","message":"Remove BAD","path":f["path"],"line":2}))
        sys.exit(1)
"#,
                ),
            ],
        )
        .await;
    server
        .acl(
            "validation",
            "alice",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
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
    server
        .acl(
            "story",
            "alice",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    // Depot `harbor`: shelf submits to main need ci/build and ci/test from `ci`.
    server.create_depot("harbor").await;
    server
        .commit_files(
            "harbor",
            "main",
            &[
                ("a.txt", b"apple\n"),
                ("door.txt", b"oak\n"),
                (
                    ".lodeconfig",
                    br#"[checks]
branches = ["main"]
required = [{ name = "ci/build", reporter = "ci" }, { name = "ci/test", reporter = "ci" }]
"#,
                ),
            ],
        )
        .await;
    for who in ["alice", "bob"] {
        server
            .acl(
                "harbor",
                who,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
    }
    let ci = server.create_user("ci", &["read", "checks"]).await;
    server
        .acl(
            "harbor",
            "ci",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    // Depot `forks`: branch `dev` changes four files main changes too (three text, one binary)
    // and adds one; branch `feature` only adds a file. Two folders for the view.
    server.create_depot("forks").await;
    server
        .commit_files(
            "forks",
            "main",
            &[
                ("a.txt", b"apple\n"),
                ("b.txt", b"one\ntwo\nthree\n"),
                ("c.txt", b"c\n"),
                ("d.txt", b"d\n"),
                ("pic.bin", &[0, 1, 0, 1]),
                ("Content/Maps/harbor.txt", b"map\n"),
                ("Content/Cine/intro.txt", b"intro\n"),
            ],
        )
        .await;
    for name in ["dev", "feature"] {
        server
            .admin()
            .create_branch(
                "forks",
                &CreateBranchRequest {
                    name: name.into(),
                    from_branch: "main".into(),
                    from_rev: None,
                    stream: None,
                },
            )
            .await
            .unwrap();
    }
    server
        .commit_files(
            "forks",
            "dev",
            &[
                ("b.txt", b"one\nTWO dev\nthree\n"),
                ("c.txt", b"c dev\n"),
                ("d.txt", b"d dev\n"),
                ("pic.bin", &[0, 2, 0, 2]),
                ("f.txt", b"new on dev\n"),
            ],
        )
        .await;
    server
        .commit_files("forks", "feature", &[("g.txt", b"from feature\n")])
        .await;
    server
        .commit_files(
            "forks",
            "main",
            &[
                ("b.txt", b"one\nTWO main\nthree\n"),
                ("c.txt", b"c main\n"),
                ("d.txt", b"d main\n"),
                ("pic.bin", &[0, 3, 0, 3]),
            ],
        )
        .await;
    server
        .acl(
            "forks",
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
        server,
        url,
        head,
        story,
        ci,
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
    bob_on(world, "game").await
}

/// Bob, in his own app and workspace of `depot`.
async fn bob_on(world: &World, depot: &str) -> (tempfile::TempDir, Desk, String) {
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
    let dir = home.path().join(depot);
    desk.checkout(
        format!("{}/{depot}", world.url),
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
    until("the server check", || w.get_sign_in().probe == "ok").await;
    assert!(value(&w, "Server check").contains("Lode server"));

    // An address with a path: the dialog points at the server's own address.
    type_into(&w, "Server", &format!("{}/admin/", world.url));
    until("the address is refused", || w.get_sign_in().probe == "bad").await;
    let check = value(&w, "Server check");
    assert!(check.contains(&world.url), "{check}");
    assert_eq!(w.get_sign_in().suggested, world.url.as_str());
    click(&w, &format!("Use {}", world.url));
    assert_eq!(value(&w, "Server"), world.url);
    until("the server check", || w.get_sign_in().probe == "ok").await;

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
    // The advice is about the password, not about signing in somewhere else.
    let advice = value(&w, "Next step");
    assert!(advice.contains("password"), "{advice}");
    assert!(!advice.contains("Sign in to a server"), "{advice}");
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

async fn desktop_menus(world: &World) {
    let home = Home::new();
    let opened = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = opened.clone();
    let w = app::build(Options {
        home: home.path().into(),
        data_dir: home.path().join("app"),
        config_dir: Some(home.path().join("config")),
        pick_folder: Rc::new(|_| None),
        open_browser: Arc::new(move |url| {
            captured.lock().unwrap().push(url.to_owned());
            true
        }),
        updater: Updater {
            pubkey: String::new(),
            feed: String::new(),
            current: "0.5.0".into(),
            platforms: Vec::new(),
            settings_file: home.path().join("app/settings.json"),
        },
    })
    .unwrap();
    w.show().unwrap();
    let c = w.global::<lode_desk::app::Connection>();
    c.invoke_open();
    assert_eq!(w.get_dialog(), Dialog::Connection);
    type_into(&w, "Connection server URL", &world.url);
    click(&w, "Test connection");
    until("server health report", || {
        !c.get_busy() && c.get_report().contains("dns")
    })
    .await;
    click(&w, "Sign in…");
    assert_eq!(w.get_dialog(), Dialog::SignIn);
    assert_eq!(w.get_sign_in().server, world.url.as_str());
    w.invoke_close_dialog();
    w.invoke_guide();
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://github.com/hutler/Lode/blob/main/desk/docs/guide.md"]
    );
    w.invoke_exit_requested();
    assert!(!w.window().is_visible());
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

    // Closing the tab goes back to Workspaces, where the workspace is listed.
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
    click(&w, "Workspaces");
    click(&w, &label);
    until("its tab is selected", || w.get_current_tab() == 1).await;
    assert_eq!(w.get_workspaces().row_count(), 1);

    // Forget takes it off the list, and leaves the folder alone.
    click(&w, "Workspaces");
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
    click(&w, "Workspaces");
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
async fn content_validation(world: &World) {
    let home = Home::new();
    let roots = with_workspaces(world, &home, &[("validation", "verify")]).await;
    let root = Path::new(&roots[0]);
    let file = root.join("a.txt");
    let w = home.window(world, None);
    let v = w.global::<app::Validation>();
    w.invoke_open_recent(roots[0].as_str().into());
    std::fs::write(&file, "one\nBAD\n").unwrap();
    until("tracked edit visible", || row(&w, "a.txt", false).is_some()).await;
    click(&w, "Pick a.txt");
    type_into(&w, "Commit message", "Change text");
    click(&w, "Check changes");
    until("trust question", || v.get_stage() == "trust").await;
    assert!(v.get_detail().contains("check.py"));
    click(&w, "Show script");
    assert!(v.get_preview().contains("json.load"));
    click(&w, "Don’t run");
    until("refused script left edits alone", || {
        w.get_dialog() == Dialog::None
    })
    .await;
    click(&w, "Commit 1 file");
    until("trust on commit", || v.get_stage() == "trust").await;
    click(&w, "Run");
    until("per-file results", || v.get_stage() == "results").await;
    assert!(v
        .get_rows()
        .iter()
        .any(|r| r.kind == "script" && r.label == "text · Failed"));
    assert!(v.get_allow_commit() && v.get_allow_shelve());
    click(&w, "a.txt Line 2 · error: Remove BAD");
    let reading = w.global::<Reading>();
    until("problem opens its diff row", || {
        w.get_changes().details.path == "a.txt" && reading.get_diff().focus_row > 0
    })
    .await;
    let at = (reading.get_diff().focus_row - 1) as usize;
    let target = reading.get_diff_rows().row_data(at).unwrap();
    assert!(target.new_num == "2" || target.right.num == "2");

    click(&w, "Check changes");
    until("manual check refused", || {
        w.get_dialog() == Dialog::Validation && v.get_stage() == "results"
    })
    .await;
    std::fs::write(&file, "one\ngood\n").unwrap();
    click(&w, "Fix and check again");
    until("fixed check passes", || {
        v.get_rows().iter().any(|r| r.label == "text · Passed")
    })
    .await;
    click(&w, "Close");
    let config_path = root.join(".lodeconfig");
    let config_text = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        &config_path,
        format!("[validate]\npython = \"3.999\"\n{config_text}"),
    )
    .unwrap();
    click(&w, "Check changes");
    until("missing Python skips check", || {
        v.get_rows().iter().any(|r| r.label == "text · Skipped") && v.get_python_seen()
    })
    .await;
    click(&w, "Close");
    assert!(v.get_python_warning().contains("3.999"));
    click(&w, "Dismiss");
    click(&w, "Check changes");
    until("second check finishes", || {
        v.get_rows().iter().any(|r| r.label == "text · Skipped")
    })
    .await;
    click(&w, "Close");
    assert!(v.get_python_warning().is_empty());
    std::fs::write(config_path, config_text).unwrap();
    std::fs::write(&file, "one\nBAD again\n").unwrap();
    click(&w, "Commit 1 file");
    until("commit refused again", || {
        w.get_dialog() == Dialog::Validation && v.get_stage() == "results"
    })
    .await;
    click(&w, "Shelve anyway");
    until("unverified shelf created", || {
        value_or_empty(&w, "Done").starts_with("Shelved as shelf")
    })
    .await;
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("BAD again"));
    click(&w, "Commit 1 file");
    until("commit bypass offered", || {
        w.get_dialog() == Dialog::Validation && v.get_stage() == "results" && v.get_allow_commit()
    })
    .await;
    click(&w, "Commit anyway");
    until("unverified commit created", || {
        value_or_empty(&w, "Done").starts_with("Committed r")
    })
    .await;

    let config = root.join(".lodeconfig");
    let mut settings = std::fs::read_to_string(&config).unwrap();
    settings.push_str("\n[branches]\nshelf_only = [\"main\"]\n");
    std::fs::write(config, settings).unwrap();
    std::fs::write(&file, "one\nlast edit\n").unwrap();
    until("new edit after commit", || {
        row(&w, "a.txt", false).is_some_and(|r| !r.picked)
    })
    .await;
    click(&w, "Pick a.txt");
    type_into(&w, "Commit message", "Shelf-only edit");
    click(&w, "Commit 1 file");
    until("shelf-only action", || v.get_shelf_required()).await;
    assert!(!v.get_allow_commit());
    click(&w, "Shelve instead");
    until("normal validated shelf on protected branch", || {
        value_or_empty(&w, "Done").starts_with("Shelved as shelf")
    })
    .await;
}

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

fn revs(w: &AppWindow) -> Vec<RevRow> {
    w.global::<Reading>().get_revs().iter().collect()
}

fn diff_rows(w: &AppWindow) -> Vec<DiffRow> {
    w.global::<Reading>().get_diff_rows().iter().collect()
}

fn has_image(w: &AppWindow, label: &str) -> bool {
    all(w, label)
        .iter()
        .any(|e| e.accessible_role() == Some(AccessibleRole::Image))
}

async fn history_blame_and_diffs(world: &World) {
    let s = &world.story;
    let home = Home::new();
    let folder = home.path().join("story");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/story", world.url));
    click(&w, "Browse…");
    click(&w, "Check out");
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;
    until("its changes load", || w.get_changes().loaded).await;
    let r = w.global::<Reading>();

    // The branch's history, a page at a time.
    click(&w, "History");
    assert_eq!(r.get_pane(), Pane::History);
    until("the first page loads", || !r.get_history().loading).await;
    assert_eq!(revs(&w).len(), 100);
    assert!(r.get_history().more);
    assert_eq!(r.get_history().title, "History of main");
    let newest = &revs(&w)[0];
    assert_eq!(newest.rev, i32::try_from(s.head).unwrap());
    assert!(newest.selected && newest.branch_tag == "main" && !newest.line_above);
    until("the newest revision's details", || r.get_detail().shown).await;
    assert_eq!(r.get_detail().title, format!("r{}", s.head));
    assert_eq!(value(&w, "Message"), newest.summary.as_str());
    click(&w, "Load older revisions");
    until("the next page loads", || revs(&w).len() == s.total).await;
    until("no more pages", || !r.get_history().loading).await;
    assert!(!r.get_history().more);
    assert!(
        !revs(&w).last().unwrap().line_below,
        "the first revision ends the line"
    );

    // Selecting a revision, with the mouse and with the arrow keys.
    let second = revs(&w)[1].clone();
    click(&w, &format!("{} {}", second.label, second.summary));
    until("its details", || r.get_detail().rev == second.rev).await;
    assert!(revs(&w)[1].selected && !revs(&w)[0].selected);
    r.invoke_step_rev(-1);
    until("the newer one's details", || {
        r.get_detail().rev == newest.rev
    })
    .await;
    let files: Vec<String> = r.get_files().iter().map(|f| f.path.to_string()).collect();
    assert_eq!(files, ["notes.txt", "pic.png"]);

    // A text diff with the changed words marked.
    click(&w, "Diff of notes.txt");
    assert_eq!(r.get_pane(), Pane::Diff);
    until("the diff loads", || !r.get_diff().loading).await;
    assert_eq!(value(&w, "Compared"), format!("notes.txt in r{}", s.head));
    assert_eq!((r.get_diff().insertions, r.get_diff().deletions), (1, 1));
    r.invoke_set_layout("unified".into());
    let rows = diff_rows(&w);
    assert!(rows[0].hunk);
    let added = rows.iter().find(|r| r.kind == "add").unwrap();
    assert_eq!(added.text, "speed = 12");
    let marks: Vec<Span> = added.spans.iter().collect();
    assert_eq!(marks, [Span { start: 8, len: 2 }]);
    click(&w, "Side by side");
    assert_eq!(r.get_diff().layout, "split");
    let pair = diff_rows(&w)
        .into_iter()
        .find(|r| r.left.kind == "delete")
        .unwrap();
    assert_eq!(
        (pair.left.text.as_str(), pair.right.text.as_str()),
        ("speed = 10", "speed = 12")
    );
    assert_eq!(
        pair.right.spans.iter().collect::<Vec<_>>(),
        [Span { start: 8, len: 2 }]
    );
    let prefs = std::fs::read_to_string(home.path().join("app/prefs.json")).unwrap();
    assert!(prefs.contains("\"split\""), "the layout is kept: {prefs}");
    click(&w, "Back to History");
    assert_eq!(r.get_pane(), Pane::History);
    until("history again", || r.get_detail().rev == newest.rev).await;

    // Blame of the file, and a revision from it.
    click(&w, "Blame notes.txt");
    assert_eq!(r.get_pane(), Pane::Blame);
    until("blame loads", || !r.get_blame().loading).await;
    let lines: Vec<BlameLine> = r.get_blame_lines().iter().collect();
    let by: Vec<i32> = lines.iter().map(|l| l.rev).collect();
    let (first, head) = (
        i32::try_from(s.first).unwrap(),
        i32::try_from(s.head).unwrap(),
    );
    assert_eq!(by, [first, head, first]);
    assert!(lines.iter().all(|l| l.start), "three runs of one line");
    assert_eq!(lines[1].text, "speed = 12");
    click(&w, &format!("Revision r{first}"));
    assert_eq!(r.get_pane(), Pane::History);
    until("that revision's details", || r.get_detail().rev == first).await;

    // A file's own history.
    click(&w, "History of notes.txt");
    until("the file's history", || {
        !r.get_history().loading && r.get_history().path == "notes.txt"
    })
    .await;
    assert_eq!(
        revs(&w).iter().map(|r| r.rev).collect::<Vec<_>>(),
        [head, first]
    );
    click(&w, "Branch history");
    until("the branch again", || {
        !r.get_history().loading && revs(&w).len() == 100
    })
    .await;

    // An image diff in each mode.
    until("the newest revision's details", || {
        r.get_detail().rev == head
    })
    .await;
    click(&w, "Diff of pic.png");
    until("the images load", || {
        let d = r.get_diff();
        !d.loading && d.image && !r.get_image().loading
    })
    .await;
    let img = r.get_image();
    assert!(img.both && img.mode == "side");
    assert!(
        img.old_caption.starts_with("Before   4 × 4"),
        "{}",
        img.old_caption
    );
    assert!(has_image(&w, "Before") && has_image(&w, "After"));
    click(&w, "Swipe");
    assert_eq!(r.get_image().mode, "swipe");
    assert!(shown(&w, "Swipe position"));
    assert!(has_image(&w, "Before") && has_image(&w, "After"));
    r.set_image_pos(30.0);
    click(&w, "Onion skin");
    assert_eq!(r.get_image().mode, "onion");
    assert!(shown(&w, "Opacity of after"));
    assert!(has_image(&w, "Before") && has_image(&w, "After"));
    let prefs = std::fs::read_to_string(home.path().join("app/prefs.json")).unwrap();
    assert!(prefs.contains("\"onion\""), "the mode is kept: {prefs}");
    click(&w, "Side by side");
    assert_eq!(r.get_image().mode, "side");

    // The working copy's diff, from Changes.
    let root = std::fs::canonicalize(&folder).unwrap();
    click(&w, "Changes");
    assert_eq!(r.get_pane(), Pane::Changes);
    std::fs::write(root.join("notes.txt"), "line one\nspeed = 14\nend\n").unwrap();
    until("the edit shows", || w.get_changes().total == 1).await;
    click(&w, "notes.txt");
    click(&w, "Diff of notes.txt");
    until("the working diff loads", || {
        r.get_pane() == Pane::Diff && !r.get_diff().loading
    })
    .await;
    assert_eq!(value(&w, "Compared"), "notes.txt (working copy)");
    assert!(diff_rows(&w).iter().any(|r| r.right.text == "speed = 14"));
    click(&w, "Back to Changes");
    assert_eq!(r.get_pane(), Pane::Changes);
    w.hide().unwrap();
    println!("history, blame and diffs ... ok");
}

fn lock_rows(w: &AppWindow) -> Vec<LockRow> {
    w.global::<Team>().get_lock_rows().iter().collect()
}

fn shelf_rows(w: &AppWindow) -> Vec<ShelfRow> {
    w.global::<Team>().get_shelf_rows().iter().collect()
}

fn checks_shown(rows: impl Model<Data = CheckRow>) -> Vec<(String, String)> {
    rows.iter()
        .map(|c| (c.name.to_string(), c.word.to_string()))
        .collect()
}

/// `ci` reports `name` as `state` on `target` of depot `harbor` (on the tokio runtime, which
/// the client needs).
async fn report(world: &World, target: CheckTarget, name: &str, state: CheckState) {
    let client = world.server.client(&world.ci);
    let req = ReportCheckRequest {
        target,
        name: name.into(),
        state,
        summary: None,
        url: Some("https://ci.example.com/1".into()),
    };
    world
        .rt
        .spawn(async move { client.report_check("harbor", &req).await })
        .await
        .unwrap()
        .unwrap();
}

// A versioned UE4 editor package prefix with a named FObjectThumbnail. The browser
// needs only the summary, thumbnail record and table. Export data is never read.
fn rock_package() -> Vec<u8> {
    let png = png_of([143, 123, 101]);
    let mut bytes = Vec::new();
    let int = |b: &mut Vec<u8>, n: i32| b.extend(n.to_le_bytes());
    for n in [0x9e2a83c1u32 as i32, -7, 864, 522, 0, 0, 0, 0, 0] {
        int(&mut bytes, n);
    }
    // Name map, localization string, text/export/import tables, dependencies,
    // soft references, searchable names and thumbnail table offset.
    for _ in 0..13 {
        int(&mut bytes, 0);
    }
    let slot = bytes.len();
    int(&mut bytes, 0);
    let thumb = bytes.len() as i32;
    for n in [4, 4, png.len() as i32] {
        int(&mut bytes, n);
    }
    bytes.extend(png);
    let table = bytes.len() as i32;
    bytes[slot..slot + 4].copy_from_slice(&table.to_le_bytes());
    int(&mut bytes, 1);
    for s in ["StaticMesh", "BigRock"] {
        int(&mut bytes, s.len() as i32 + 1);
        bytes.extend(s.as_bytes());
        bytes.push(0);
    }
    int(&mut bytes, thumb);
    bytes
}

async fn artist_browser(world: &World) {
    let home = Home::new();
    let folder = home.path().join("artist");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/artist", world.url));
    click(&w, "Browse…");
    click(&w, "Check out");
    until("artist workspace opens", || {
        w.get_dialog() == Dialog::None && w.get_changes().loaded
    })
    .await;
    let root = "Content/Art/BigRock.uasset";
    let (_other, bob, bob_root) = bob_on(world, "artist").await;
    bob.lock(
        bob_root.clone(),
        vec![root.into()],
        Some("silhouette pass".into()),
    )
    .await
    .unwrap();
    assert!(folder
        .join(root)
        .metadata()
        .unwrap()
        .permissions()
        .readonly());
    assert_eq!(w.global::<Reading>().get_pane(), Pane::Changes);
    let ui = w.global::<lode_desk::app::Assets>();
    for label in [
        "Browse depot root",
        "Browse folder Content",
        "Browse folder Content/Art",
    ] {
        until(label, || shown(&w, label) && !ui.get_busy()).await;
        click(&w, label);
    }
    let select = "Inspect Content/Art/BigRock.uasset";
    until("gallery loads", || shown(&w, select) && !ui.get_busy()).await;
    click(&w, select);
    let card = ui.get_selected();
    assert!(card.preview);
    assert_eq!(card.holder, "bob");
    assert_eq!(card.comment, "silhouette pass");
    assert!(!card.workspace.is_empty());
    assert!(!card.mine);
    click(&w, "Inspect Content/Art/Unknown.uasset");
    assert!(!ui.get_selected().preview);
    click(&w, select);
    click(&w, "Watch lock");
    assert!(ui.get_watching());
    bob.unlock(bob_root, vec![root.into()], false)
        .await
        .unwrap();
    until("released lock is visible", || {
        !ui.get_busy() && ui.get_selected().path == root && ui.get_selected().holder.is_empty()
    })
    .await;
    click(&w, "Lock for editing");
    type_into(&w, "Comment", "rock pass");
    click_in(&w, "Lock", "Lock");
    until("real lock and writable copy", || {
        ui.get_selected().mine && ui.get_selected().writable
    })
    .await;
    assert!(!folder
        .join(root)
        .metadata()
        .unwrap()
        .permissions()
        .readonly());
    click(&w, "Release my lock");
    until("unlock restores file protection", || {
        !ui.get_busy() && !ui.get_selected().mine && !ui.get_selected().writable
    })
    .await;
    assert!(folder
        .join(root)
        .metadata()
        .unwrap()
        .permissions()
        .readonly());
    w.hide().unwrap();
}

async fn locks_shelves_and_checks(world: &World) {
    let home = Home::new();
    let folder = home.path().join("harbor");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/harbor", world.url));
    click(&w, "Browse…");
    click(&w, "Check out");
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;
    until("its changes load", || w.get_changes().loaded).await;
    let root = std::fs::canonicalize(&folder).unwrap();
    let t = w.global::<Team>();
    let r = w.global::<Reading>();

    // Bob holds door.txt.
    let (_bob_home, bob, bob_root) = bob_on(world, "harbor").await;
    bob.lock(bob_root, vec!["door.txt".into()], Some("door art".into()))
        .await
        .unwrap();

    // Alice locks a file she changed, from its details.
    std::fs::write(root.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(root.join("door.txt"), "pine\n").unwrap();
    until("the edits show", || w.get_changes().total == 2).await;
    click(&w, "a.txt");
    click(&w, "Lock a.txt");
    assert_eq!(w.get_dialog(), Dialog::Lock);
    assert_eq!(t.get_lock_dialog().what, "a.txt");
    type_into(&w, "Comment", "tuning the orchard");
    click_in(&w, "Lock", "Lock");
    until("the lock is taken", || w.get_dialog() == Dialog::None).await;
    until("it is done", || value_or_empty(&w, "Done") == "Locked.").await;

    // Bob's lock is refused, naming him.
    click(&w, "door.txt");
    click(&w, "Lock door.txt");
    click_in(&w, "Lock", "Lock");
    until("the lock is refused", || t.get_lock_dialog().error.shown).await;
    assert_eq!(t.get_lock_dialog().error.code, "locked");
    until("the holders show", || {
        !t.get_lock_dialog().holders.is_empty()
    })
    .await;
    let holders = value(&w, "Held by others");
    assert!(
        holders.contains("door.txt") && holders.contains("bob") && holders.contains("door art"),
        "{holders}"
    );
    click_in(&w, "Lock", "Cancel");
    assert_eq!(w.get_dialog(), Dialog::None);

    // The locks view lists both, filters to mine, and releases mine.
    click(&w, "Locks");
    assert_eq!(r.get_pane(), Pane::Locks);
    until("locks load", || {
        t.get_locks().loaded && lock_rows(&w).len() == 2
    })
    .await;
    let locks = lock_rows(&w);
    let mine = locks.iter().find(|l| l.path == "a.txt").unwrap();
    assert!(
        mine.mine && mine.comment == "tuning the orchard",
        "{mine:?}"
    );
    let door = locks.iter().find(|l| l.path == "door.txt").unwrap();
    assert!(!door.mine && door.owner == "bob", "{door:?}");
    assert_eq!((t.get_locks().held, t.get_locks().mine), (2, 1));
    click(&w, "Only mine");
    until("only mine show", || lock_rows(&w).len() == 1).await;
    click(&w, "Only mine");
    type_into(&w, "Filter by path", "door");
    t.invoke_filter_locks();
    assert_eq!(lock_rows(&w).len(), 1);
    assert_eq!(lock_rows(&w)[0].path, "door.txt");
    type_into(&w, "Filter by path", "");
    t.invoke_filter_locks();
    click(&w, "Unlock a.txt");
    until("mine is released", || {
        !t.get_locks().busy && lock_rows(&w).iter().all(|l| l.path != "a.txt")
    })
    .await;
    assert_eq!(lock_rows(&w).len(), 1);

    // Shelve the picked file: it goes to the server and is reverted here.
    click(&w, "Changes");
    assert_eq!(r.get_pane(), Pane::Changes);
    until("changes show", || row(&w, "a.txt", false).is_some()).await;
    click(&w, "Pick a.txt");
    click(&w, "Shelve picked files");
    assert_eq!(w.get_dialog(), Dialog::Shelve);
    assert_eq!(t.get_shelve().title, "Shelve 1 file");
    type_into(&w, "Description", "Apricot orchard");
    click_in(&w, "Shelve", "Shelve");
    until("it is shelved", || {
        value_or_empty(&w, "Done").starts_with("Shelved as shelf")
    })
    .await;
    let done = value(&w, "Done");
    assert!(
        done.ends_with("(1 file); the files were reverted here."),
        "{done}"
    );
    let id: u64 = done
        .trim_start_matches("Shelved as shelf ")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "apple\n"
    );
    until("a.txt is no longer changed", || {
        row(&w, "a.txt", false).is_none()
    })
    .await;

    // CI passes the build and fails the tests.
    let target = CheckTarget {
        kind: CheckTargetKind::Shelf,
        id,
        version: Some(1),
    };
    report(world, target, "ci/build", CheckState::Success).await;
    report(world, target, "ci/test", CheckState::Failure).await;

    // The shelves view shows it with a failing badge, and its checks.
    click(&w, "Shelves");
    assert_eq!(r.get_pane(), Pane::Shelves);
    until("shelves load", || !shelf_rows(&w).is_empty()).await;
    let label = format!("Shelf {id}: Apricot orchard");
    until("the badge shows", || shelf_rows(&w)[0].checks.shown).await;
    let badge = shelf_rows(&w)[0].checks.clone();
    assert_eq!(
        (badge.state.as_str(), badge.text.as_str()),
        ("failure", "✕ 1/2")
    );
    assert!(shelf_rows(&w)[0].you);
    click(&w, &label);
    until("the shelf shows", || t.get_shelf().shown).await;
    assert_eq!(value(&w, "Shelf"), "Apricot orchard");
    until("its checks show", || t.get_shelf_checks().row_count() == 2).await;
    assert_eq!(
        checks_shown(t.get_shelf_checks()),
        [
            ("ci/build".to_owned(), "passed".to_owned()),
            ("ci/test".to_owned(), "failed".to_owned())
        ]
    );
    until("its files show", || t.get_shelf_files().row_count() == 1).await;
    assert!(t.get_shelf_diff().row_count() > 0, "the shelf's diff shows");

    // A submit is held back by the failing check.
    click(&w, "Submit…");
    assert_eq!(w.get_dialog(), Dialog::SubmitShelf);
    assert_eq!(t.get_submit_text(), "Apricot orchard");
    click_in(&w, "Submit shelf", "Submit");
    until("the submit is refused", || t.get_submit().error.shown).await;
    assert_eq!(t.get_submit().error.code, "checks_required");
    assert_eq!(
        t.get_submit().error.message,
        "Required checks have not passed for version 1."
    );
    assert_eq!(value(&w, "Missing checks"), "ci/test: failure");
    click_in(&w, "Submit shelf", "Cancel");

    // Unshelve brings the change back.
    click(&w, "Unshelve");
    until("it is unshelved", || w.get_dialog() == Dialog::Unshelved).await;
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "apricot\n"
    );
    click_in(&w, &format!("Unshelved shelf {id}"), "Done");

    // Delete it; the open list is empty again.
    click(&w, "Delete shelf");
    assert_eq!(w.get_dialog(), Dialog::Confirm);
    click_in(&w, &format!("Delete shelf {id}?"), "Delete shelf");
    until("it is deleted", || {
        w.get_dialog() == Dialog::None && shelf_rows(&w).is_empty()
    })
    .await;
    assert_eq!(t.get_shelves().notice, format!("Deleted shelf {id}."));

    // A revision's checks show in the history: a badge on its row, the list in its details.
    click(&w, "History");
    until("history loads", || {
        !r.get_history().loading && !revs(&w).is_empty()
    })
    .await;
    let head = u64::try_from(revs(&w)[0].rev).unwrap();
    assert!(!revs(&w)[0].checks.shown, "nothing reported yet");
    let target = CheckTarget {
        kind: CheckTargetKind::Revision,
        id: head,
        version: None,
    };
    report(world, target, "ci/build", CheckState::Success).await;
    report(world, target, "ci/test", CheckState::Success).await;
    click(&w, "Changes");
    click(&w, "History");
    until("the badge shows", || {
        revs(&w).first().is_some_and(|r| r.checks.shown)
    })
    .await;
    let badge = revs(&w)[0].checks.clone();
    assert_eq!(
        (badge.state.as_str(), badge.text.as_str()),
        ("success", "✓ 2")
    );
    until("the selected revision's checks", || {
        r.get_rev_checks().row_count() == 2
    })
    .await;
    assert_eq!(
        checks_shown(r.get_rev_checks()),
        [
            ("ci/build".to_owned(), "passed".to_owned()),
            ("ci/test".to_owned(), "passed".to_owned())
        ]
    );
    println!("locks, shelves and checks ... ok");
}

fn branch_rows(w: &AppWindow) -> Vec<BranchRow> {
    w.global::<Branching>().get_rows().iter().collect()
}

fn folders(w: &AppWindow) -> Vec<FolderRow> {
    w.global::<Branching>().get_folders().iter().collect()
}

/// Opens the conflict of `path` from its details in Changes.
async fn open_conflict(w: &AppWindow, path: &str) {
    let b = w.global::<Branching>();
    click(w, path);
    click(w, &format!("Resolve {path}"));
    assert_eq!(w.global::<Reading>().get_pane(), Pane::Conflict);
    until("the conflict loads", || b.get_conflict().loaded).await;
    assert_eq!(b.get_conflict().path, path);
}

async fn resolved(w: &AppWindow, path: &str, how: &str) {
    until("it is resolved", || {
        value_or_empty(w, "Done") == format!("Resolved {path}: {how}.")
    })
    .await;
    assert_eq!(w.global::<Reading>().get_pane(), Pane::Changes);
    until("the file is no longer conflicted", || {
        row(w, path, false).is_none_or(|r| r.status != "conflicted")
    })
    .await;
}

/// Opens the branch list from the sidebar, whose button names the branch and revision.
fn open_branches(w: &AppWindow) {
    let t = w.get_workspaces().row_data(0).unwrap();
    click(
        w,
        &format!("Branch {} at r{}: change branch", t.branch, t.rev),
    );
}

async fn branches_merges_and_views(world: &World) {
    let home = Home::new();
    let folder = home.path().join("forks");
    let w = home.window(world, Some(folder.clone()));
    signed_in(world, &w).await;
    click(&w, "Check out a depot…");
    type_into(&w, "Depot", &format!("{}/forks", world.url));
    click(&w, "Browse…");
    click(&w, "Check out");
    until("the workspace opens", || w.get_dialog() == Dialog::None).await;
    until("its changes load", || w.get_changes().loaded).await;
    let root = std::fs::canonicalize(&folder).unwrap();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    let b = w.global::<Branching>();
    let r = w.global::<Reading>();
    let tab = |w: &AppWindow| w.get_workspaces().row_data(0).unwrap();

    // The branch list: the current one first, filtered by name.
    open_branches(&w);
    assert_eq!(w.get_dialog(), Dialog::Branches);
    until("branches load", || b.get_branches().loaded).await;
    let names: Vec<String> = branch_rows(&w).iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names, ["main", "dev", "feature"]);
    assert!(branch_rows(&w)[0].current && branch_rows(&w)[0].selected);
    assert_eq!(branch_rows(&w)[1].origin, "from main@r2");
    type_into(&w, "Filter branches", "FEA");
    b.invoke_filter_changed();
    assert_eq!(branch_rows(&w).len(), 1);
    type_into(&w, "Filter branches", "nope");
    b.invoke_filter_changed();
    assert_eq!(b.get_branches().empty, "No branch matches “nope”.");
    type_into(&w, "Filter branches", "");
    b.invoke_filter_changed();

    // Switch to dev; a revision it does not have is refused first.
    click(&w, "Branch dev");
    assert!(!b.get_branches().is_current);
    type_into(&w, "Revision", "r99");
    b.invoke_rev_edited();
    assert!(b.get_branches().bad_rev);
    type_into(&w, "Revision", "");
    b.invoke_rev_edited();
    assert!(!b.get_branches().bad_rev);
    let dev_head = branch_rows(&w)
        .iter()
        .find(|r| r.name == "dev")
        .map(|r| r.head.to_string())
        .unwrap();
    click(&w, "Switch");
    until("the switch is done", || w.get_dialog() == Dialog::Outcome).await;
    assert_eq!(b.get_outcome().title, "Switched to dev");
    assert!(
        b.get_outcome().text.starts_with(&format!(
            "The workspace is on dev at {dev_head}, from main."
        )),
        "{}",
        b.get_outcome().text
    );
    assert_eq!(read("c.txt"), "c dev\n");
    click_in(&w, "Switched to dev", "Done");
    until("the tab follows", || tab(&w).branch == "dev").await;

    // And back to main.
    open_branches(&w);
    until("branches load", || {
        b.get_branches().loaded && b.get_branches().current == "dev"
    })
    .await;
    click(&w, "Branch main");
    click(&w, "Switch");
    until("back on main", || w.get_dialog() == Dialog::Outcome).await;
    click_in(&w, "Switched to main", "Done");
    until("the tab follows", || tab(&w).branch == "main").await;
    assert_eq!(read("c.txt"), "c main\n");

    // A clean merge of feature, committed.
    open_branches(&w);
    until("branches load", || b.get_branches().loaded).await;
    click(&w, "Branch feature");
    click(&w, "Merge feature…");
    assert_eq!(w.get_dialog(), Dialog::Merge);
    assert_eq!(b.get_merge().title, "Merge into main");
    click_in(&w, "Merge", "Merge");
    until("the merge is done", || w.get_dialog() == Dialog::Outcome).await;
    let o = b.get_outcome();
    assert!(o.title.starts_with("Merged feature r"), "{o:?}");
    assert!(o.done && o.conflicts.is_empty(), "{o:?}");
    click_in(&w, &o.title, "Done");
    assert_eq!(read("g.txt"), "from feature\n");
    until("the merge waits for the commit", || {
        !w.get_changes().merging.is_empty()
    })
    .await;
    w.set_commit_message("Merge feature into main".into());
    click(&w, "Commit merge");
    until("the merge is committed", || {
        value_or_empty(&w, "Done").starts_with("Committed r")
    })
    .await;

    // A merge of dev: four files conflict, each resolved another way.
    open_branches(&w);
    until("branches load", || b.get_branches().loaded).await;
    click(&w, "Branch dev");
    click(&w, "Merge dev…");
    click_in(&w, "Merge", "Merge");
    until("the merge is done", || w.get_dialog() == Dialog::Outcome).await;
    let o = b.get_outcome();
    assert!(
        !o.done && o.conflicts.starts_with("4 files changed on both sides"),
        "{o:?}"
    );
    click(&w, "Resolve conflicts");
    assert_eq!(w.get_dialog(), Dialog::None);
    assert_eq!(r.get_pane(), Pane::Changes);
    until("the conflicts show", || w.get_changes().conflicted == 4).await;
    assert_eq!(read("f.txt"), "new on dev\n");

    // b.txt: their side of the region, then the result saved.
    open_conflict(&w, "b.txt").await;
    let c = b.get_conflict();
    assert!(c.editable && c.from == dev_head, "{c:?}");
    assert_eq!(c.regions_left, 1);
    let mine: Vec<(String, bool)> = b
        .get_mine_lines()
        .iter()
        .map(|l| (l.text.to_string(), l.changed))
        .collect();
    assert_eq!(
        mine,
        [
            ("one".to_owned(), false),
            ("TWO main".to_owned(), true),
            ("three".to_owned(), false)
        ]
    );
    assert!(b
        .get_theirs_lines()
        .iter()
        .any(|l| l.text == "TWO dev" && l.changed));
    let region = b.get_regions().row_data(0).unwrap();
    assert_eq!(
        (region.mine.as_str(), region.theirs.as_str()),
        ("TWO main", "TWO dev")
    );
    click(&w, "Use theirs in Conflict 1 of 1");
    assert_eq!(b.get_conflict().regions_left, 0);
    assert_eq!(b.get_result_text(), "one\nTWO dev\nthree\n");
    click(&w, "Save and mark resolved");
    resolved(&w, "b.txt", "saved the edited result").await;
    assert_eq!(read("b.txt"), "one\nTWO dev\nthree\n");

    // c.txt: mine; d.txt: theirs.
    open_conflict(&w, "c.txt").await;
    click(&w, "Take mine");
    resolved(&w, "c.txt", "kept your version").await;
    assert_eq!(read("c.txt"), "c main\n");
    open_conflict(&w, "d.txt").await;
    click(&w, "Take theirs");
    resolved(&w, "d.txt", "took their version").await;
    assert_eq!(read("d.txt"), "d dev\n");

    // pic.bin cannot be merged line by line: one side is kept.
    open_conflict(&w, "pic.bin").await;
    assert!(!b.get_conflict().editable);
    assert_eq!(b.get_conflict().mine_note, "4 B");
    click(&w, "Keep mine");
    resolved(&w, "pic.bin", "kept your version").await;
    assert_eq!(std::fs::read(root.join("pic.bin")).unwrap(), [0, 3, 0, 3]);
    until("no conflicts left", || w.get_changes().conflicted == 0).await;

    // Abandoning the merge keeps the files but not the merge.
    click(&w, "Abandon merge");
    until("the merge is gone", || w.get_changes().merging.is_empty()).await;
    assert_eq!(read("f.txt"), "new on dev\n");

    // The view: leave a folder out, with its dry run first.
    click(&w, "Workspace view");
    assert_eq!(r.get_pane(), Pane::View);
    until("the view loads", || {
        b.get_view().loaded && !folders(&w).is_empty()
    })
    .await;
    assert_eq!(value(&w, "On disk").split(',').next(), Some("8 files"));
    assert_eq!(folders(&w)[0].path, "Content");
    click(&w, "Open Content");
    until("its folders show", || folders(&w).len() == 3).await;
    let paths: Vec<String> = folders(&w).iter().map(|f| f.path.to_string()).collect();
    assert_eq!(paths, ["Content", "Content/Cine", "Content/Maps"]);
    click(&w, "Include Content/Cine");
    assert_eq!(b.get_exclude_text(), "Content/Cine/**");
    assert!(b.get_view().dirty);
    assert!(folders(&w)[0].on && folders(&w)[0].mixed && !folders(&w)[1].on);
    click(&w, "Preview change");
    until("the dry run answers", || b.get_view().preview).await;
    assert_eq!(
        value(&w, "Change"),
        "Fetch 0 files (0 B); remove 1 file from disk.",
        "{}",
        b.get_view().preview_paths
    );
    assert!(
        root.join("Content/Cine/intro.txt").exists(),
        "a dry run changes nothing"
    );
    click(&w, "Apply");
    until("the view is applied", || !b.get_view().done.is_empty()).await;
    assert_eq!(
        b.get_view().done,
        "View updated: 0 files fetched, 1 removed."
    );
    assert!(!root.join("Content/Cine/intro.txt").exists());
    assert_eq!(read("b.txt"), "one\nTWO dev\nthree\n", "local changes stay");
    assert_eq!(read("f.txt"), "new on dev\n", "added files stay");
    until("it reloads", || b.get_view().loaded && !b.get_view().dirty).await;

    // A file with local changes stops the next change until it is dropped on purpose.
    std::fs::write(root.join("Content/Maps/harbor.txt"), "map, edited\n").unwrap();
    type_into(&w, "Exclude patterns", "Content/**");
    b.invoke_patterns_edited();
    click(&w, "Preview change");
    until("the dry run answers", || b.get_view().preview).await;
    assert!(b.get_view().stopped);
    assert_eq!(
        value(&w, "Files with local changes"),
        "Content/Maps/harbor.txt"
    );
    click(&w, "Drop them anyway…");
    assert_eq!(w.get_dialog(), Dialog::Confirm);
    click_in(&w, "Drop local changes?", "Drop and apply");
    until("the view is applied", || !b.get_view().done.is_empty()).await;
    assert!(!root.join("Content/Maps/harbor.txt").exists());
    println!("branches, merges and views ... ok");
}

/// Alice's app with workspaces of `depots` (checked out outside the window), in folders
/// named after them.
async fn with_workspaces(world: &World, home: &Home, folders: &[(&str, &str)]) -> Vec<String> {
    let desk = Desk::new(
        home.path().to_path_buf(),
        &home.path().join("app"),
        Arc::new(Quiet),
    )
    .with_config_dir(home.path().join("config"));
    desk.sign_in(world.url.clone(), "alice".into(), "correct horse".into())
        .await
        .unwrap();
    let mut roots = Vec::new();
    for (depot, folder) in folders {
        let dir = home.path().join(folder);
        let done = desk
            .checkout(
                format!("{}/{depot}", world.url),
                dir.display().to_string(),
                "main".into(),
            )
            .await
            .unwrap();
        roots.push(done["workspace"].as_str().unwrap().to_owned());
    }
    roots
}

fn link_to(world: &World, link: impl FnOnce(String) -> Link) -> String {
    links::build(&link(world.url.clone())).unwrap()
}

async fn links_handed_to_the_app(world: &World) {
    let s = &world.story;
    let home = Home::new();
    let roots = with_workspaces(world, &home, &[("story", "story")]).await;
    let w = home.window(world, None);
    let r = w.global::<Reading>();
    let l = w.global::<Links>();

    // A link that is not one of the forms is refused, and nothing opens.
    w.invoke_link_arrived("lode://lode.example.com/story".into());
    assert_eq!(w.get_dialog(), Dialog::LinkRefused);
    assert_eq!(
        value(&w, "Why"),
        "The link names nothing to open. Nothing was opened."
    );
    click(&w, "OK");
    assert_eq!(w.get_dialog(), Dialog::None);
    assert_eq!(w.get_workspaces().row_count(), 0);

    // A revision link opens the only workspace of its depot on that revision, even one that
    // is not on the first page of the history.
    let link = link_to(world, |server| Link::Revision {
        server,
        depot: "story".into(),
        rev: s.first,
    });
    w.invoke_link_arrived(link.as_str().into());
    until("the workspace opens on the revision", || {
        r.get_pane() == Pane::History && r.get_detail().rev == i32::try_from(s.first).unwrap()
    })
    .await;
    assert_eq!(w.get_current_tab(), 1);
    assert_eq!(w.get_workspaces().row_data(0).unwrap().root, roots[0]);
    assert_eq!(r.get_detail().title, format!("r{}", s.first));

    // Its "Copy link" gives the same link back.
    click(&w, &format!("Copy link to r{}", s.first));
    assert_eq!(l.get_copied_link(), link);
    assert_eq!(l.get_copied(), format!("r{}", s.first));

    // A file link for another branch shows the file's history here, and says so.
    let link = link_to(world, |server| Link::File {
        server,
        depot: "story".into(),
        path: "notes.txt".into(),
        rev: None,
        branch: Some("dev".into()),
    });
    w.invoke_link_arrived(link.as_str().into());
    until("the file's history", || {
        r.get_history().path == "notes.txt" && !r.get_history().loading
    })
    .await;
    assert_eq!(
        value(&w, "Note"),
        "The link is for notes.txt on dev, and this workspace is on main, so its history on main is shown."
    );
    click(&w, "Dismiss");
    assert_eq!(l.get_notice(), "");
    until("its newest revision", || r.get_detail().shown).await;
    click(&w, &format!("Copy link to notes.txt at r{}", s.head));
    assert_eq!(
        l.get_copied_link(),
        link_to(world, |server| Link::File {
            server,
            depot: "story".into(),
            path: "notes.txt".into(),
            rev: Some(s.head),
            branch: None,
        })
    );
    w.hide().unwrap();

    // With two workspaces of the depot, the app asks which one.
    let home = Home::new();
    let roots = with_workspaces(world, &home, &[("story", "one"), ("story", "two")]).await;
    let w = home.window(world, None);
    let r = w.global::<Reading>();
    let link = link_to(world, |server| Link::Revision {
        server,
        depot: "story".into(),
        rev: s.head,
    });
    w.invoke_link_arrived(link.into());
    assert_eq!(w.get_dialog(), Dialog::LinkChoose);
    assert_eq!(w.global::<Links>().get_choices().row_count(), 2);
    click(&w, &format!("Open in {}", roots[1]));
    until("the chosen workspace opens", || {
        r.get_pane() == Pane::History && r.get_detail().shown
    })
    .await;
    assert_eq!(w.get_workspaces().row_data(0).unwrap().root, roots[1]);

    // A link that arrives while a dialog is open waits for it to close.
    click(&w, "Workspaces");
    click(&w, "About Lode Desk");
    click(&w, "Close");
    click(&w, "Sign in to a server…");
    let link = link_to(world, |server| Link::Revision {
        server,
        depot: "harbor".into(),
        rev: 1,
    });
    w.invoke_link_arrived(link.into());
    assert_eq!(w.get_dialog(), Dialog::SignIn);
    press(&w, &[], Key::Escape);

    // No workspace of that depot: the app offers to check it out, with the link's server.
    assert_eq!(w.get_dialog(), Dialog::LinkNone);
    click(&w, "Check out harbor…");
    assert_eq!(w.get_dialog(), Dialog::CheckOut);
    assert_eq!(value(&w, "Depot"), format!("{}/harbor", world.url));
    w.hide().unwrap();
    println!("links handed to the app ... ok");
}

const PACKAGE: &[u8] = b"the next Lode Desk";

/// Serves a feed offering 99.0.0 for `windows-x86_64`, signed with `sk`.
fn serve_feed(sk: &minisign::SecretKey) -> String {
    use base64::Engine;
    use std::io::{Read, Write};
    let b64 = |t: String| base64::engine::general_purpose::STANDARD.encode(t);
    let sig = minisign::sign(None, sk, PACKAGE, None, None).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let feed = json!({
        "version": "99.0.0",
        "notes": "Faster history.",
        "pub_date": "2026-11-20T10:00:00Z",
        "platforms": {
            "windows-x86_64": { "signature": b64(sig.into_string()), "url": format!("{base}/package") }
        }
    })
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                feed.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(feed.as_bytes());
        }
    });
    format!("{base}/latest.json")
}

async fn updates_of_the_app(world: &World) {
    use base64::Engine;
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey =
        base64::engine::general_purpose::STANDARD.encode(kp.pk.to_box().unwrap().into_string());
    let feed = serve_feed(&kp.sk);
    let home = Home::new();
    let updater = || Updater {
        pubkey: pubkey.clone(),
        feed: feed.clone(),
        current: "0.1.0".into(),
        platforms: vec!["windows-x86_64-msi".into(), "windows-x86_64".into()],
        settings_file: home.path().join("app/settings.json"),
    };
    let w = home.window_with(world, None, updater());
    let u = w.global::<Updates>();

    // The check a moment after start finds the new version and offers it.
    until("the update is offered", || u.get_offer().shown).await;
    let offer = u.get_offer();
    assert_eq!(offer.version, "99.0.0");
    assert_eq!(offer.current, "0.1.0");
    click(&w, "What's new");
    assert_eq!(value(&w, "What's new"), "Faster history.");

    // "Later" puts it away; "Check for updates now" brings it back and says what it found.
    click(&w, "Later");
    assert!(!u.get_offer().shown);
    click(&w, "Check for updates now");
    until("the check ends", || !u.get_settings().checking).await;
    assert_eq!(value(&w, "Update check"), "Lode Desk 99.0.0 is available.");
    assert!(u.get_offer().shown);

    // Turning automatic checks off is kept in the settings, and the next start does not check.
    assert!(u.get_settings().check);
    click(&w, "Check for updates automatically");
    assert!(!u.get_settings().check);
    let saved = lode_desk::update::load_settings(&home.path().join("app/settings.json"));
    assert!(!saved.check_updates);
    w.hide().unwrap();

    let w = home.window_with(world, None, updater());
    let u = w.global::<Updates>();
    assert!(!u.get_settings().check);
    sleep(4000).await;
    assert!(
        !u.get_offer().shown,
        "no check at start with the setting off"
    );
    // Asking still checks.
    click(&w, "Check for updates now");
    until("the check ends", || !u.get_settings().checking).await;
    assert!(u.get_offer().shown);
    w.hide().unwrap();
    println!("updates of the app ... ok");
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
        desktop_menus(&world).await;
        sign_in_with_a_password(&world).await;
        single_sign_on_when_offered(&world).await;
        check_out_with_progress_then_reopen(&world).await;
        check_out_before_signing_in(&world).await;
        opening_a_folder_that_is_not_a_workspace(&world).await;
        changes_pick_commit_revert_and_changelists(&world).await;
        content_validation(&world).await;
        twenty_thousand_rows(&world).await;
        history_blame_and_diffs(&world).await;
        artist_browser(&world).await;
        locks_shelves_and_checks(&world).await;
        branches_merges_and_views(&world).await;
        links_handed_to_the_app(&world).await;
        updates_of_the_app(&world).await;
        let _ = slint::quit_event_loop();
    })
    .unwrap();
    slint::run_event_loop_until_quit().unwrap();
    println!("ui: all scenarios passed");
    std::process::ExitCode::SUCCESS
}
