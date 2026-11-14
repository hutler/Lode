//! What reaches the window from outside it (T135): `lode://` links handed to the app, links
//! copied from it, and updates of the app itself.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::read::View;
use super::{error_info, host_of, spawn, text, App, AppWindow, Dialog, ErrorInfo};
use super::{Branching, LinkChoice, LinkState, Links, Team, UpdateOffer, Updates, WorkspaceTab};
use crate::desk::ErrorView;
use crate::links::{self, Link};
use crate::update::{self, Found, UpdateCheck, Updater};

/// How long "Copied" shows on a copy button.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// How long after start the first update check waits, so it never slows the start.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(3);
/// How often a running app looks for a new version.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest piece of a refused link the dialog shows.
const SHOWN_LINK: usize = 200;

/// Links and updates.
pub(super) struct OutsideState {
    choices: Rc<VecModel<LinkChoice>>,
    /// The link a choice, or a check out, is waiting for.
    pending: RefCell<Option<Link>>,
    /// A link that arrived while another dialog was open; shown when it closes.
    queued: RefCell<Option<String>>,
    updater: Updater,
    /// The version on offer, with its package.
    found: RefCell<Option<Found>>,
    /// The version the user said "Later" to.
    later: RefCell<Option<String>>,
    copied_timer: slint::Timer,
    check_timer: slint::Timer,
}

impl OutsideState {
    pub(super) fn new(window: &AppWindow, updater: Updater) -> Self {
        let choices = Rc::new(VecModel::<LinkChoice>::default());
        window
            .global::<Links>()
            .set_choices(ModelRc::from(Rc::clone(&choices)));
        let mut settings = window.global::<Updates>().get_settings();
        settings.check = update::load_settings(&updater.settings_file).check_updates;
        window.global::<Updates>().set_settings(settings);
        Self {
            choices,
            pending: RefCell::new(None),
            queued: RefCell::new(None),
            updater,
            found: RefCell::new(None),
            later: RefCell::new(None),
            copied_timer: slint::Timer::default(),
            check_timer: slint::Timer::default(),
        }
    }
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>) {
    let a = Rc::clone(app);
    window.on_link_arrived(move |text| a.open_link_text(text.to_string()));
    let l = window.global::<Links>();
    let a = Rc::clone(app);
    l.on_pick(move |path| a.pick_workspace(path.to_string()));
    let a = Rc::clone(app);
    l.on_check_out(move || a.check_out_for_link());
    let a = Rc::clone(app);
    l.on_close(move || a.close_dialog());
    let a = Rc::clone(app);
    l.on_copy_revision(move |rev| {
        if let Ok(rev) = u64::try_from(rev) {
            a.copy_link(
                |server, depot| Link::Revision { server, depot, rev },
                format!("r{rev}"),
            );
        }
    });
    let a = Rc::clone(app);
    l.on_copy_file(move |path, rev| {
        if let Ok(rev) = u64::try_from(rev) {
            let target = format!("{path} at r{rev}");
            let path = path.to_string();
            a.copy_link(
                |server, depot| Link::File {
                    server,
                    depot,
                    path,
                    rev: Some(rev),
                    branch: None,
                },
                target,
            );
        }
    });
    let a = Rc::clone(app);
    l.on_copy_shelf(move |id| {
        if let Ok(id) = u64::try_from(id) {
            a.copy_link(
                |server, depot| Link::Shelf { server, depot, id },
                format!("shelf {id}"),
            );
        }
    });
    let u = window.global::<Updates>();
    let a = Rc::clone(app);
    u.on_check_now(move || a.check_updates(true));
    let a = Rc::clone(app);
    u.on_set_check(move |on| a.set_check_updates(on));
    let a = Rc::clone(app);
    u.on_later(move || a.update_later());
    let a = Rc::clone(app);
    u.on_install(move || a.install_update());

    // The first check a moment after start, then once a day.
    let me = Rc::downgrade(app);
    slint::Timer::single_shot(FIRST_CHECK_AFTER, move || {
        if let Some(a) = me.upgrade() {
            a.check_updates(false);
        }
    });
    let me = Rc::downgrade(app);
    app.outside
        .check_timer
        .start(slint::TimerMode::Repeated, CHECK_EVERY, move || {
            if let Some(a) = me.upgrade() {
                a.check_updates(false);
            }
        });
}

