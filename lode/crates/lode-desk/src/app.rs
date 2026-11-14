//! The Slint window (T130): Home with recent workspaces, sign-in, check out, and one tab per
//! open workspace.
//!
//! The screens are the `.slint` files in `ui/slint/`; this module fills their properties and
//! answers their callbacks. Every operation goes through [`Desk`], which runs it on a thread
//! of its own, so the UI thread only awaits the answer. Progress and the single sign-on
//! address come from that thread and reach the window through `slint::invoke_from_event_loop`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::Arc;
use std::time::Duration;

use lode_ops::output::{ProgressEvent, Ui};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::desk::{Desk, ErrorView, RecentWorkspace};

/// The screens as Slint compiles them. Generated code: the crate's lints do not apply.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
mod screens {
    slint::include_modules!();
}

pub use screens::*;

mod work;

use work::WorkspaceState;

/// The app's identifier; its data directory is named after it, as the Tauri app's was, so the
/// recent list carries over.
pub const APP_ID: &str = "dev.simpledevops.lode-desk";

/// How long the server address must stay unchanged before the app asks it about single
/// sign-on.
const SSO_OFFER_DELAY: Duration = Duration::from_millis(300);

/// Asks for a folder; the argument is the picker's title. Runs on the UI thread.
pub type PickFolder = Rc<dyn Fn(&str) -> Option<PathBuf>>;

/// Opens a web address in the browser; false when it could not. Runs on an operation's thread.
pub type OpenBrowser = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Where the app keeps things and how it reaches the system; tests replace the parts that
/// would touch the real user's files or open windows.
pub struct Options {
    /// Where relative paths start (the user's home directory).
    pub home: PathBuf,
    /// The app's own files, such as the recent list.
    pub data_dir: PathBuf,
    /// The lode config directory; `None` uses the same one as `lode`.
    pub config_dir: Option<PathBuf>,
    /// The folder picker.
    pub pick_folder: PickFolder,
    /// The browser for single sign-on.
    pub open_browser: OpenBrowser,
}

impl Options {
    /// The real user's directories, the system's folder picker and browser.
    pub fn from_system() -> Result<Self, String> {
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| "cannot find the home directory".to_owned())?;
        Ok(Self {
            home: dirs.home_dir().to_path_buf(),
            data_dir: dirs.data_dir().join(APP_ID),
            config_dir: None,
            pick_folder: Rc::new(|title| rfd::FileDialog::new().set_title(title).pick_folder()),
            open_browser: Arc::new(|url| lode_ops::output::open_in_browser(url).is_ok()),
        })
    }
}

/// Starts the window and runs until it closes.
pub fn run() -> Result<(), String> {
    let window = build(Options::from_system()?)?;
    window.run().map_err(|e| e.to_string())
}

/// Makes the window and connects it to the operations. The window is not shown yet.
pub fn build(options: Options) -> Result<AppWindow, String> {
    let window = AppWindow::new().map_err(|e| e.to_string())?;
    let ui = Arc::new(WindowUi {
        window: window.as_weak(),
        open_browser: options.open_browser,
    });
    let changed = window.as_weak();
    let mut desk = Desk::new(options.home, &options.data_dir, ui).with_change_hook(move |root| {
        let _ = changed.upgrade_in_event_loop(move |w| w.invoke_files_changed(root.into()));
    });
    if let Some(dir) = options.config_dir {
        desk = desk.with_config_dir(dir);
    }
    let recent = Rc::new(VecModel::<Recent>::default());
    let workspaces = Rc::new(VecModel::<WorkspaceTab>::default());
    window.set_version(env!("CARGO_PKG_VERSION").into());
    window.set_recent(ModelRc::from(Rc::clone(&recent)));
    window.set_workspaces(ModelRc::from(Rc::clone(&workspaces)));
    let change_rows = Rc::new(VecModel::<ChangeRow>::default());
    let move_targets = Rc::new(VecModel::<MoveTarget>::default());
    let messages_model = Rc::new(VecModel::<SharedString>::default());
    window.set_change_rows(ModelRc::from(Rc::clone(&change_rows)));
    window.set_move_targets(ModelRc::from(Rc::clone(&move_targets)));
    window.set_recent_messages(ModelRc::from(Rc::clone(&messages_model)));
    let messages_file = options.data_dir.join("messages.json");
    let app = Rc::new(App {
        window: window.as_weak(),
        desk,
        recent,
        workspaces,
        pick_folder: options.pick_folder,
        sign_in_generation: Cell::new(0),
        sso_offer_generation: Cell::new(0),
        back_to_checkout: Cell::new(false),
        change_rows,
        move_targets,
        messages_model,
        messages: RefCell::new(work::load_messages(&messages_file)),
        messages_file,
        work: RefCell::new(HashMap::new()),
        shown_root: RefCell::new(None),
        dialog_paths: RefCell::new(Vec::new()),
        this: RefCell::new(RcWeak::new()),
    });
    *app.this.borrow_mut() = Rc::downgrade(&app);
    app.refresh_recent();
    connect(&window, &app);
    work::connect(&window, &app);
    Ok(window)
}