/// Runs `f` on a thread of its own with a single-threaded runtime (the update check needs one;
/// the window's thread has none).
async fn off_thread<F, Fut, T>(f: F) -> Result<T, ErrorView>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, ErrorView>>,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("lode-desk-update".into())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(f()),
                Err(e) => Err(internal(&e.to_string())),
            };
            let _ = tx.send(result);
        });
    if let Err(e) = spawned {
        return Err(internal(&e.to_string()));
    }
    rx.await
        .unwrap_or_else(|_| Err(internal("the check stopped without an answer")))
}

fn internal(message: &str) -> ErrorView {
    ErrorView {
        code: "internal".into(),
        message: message.into(),
        details: Value::Null,
    }
}

/// `https://host[:port]` lowercased, without a default port or a trailing slash, so two
/// spellings of one server compare equal.
pub(super) fn same_server(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().to_ascii_lowercase();
        let (scheme, rest) = s.split_once("://").unwrap_or(("https", s.as_str()));
        let host = rest.split('/').next().unwrap_or("");
        let host = match scheme {
            "https" => host.strip_suffix(":443").unwrap_or(host),
            "http" => host.strip_suffix(":80").unwrap_or(host),
            _ => host,
        };
        format!("{scheme}://{host}")
    }
    norm(a) == norm(b)
}

/// What a link points at, in words.
pub(super) fn link_target(link: &Link) -> String {
    match link {
        Link::Revision { rev, .. } => format!("r{rev}"),
        Link::Shelf { id, .. } => format!("shelf {id}"),
        Link::File {
            path,
            rev: Some(rev),
            ..
        } => format!("{path} at r{rev}"),
        Link::File {
            path,
            branch: Some(b),
            ..
        } => format!("{path} on {b}"),
        Link::File { path, .. } => format!("{path} on its branch"),
    }
}

fn link_at(link: &Link) -> (&str, &str) {
    match link {
        Link::Revision { server, depot, .. }
        | Link::File { server, depot, .. }
        | Link::Shelf { server, depot, .. } => (server, depot),
    }
}

/// What an update check found, in words.
fn check_text(check: &UpdateCheck) -> String {
    match check {
        UpdateCheck::Off { reason } => {
            let mut c = reason.chars();
            match c.next() {
                Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str()),
                None => String::new(),
            }
        }
        UpdateCheck::Unconfigured => "This build does not update itself.".into(),
        UpdateCheck::Current { current } => format!("Lode Desk {current} is the newest version."),
        UpdateCheck::Available { version, .. } => format!("Lode Desk {version} is available."),
    }
}

fn sentence(why: &str) -> String {
    let mut c = why.chars();
    match c.next() {
        Some(f) => {
            let s = format!("{}{}", f.to_uppercase(), c.as_str());
            if s.ends_with('.') {
                s
            } else {
                format!("{s}.")
            }
        }
        None => String::new(),
    }
}

impl App {
    // --- Links handed to the app --------------------------------------------------------------

    /// Acts on a `lode://` link handed to the app: refused, or shown in a workspace of its
    /// depot (asking which one when several fit).
    pub(super) fn open_link_text(self: &Rc<Self>, text: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let link_dialogs = [
            Dialog::LinkRefused,
            Dialog::LinkChoose,
            Dialog::LinkNone,
            Dialog::About,
        ];
        let dialog = w.get_dialog();
        if dialog != Dialog::None && !link_dialogs.contains(&dialog) {
            // Never over a dialog the user is filling in: the link waits until it closes.
            *self.outside.queued.borrow_mut() = Some(text);
            return;
        }
        let l = w.global::<Links>();
        let link = match links::parse(&text) {
            Ok(link) => link,
            Err(e) => {
                let shown: String = text.chars().take(SHOWN_LINK).collect();
                l.set_link(LinkState {
                    text: if shown.len() < text.len() {
                        format!("{shown}…")
                    } else {
                        shown
                    }
                    .into(),
                    why: sentence(&e.0).into(),
                    ..LinkState::default()
                });
                w.set_dialog(Dialog::LinkRefused);
                return;
            }
        };
        let (server, depot) = link_at(&link);
        l.set_link(LinkState {
            text: text.as_str().into(),
            why: "".into(),
            target: link_target(&link).into(),
            depot: depot.into(),
            host: host_of(server).into(),
        });
        let mut all: Vec<LinkChoice> = Vec::new();
        let mut add = |path: &str, at: &str, d: &str, branch: &str, open: bool| {
            if d != depot || !same_server(at, server) || all.iter().any(|c| c.path == path) {
                return;
            }
            all.push(LinkChoice {
                path: path.into(),
                branch: branch.into(),
                open,
            });
        };
        for t in self.workspaces.iter() {
            add(&t.root, &t.server_url, &t.depot, &t.branch, true);
        }
        for r in self.desk.recent() {
            add(&r.path, &r.server, &r.depot, &r.branch, false);
        }
        // A branch link is best shown in a workspace on that branch.
        if let Link::File {
            branch: Some(b), ..
        } = &link
        {
            all.sort_by_key(|c| c.branch.as_str() != b.as_str());
        }
        match all.as_slice() {
            [] => {
                *self.outside.pending.borrow_mut() = Some(link);
                w.set_dialog(Dialog::LinkNone);
            }
            [one] => {
                let path = one.path.to_string();
                self.follow(link, path);
            }
            _ => {
                self.outside.choices.set_vec(all);
                *self.outside.pending.borrow_mut() = Some(link);
                w.set_dialog(Dialog::LinkChoose);
            }
        }
    }

    /// Shows a link that waited for a dialog to close.
    pub(super) fn queued_link(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        if w.get_dialog() == Dialog::None {
            if let Some(text) = self.outside.queued.borrow_mut().take() {
                self.open_link_text(text);
            }
        }
    }

    fn pick_workspace(self: &Rc<Self>, path: String) {
        if let Some(link) = self.outside.pending.borrow_mut().take() {
            self.follow(link, path);
        }
    }

    /// Shows what `link` points at in the workspace at `path`, opening it first if needed.
    fn follow(self: &Rc<Self>, link: Link, path: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        w.set_dialog(Dialog::None);
        if let Some(tab) = self.workspaces.iter().find(|t| t.root == path) {
            self.go(&w, &tab, &link);
            return;
        }
        let me = Rc::clone(self);
        spawn(async move {
            let result = me.desk.open_workspace(path).await;
            let Some(w) = me.window.upgrade() else { return };
            match result {
                Ok(info) => {
                    me.refresh_recent();
                    me.show_link_in(&w, &info, &link);
                }
                Err(e) => {
                    w.set_current_tab(0);
                    w.set_home_error(error_info(&e));
                }
            }
        });
    }

    /// Opens the workspace `info` describes (a new tab) on the view `link` points at.
    fn show_link_in(&self, w: &AppWindow, info: &Value, link: &Link) {
        let root = text(&info["root"]);
        self.prepare(&root, &text(&info["branch"]), link);
        self.show_workspace(w, info);
    }

    /// Keeps the view `link` points at as the one `root` shows next, and says when the link's
    /// branch is not the workspace's.
    fn prepare(&self, root: &str, branch: &str, link: &Link) -> View {
        let view = match link {
            Link::Revision { rev, .. } => View::History {
                path: None,
                rev: Some(*rev),
            },
            Link::File { path, rev, .. } => View::History {
                path: Some(path.clone()),
                rev: *rev,
            },
            Link::Shelf { id, .. } => {
                self.want_shelf(*id);
                View::Shelves
            }
        };
        self.work
            .borrow_mut()
            .entry(root.to_owned())
            .or_default()
            .view = view.clone();
        let notice = match link {
            Link::File {
                path,
                rev: None,
                branch: Some(b),
                ..
            } if b != branch => format!(
                "The link is for {path} on {b}, and this workspace is on {branch}, so its history on {branch} is shown."
            ),
            _ => String::new(),
        };
        if let Some(w) = self.window.upgrade() {
            w.global::<Links>().set_notice(notice.into());
        }
        view
    }