/// Sends an operation's reports to the window.
struct WindowUi {
    window: slint::Weak<AppWindow>,
    open_browser: OpenBrowser,
}

impl Ui for WindowUi {
    fn line(&self, _text: &str) {}

    fn warn(&self, _text: &str) {}

    fn progress(&self, event: &ProgressEvent<'_>) {
        let (fraction, text) = progress_view(event);
        let _ = self.window.upgrade_in_event_loop(move |w| {
            let mut state = w.get_checkout();
            if state.busy {
                state.progress = fraction;
                state.progress_text = text.as_str().into();
                w.set_checkout(state);
            }
            let mut changes = w.get_changes();
            if changes.busy == "Committing" {
                changes.progress = fraction;
                changes.progress_text = text.into();
                w.set_changes(changes);
            }
        });
    }

    fn open_url(&self, url: &str) -> bool {
        // The dialog shows the address too, for when no browser opens.
        let shown = url.to_owned();
        let _ = self.window.upgrade_in_event_loop(move |w| {
            let mut state = w.get_sign_in();
            if state.waiting {
                state.sso_url = shown.into();
                w.set_sign_in(state);
            }
        });
        (self.open_browser)(url)
    }
}

/// The window's controller.
struct App {
    window: slint::Weak<AppWindow>,
    desk: Desk,
    recent: Rc<VecModel<Recent>>,
    workspaces: Rc<VecModel<WorkspaceTab>>,
    pick_folder: PickFolder,
    /// Bumped each time the sign-in dialog opens or closes, so a late answer from an
    /// earlier one is dropped.
    sign_in_generation: Cell<u64>,
    /// Bumped on each edit of the server address; only the newest question is answered.
    sso_offer_generation: Cell<u64>,
    /// The sign-in dialog was opened from the check-out dialog, which comes back after it.
    back_to_checkout: Cell<bool>,
    /// The rows of the changes view.
    change_rows: Rc<VecModel<ChangeRow>>,
    /// The changelists the move dialog offers.
    move_targets: Rc<VecModel<MoveTarget>>,
    /// The recent commit messages, for their dialog.
    messages_model: Rc<VecModel<SharedString>>,
    /// The recent commit messages, newest first.
    messages: RefCell<Vec<String>>,
    /// `messages.json` in the app's data directory.
    messages_file: PathBuf,
    /// What each open workspace shows, by root.
    work: RefCell<HashMap<String, WorkspaceState>>,
    /// The workspace the changes view shows now.
    shown_root: RefCell<Option<String>>,
    /// The files a revert or move dialog acts on.
    dialog_paths: RefCell<Vec<String>>,
    /// This controller, for tasks that outlive a call.
    this: RefCell<RcWeak<App>>,
}