    fn go(&self, w: &AppWindow, tab: &WorkspaceTab, link: &Link) {
        let view = self.prepare(&tab.root, &tab.branch, link);
        if self.shown_root.borrow().as_deref() == Some(tab.root.as_str()) && w.get_current_tab() > 0
        {
            self.open_view(view);
        } else if let Some(i) = self.workspaces.iter().position(|t| t.root == tab.root) {
            w.set_current_tab(super::tab_number(i));
            self.show_root(w, Some(tab.root.to_string()));
        }
    }

    /// Selects shelf `id` the next time the shelves load, whoever owns it.
    fn want_shelf(&self, id: u64) {
        self.team_want(id);
        if let Some(w) = self.window.upgrade() {
            let t = w.global::<Team>();
            let mut s = t.get_shelves();
            s.whose = "all".into();
            t.set_shelves(s);
        }
    }

    /// The check-out dialog for the link's depot; the link opens once it is checked out.
    fn check_out_for_link(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let pending = self.outside.pending.borrow().clone();
        let Some(link) = pending else {
            w.set_dialog(Dialog::None);
            return;
        };
        let (server, depot) = link_at(&link);
        let branch = match &link {
            Link::File {
                branch: Some(b), ..
            } => b.clone(),
            _ => "main".into(),
        };
        w.set_dialog(Dialog::None);
        self.show_check_out();
        let mut state = w.get_checkout();
        state.target = format!("{}/{depot}", server.trim_end_matches('/')).into();
        state.branch = branch.into();
        w.set_checkout(state);
    }

    /// After a check out: opens the link it was for, when it is of that depot.
    pub(super) fn link_after_checkout(&self, w: &AppWindow, info: &Value) {
        let pending = self.outside.pending.borrow_mut().take();
        let Some(link) = pending else { return };
        let (server, depot) = link_at(&link);
        if text(&info["depot"]) == depot && same_server(&text(&info["server"]), server) {
            let root = text(&info["root"]);
            self.prepare(&root, &text(&info["branch"]), &link);
            self.restore_view(&root);
        } else {
            w.global::<Links>().set_notice("".into());
        }
    }

    // --- Copying links ---------------------------------------------------------------------

    fn copy_link(&self, make: impl FnOnce(String, String) -> Link, target: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(tab) = self
            .shown_root
            .borrow()
            .as_deref()
            .and_then(|r| self.workspaces_row(r))
        else {
            return;
        };
        let link = make(tab.server_url.to_string(), tab.depot.to_string());
        match links::build(&link) {
            Ok(text) => {
                w.invoke_copy_text(text.as_str().into());
                let l = w.global::<Links>();
                l.set_copied(target.as_str().into());
                l.set_copied_link(text.into());
                let weak = w.as_weak();
                self.outside.copied_timer.start(
                    slint::TimerMode::SingleShot,
                    COPIED_FOR,
                    move || {
                        if let Some(w) = weak.upgrade() {
                            w.global::<Links>().set_copied("".into());
                        }
                    },
                );
            }
            Err(e) => w.set_workspace_error(ErrorInfo {
                shown: true,
                message: format!("No link to {target}: {e}").into(),
                next: "".into(),
                code: "usage".into(),
            }),
        }
    }

    // --- Updates ---------------------------------------------------------------------------

    /// Looks for a new version; `asked` when the user pressed "Check for updates now", which
    /// shows the result and runs even with the setting off.
    pub(super) fn check_updates(self: &Rc<Self>, asked: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let u = w.global::<Updates>();
        if asked {
            let mut s = u.get_settings();
            if s.checking {
                return;
            }
            s.checking = true;
            s.result = "".into();
            s.error = ErrorInfo::default();
            u.set_settings(s);
        }
        let updater = self.outside.updater.clone();
        let me = Rc::clone(self);
        spawn(async move {
            let env = std::env::var_os(update::NO_UPDATE_ENV);
            let result =
                off_thread(move || async move { updater.check(asked, env.as_deref()).await }).await;
            let Some(w) = me.window.upgrade() else { return };
            let u = w.global::<Updates>();
            let mut s = u.get_settings();
            if asked {
                s.checking = false;
            }
            match result {
                Ok((check, found)) => {
                    if asked {
                        s.result = check_text(&check).into();
                    }
                    if let (
                        UpdateCheck::Available {
                            current,
                            version,
                            notes,
                            ..
                        },
                        Some(found),
                    ) = (check, found)
                    {
                        let later = me.outside.later.borrow().clone();
                        // "Later" holds for that version until the user asks again.
                        if asked || later.as_deref() != Some(version.as_str()) {
                            *me.outside.found.borrow_mut() = Some(found);
                            u.set_offer(UpdateOffer {
                                shown: true,
                                version: version.into(),
                                current: current.into(),
                                notes: notes.into(),
                                busy: false,
                                error: ErrorInfo::default(),
                            });
                        }
                    }
                }
                // A failed daily check stays quiet; "Check for updates now" shows why.
                Err(e) if asked => s.error = error_info(&e),
                Err(_) => {}
            }
            u.set_settings(s);
        });
    }