fn connect(window: &AppWindow, app: &Rc<App>) {
    let a = Rc::clone(app);
    window.on_open_recent(move |path| a.open(path.to_string()));
    let a = Rc::clone(app);
    window.on_forget_recent(move |path| a.forget(&path));
    let a = Rc::clone(app);
    window.on_open_folder(move || a.open_folder());
    let a = Rc::clone(app);
    window.on_show_sign_in(move |server| a.show_sign_in(&server));
    let a = Rc::clone(app);
    window.on_show_check_out(move || a.show_check_out());
    let a = Rc::clone(app);
    window.on_close_workspace(move |i| a.close_workspace(i));
    let a = Rc::clone(app);
    window.on_sign_in_server_edited(move |server| a.server_edited(server.to_string()));
    let a = Rc::clone(app);
    window.on_sign_in_password(move || a.sign_in_password());
    let a = Rc::clone(app);
    window.on_sign_in_sso(move || a.sign_in_sso());
    let a = Rc::clone(app);
    window.on_pick_checkout_folder(move || a.pick_checkout_folder());
    let a = Rc::clone(app);
    window.on_check_out(move || a.check_out());
    let a = Rc::clone(app);
    window.on_close_dialog(move || a.close_dialog());
}

/// Runs `future` on the UI thread's event loop.
fn spawn(future: impl Future<Output = ()> + 'static) {
    // Fails only when there is no event loop, and then there is no window to update either.
    let _ = slint::spawn_local(future);
}

impl App {
    fn refresh_recent(&self) {
        let rows: Vec<Recent> = self.desk.recent().iter().map(recent_row).collect();
        self.recent.set_vec(rows);
    }

    /// Opens the workspace at `path` in a tab (or selects its tab when it is open).
    fn open(self: &Rc<Self>, path: String) {
        let me = Rc::clone(self);
        spawn(async move {
            let result = me.desk.open_workspace(path).await;
            let Some(w) = me.window.upgrade() else { return };
            match result {
                Ok(info) => {
                    me.show_workspace(&w, &info);
                    me.refresh_recent();
                }
                Err(e) => {
                    w.set_current_tab(0);
                    w.set_home_error(error_info(&e));
                }
            }
        });
    }

    fn show_workspace(&self, w: &AppWindow, info: &Value) {
        let tab = WorkspaceTab {
            root: text(&info["root"]).into(),
            depot: text(&info["depot"]).into(),
            branch: text(&info["branch"]).into(),
            rev: text(&info["rev"]).into(),
            server: host_of(&text(&info["server"])).into(),
        };
        let at = self.workspaces.iter().position(|t| t.root == tab.root);
        let index = match at {
            Some(i) => {
                self.workspaces.set_row_data(i, tab);
                i
            }
            None => {
                self.workspaces.push(tab);
                self.workspaces.row_count() - 1
            }
        };
        w.set_home_error(ErrorInfo::default());
        w.set_workspace_error(ErrorInfo::default());
        let root = text(&info["root"]);
        if let Err(e) = self.desk.watch(root.clone()) {
            w.set_workspace_error(error_info(&e));
        }
        w.set_current_tab(tab_number(index));
        // Shown at once, not only when the tab changes (it may be the current one already).
        self.show_root(w, Some(root));
    }

    fn forget(&self, path: &str) {
        if let Err(e) = self.desk.forget(path) {
            if let Some(w) = self.window.upgrade() {
                w.set_home_error(error_info(&e));
            }
        }
        self.refresh_recent();
    }

    fn open_folder(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        if w.get_dialog() != Dialog::None {
            return;
        }
        if let Some(dir) = (self.pick_folder)("Open a workspace folder") {
            self.open(dir.to_string_lossy().into_owned());
        }
    }

    fn close_workspace(&self, index: i32) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Ok(i) = usize::try_from(index) else {
            return;
        };
        if i >= self.workspaces.row_count() {
            return;
        }
        if let Some(tab) = self.workspaces.row_data(i) {
            self.desk.unwatch(&tab.root);
            self.work.borrow_mut().remove(tab.root.as_str());
        }
        self.workspaces.remove(i);
        let current = w.get_current_tab();
        let closed = tab_number(i);
        if current >= closed {
            // The tab before the closed one, or Home.
            w.set_current_tab((current - 1).max(0));
        }
    }

    fn show_sign_in(self: &Rc<Self>, server: &str) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        self.back_to_checkout
            .set(w.get_dialog() == Dialog::CheckOut);
        self.sign_in_generation
            .set(self.sign_in_generation.get() + 1);
        let server = if server.is_empty() {
            "https://"
        } else {
            server
        };
        w.set_sign_in(SignInState {
            server: server.into(),
            password_login: true,
            ..SignInState::default()
        });
        w.set_dialog(Dialog::SignIn);
        self.server_edited(server.to_owned());
    }

    /// Asks the server whether it offers single sign-on once the address has stayed the same
    /// for a moment.
    fn server_edited(self: &Rc<Self>, server: String) {
        let generation = self.sso_offer_generation.get() + 1;
        self.sso_offer_generation.set(generation);
        if let Some(w) = self.window.upgrade() {
            let mut state = w.get_sign_in();
            state.sso_label = SharedString::new();
            state.password_login = true;
            w.set_sign_in(state);
        }
        let server = server.trim().to_owned();
        if !looks_like_server(&server) {
            return;
        }
        let me: RcWeak<Self> = Rc::downgrade(self);
        slint::Timer::single_shot(SSO_OFFER_DELAY, move || {
            let Some(me) = me.upgrade() else { return };
            if me.sso_offer_generation.get() != generation {
                return;
            }
            spawn(async move {
                let offer = me.desk.sso_offer(server).await;
                if me.sso_offer_generation.get() != generation {
                    return;
                }
                let Some(w) = me.window.upgrade() else { return };
                let mut state = w.get_sign_in();
                match offer {
                    Ok(o) if !o.is_null() => {
                        state.sso_label = text(&o["label"]).into();
                        state.password_login = o["password_login"].as_bool().unwrap_or(true);
                    }
                    // No offer, or a server that does not answer yet: password only.
                    _ => {
                        state.sso_label = SharedString::new();
                        state.password_login = true;
                    }
                }
                w.set_sign_in(state);
            });
        });
    }

    fn sign_in_password(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let mut state = w.get_sign_in();
        if state.busy || !state.done.is_empty() {
            return;
        }
        let server = state.server.trim().to_owned();
        let name = state.name.trim().to_owned();
        if server.is_empty() || name.is_empty() {
            state.error = error_info(&usage("enter the server address and your name"));
            w.set_sign_in(state);
            return;
        }
        let password = state.password.to_string();
        state.busy = true;
        state.error = ErrorInfo::default();
        w.set_sign_in(state);
        let generation = self.sign_in_generation.get();
        let me = Rc::clone(self);
        spawn(async move {
            let result = me.desk.sign_in(server, name, password).await;
            me.signed_in(generation, result);
        });
    }

    fn sign_in_sso(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let mut state = w.get_sign_in();
        if state.busy {
            return;
        }
        let server = state.server.trim().to_owned();
        state.busy = true;
        state.waiting = true;
        state.error = ErrorInfo::default();
        state.sso_url = SharedString::new();
        w.set_sign_in(state);
        let generation = self.sign_in_generation.get();
        let me = Rc::clone(self);
        spawn(async move {
            let result = me.desk.sign_in_sso(server).await;
            me.signed_in(generation, result);
        });
    }

    /// Shows the result of a sign-in, unless the dialog it was for has closed.
    fn signed_in(&self, generation: u64, result: Result<Value, ErrorView>) {
        if self.sign_in_generation.get() != generation {
            return;
        }
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let mut state = w.get_sign_in();
        state.busy = false;
        state.waiting = false;
        match result {
            Ok(me) => {
                state.password = SharedString::new();
                state.done = format!(
                    "Signed in to {} as {}.",
                    text(&me["server"]).trim_end_matches('/'),
                    text(&me["name"])
                )
                .into();
            }
            Err(e) => {
                state.error = error_info(&e);
                if e.code == "unauthenticated" {
                    // "Sign in, then try again" says nothing here: this is the sign-in.
                    state.error.next = SharedString::new();
                }
            }
        }
        w.set_sign_in(state);
    }

    fn show_check_out(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        if w.get_dialog() != Dialog::None {
            return;
        }
        w.set_checkout(CheckoutState {
            target: "https://".into(),
            branch: "main".into(),
            progress: -1.0,
            ..CheckoutState::default()
        });
        w.set_dialog(Dialog::CheckOut);
    }

    fn pick_checkout_folder(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        if let Some(dir) = (self.pick_folder)("Folder for the new workspace") {
            let mut state = w.get_checkout();
            state.folder = dir.to_string_lossy().into_owned().into();
            w.set_checkout(state);
        }
    }

    fn check_out(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let mut state = w.get_checkout();
        if state.busy {
            return;
        }
        let target = state.target.trim().to_owned();
        let folder = state.folder.trim().to_owned();
        let branch = match state.branch.trim() {
            "" => "main".to_owned(),
            b => b.to_owned(),
        };
        state.sign_in_server = SharedString::new();
        if server_of_target(&target).is_none() || folder.is_empty() {
            state.error = error_info(&usage(
                "enter the depot as the server address followed by the depot name, and a folder",
            ));
            w.set_checkout(state);
            return;
        }
        state.busy = true;
        state.progress = -1.0;
        state.progress_text = SharedString::new();
        state.error = ErrorInfo::default();
        w.set_checkout(state);
        let me = Rc::clone(self);
        spawn(async move {
            let result = match me.desk.checkout(target.clone(), folder, branch).await {
                Ok(done) => me.desk.open_workspace(text(&done["workspace"])).await,
                Err(e) => Err(e),
            };
            let Some(w) = me.window.upgrade() else { return };
            let mut state = w.get_checkout();
            state.busy = false;
            match result {
                Ok(info) => {
                    w.set_checkout(state);
                    w.set_dialog(Dialog::None);
                    me.show_workspace(&w, &info);
                    me.refresh_recent();
                }
                Err(e) => {
                    if e.code == "unauthenticated" {
                        state.sign_in_server = server_of_target(&target).unwrap_or_default().into();
                    }
                    state.error = error_info(&e);
                    w.set_checkout(state);
                }
            }
        });
    }

    fn close_dialog(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        match w.get_dialog() {
            Dialog::CheckOut if w.get_checkout().busy => {}
            Dialog::NewChangelist
            | Dialog::RenameChangelist
            | Dialog::CommitChangelist
            | Dialog::DeleteChangelist
            | Dialog::Revert
            | Dialog::MoveTo
                if w.get_list_dialog().busy => {}
            Dialog::SignIn => {
                let state = w.get_sign_in();
                if state.busy && !state.waiting {
                    return;
                }
                // A browser sign-in still running ends unseen.
                self.sign_in_generation
                    .set(self.sign_in_generation.get() + 1);
                self.sso_offer_generation
                    .set(self.sso_offer_generation.get() + 1);
                if self.back_to_checkout.replace(false) {
                    if !state.done.is_empty() {
                        let mut checkout = w.get_checkout();
                        checkout.error = ErrorInfo::default();
                        checkout.sign_in_server = SharedString::new();
                        w.set_checkout(checkout);
                    }
                    w.set_dialog(Dialog::CheckOut);
                } else {
                    w.set_dialog(Dialog::None);
                }
            }
            _ => w.set_dialog(Dialog::None),
        }
    }
}