    fn set_check_updates(&self, on: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let u = w.global::<Updates>();
        let mut s = u.get_settings();
        let file = &self.outside.updater.settings_file;
        let mut settings = update::load_settings(file);
        settings.check_updates = on;
        match update::save_settings(file, &settings) {
            Ok(()) => s.check = on,
            Err(e) => s.error = error_info(&e),
        }
        u.set_settings(s);
    }

    fn update_later(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let u = w.global::<Updates>();
        let offer = u.get_offer();
        *self.outside.later.borrow_mut() = Some(offer.version.to_string());
        u.set_offer(UpdateOffer::default());
    }

    /// Downloads the new version, checks its signature, installs it and ends this copy (the
    /// installer starts the new one).
    fn install_update(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let u = w.global::<Updates>();
        let mut offer = u.get_offer();
        if offer.busy {
            return;
        }
        let Some(found) = self.outside.found.borrow().clone() else {
            return;
        };
        // Never while a branch operation or a commit runs: the installer ends the app.
        let b = w.global::<Branching>();
        let running = !b.get_branches().busy.is_empty()
            || b.get_new_branch().busy
            || b.get_merge().busy
            || b.get_view().applying
            || !w.get_changes().busy.is_empty()
            || w.get_checkout().busy;
        if running {
            offer.error = error_info(&ErrorView {
                code: "busy".into(),
                message: "an operation is still running; install the update when it is done".into(),
                details: Value::Null,
            });
            u.set_offer(offer);
            return;
        }
        offer.busy = true;
        offer.error = ErrorInfo::default();
        u.set_offer(offer);
        let updater = self.outside.updater.clone();
        let me = Rc::clone(self);
        spawn(async move {
            let result = off_thread(move || async move {
                let bytes = updater.download(&found).await?;
                update::install(&update::this_install(), &bytes)
            })
            .await;
            let Some(w) = me.window.upgrade() else { return };
            match result {
                Ok(()) => {
                    let _ = slint::quit_event_loop();
                }
                Err(e) => {
                    let u = w.global::<Updates>();
                    let mut offer = u.get_offer();
                    offer.busy = false;
                    offer.error = error_info(&e);
                    u.set_offer(offer);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_compare_without_case_default_ports_or_slashes() {
        assert!(same_server(
            "https://Lode.Example.com/",
            "https://lode.example.com"
        ));
        assert!(same_server("https://h:443", "https://h"));
        assert!(same_server("http://h:80/", "http://h"));
        assert!(!same_server("http://h", "https://h"));
        assert!(!same_server("https://h:8443", "https://h"));
    }

    #[test]
    fn targets_and_results_read_as_sentences() {
        let file = |rev, branch: Option<&str>| Link::File {
            server: "https://h".into(),
            depot: "game".into(),
            path: "a/b.txt".into(),
            rev,
            branch: branch.map(Into::into),
        };
        assert_eq!(link_target(&file(Some(3), None)), "a/b.txt at r3");
        assert_eq!(link_target(&file(None, Some("dev"))), "a/b.txt on dev");
        assert_eq!(sentence("not a lode:// link"), "Not a lode:// link.");
        assert_eq!(
            check_text(&UpdateCheck::Off {
                reason: "checking is off".into()
            }),
            "Checking is off."
        );
        assert_eq!(
            check_text(&UpdateCheck::Current {
                current: "0.2.0".into()
            }),
            "Lode Desk 0.2.0 is the newest version."
        );
    }
}