/// The tab number of the workspace at `index` (0 is Home).
fn tab_number(index: usize) -> i32 {
    i32::try_from(index).map_or(i32::MAX, |i| i.saturating_add(1))
}

fn recent_row(r: &RecentWorkspace) -> Recent {
    Recent {
        path: r.path.as_str().into(),
        depot: r.depot.as_str().into(),
        branch: r.branch.as_str().into(),
        host: host_of(&r.server).into(),
    }
}

/// `lode.example.com:8443` for `https://lode.example.com:8443/`.
fn host_of(server: &str) -> &str {
    let rest = server.split_once("://").map_or(server, |(_, r)| r);
    rest.trim_end_matches('/')
}

/// A JSON value as text: strings as they are, numbers written out, nothing for null.
fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn usage(message: &str) -> ErrorView {
    ErrorView {
        code: "usage".into(),
        message: message.into(),
        details: Value::Null,
    }
}

/// An error as the error banner shows it.
pub fn error_info(e: &ErrorView) -> ErrorInfo {
    ErrorInfo {
        shown: true,
        message: e.message.as_str().into(),
        next: next_step(&e.code).unwrap_or_default().into(),
        code: e.code.as_str().into(),
    }
}

/// What to do next, for errors where the message alone leaves the user stuck.
pub fn next_step(code: &str) -> Option<&'static str> {
    Some(match code {
        "unauthenticated" => "Sign in to the server from the Home tab, then try again.",
        "permission_denied" => "Ask a depot admin for access.",
        "workspace_in_use" => {
            "Another lode command is using this workspace; try again when it has finished."
        }
        "not_a_workspace" => "Pick the folder you checked out into, or check out the depot again.",
        "checkout_incomplete" => "Check out into the same folder again to finish it.",
        "update_incomplete" => {
            "An update or branch switch stopped before it finished. Finish it, then try again."
        }
        "network" => "Check the server address and your connection.",
        "sso_failed" => {
            "The company sign-in did not finish. If the message names your account, ask a server \
             administrator to add or enable it; otherwise sign in again."
        }
        "obliterated" => {
            "An administrator removed this content from the server for good. Other versions of \
             the file still open."
        }
        _ => return None,
    })
}

/// Whether `s` looks like a whole server address, so it is worth asking about single sign-on.
pub fn looks_like_server(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"));
    matches!(rest.and_then(|r| r.chars().next()), Some(c) if c != '/' && !c.is_whitespace())
}

/// The server of a check-out target (`<server>/<depot>`), or `None` when it is not one.
pub fn server_of_target(target: &str) -> Option<String> {
    let t = target.trim().trim_end_matches('/');
    let at = t.rfind('/')?;
    let server = &t[..at];
    if !t.contains("://") || server.ends_with('/') {
        return None;
    }
    Some(server.to_owned())
}

/// The progress bar's fraction (-1 while the total is unknown) and its text.
pub fn progress_view(e: &ProgressEvent<'_>) -> (f32, String) {
    let fraction = if e.total > 0 {
        // Display precision only.
        #[allow(clippy::cast_precision_loss)]
        let f = (e.done as f64 / e.total as f64).min(1.0) as f32;
        f
    } else {
        -1.0
    };
    let text = format!(
        "{} {} of {} · {}",
        e.label,
        grouped(e.done),
        grouped(e.total),
        human_bytes(e.bytes)
    );
    (fraction, text)
}

/// `12,345`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1.5 MB`.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_name_their_server() {
        assert_eq!(
            server_of_target(" https://lode.example.com/game/ ").as_deref(),
            Some("https://lode.example.com")
        );
        assert_eq!(server_of_target("https://lode.example.com"), None);
        assert_eq!(server_of_target("game"), None);
        assert_eq!(server_of_target("https://"), None);
    }

    #[test]
    fn server_addresses_are_recognised_once_complete() {
        assert!(looks_like_server("https://lode.example.com"));
        assert!(looks_like_server("http://127.0.0.1:8080"));
        assert!(!looks_like_server("https://"));
        assert!(!looks_like_server("lode.example.com"));
    }

    #[test]
    fn progress_reads_as_counts_and_size() {
        let e = ProgressEvent {
            label: "Fetching",
            done: 1234,
            total: 5000,
            bytes: 3 * 1024 * 1024 / 2,
            finished: false,
        };
        let (f, text) = progress_view(&e);
        assert!((f - 0.2468).abs() < 1e-4);
        assert_eq!(text, "Fetching 1,234 of 5,000 · 1.5 MB");
        let unknown = ProgressEvent { total: 0, ..e };
        assert!(progress_view(&unknown).0 < 0.0);
        assert_eq!(human_bytes(12), "12 B");
    }

    #[test]
    fn every_advised_code_has_its_next_step() {
        for code in [
            "unauthenticated",
            "permission_denied",
            "workspace_in_use",
            "not_a_workspace",
            "checkout_incomplete",
            "update_incomplete",
            "network",
            "sso_failed",
            "obliterated",
        ] {
            assert!(next_step(code).is_some(), "{code}");
        }
        assert_eq!(next_step("conflict"), None);
        assert_eq!(
            host_of("https://lode.example.com:8443/"),
            "lode.example.com:8443"
        );
    }
}
