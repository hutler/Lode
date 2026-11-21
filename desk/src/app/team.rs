//! Locks, shelves and their checks (T133). The views draw the global `Team`; this module fills
//! it and answers its callbacks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::Value;
use slint::{ComponentHandle, SharedString, VecModel};

use super::{error_info, spawn, App, AppWindow, Dialog, ErrorInfo, Reading};
use super::{CheckRow, ConfirmState, DiffRow, LockDialogState, LockRow, LocksState};
use super::{ShelfDetail, ShelfFileRow, ShelfRow, ShelveDialogState, ShelvesState, SubmitState};
use super::{Team, UnshelvedState};
use crate::desk::{ErrorView, ShelveRequest};
use crate::team;

/// What the confirm dialog does when confirmed.
#[derive(Debug, Clone, Default)]
pub(super) enum Confirm {
    #[default]
    None,
    BreakLock(String),
    DeleteShelf(u64),
    /// Archive a branch (T134).
    ArchiveBranch(String),
    /// Apply the edited view, dropping local changes in the way (T134).
    ForceView,
}

/// What the shelve dialog shelves.
#[derive(Debug, Clone, Default)]
enum ShelveWhat {
    #[default]
    None,
    Files {
        paths: Vec<String>,
        untracked: Vec<String>,
    },
    List(String),
}

/// The models and loaded state behind `Team`.
pub(super) struct TeamState {
    lock_rows: Rc<VecModel<LockRow>>,
    shelf_rows: Rc<VecModel<ShelfRow>>,
    shelf_files: Rc<VecModel<ShelfFileRow>>,
    shelf_checks: Rc<VecModel<CheckRow>>,
    shelf_diff: Rc<VecModel<DiffRow>>,
    shelve_targets: Rc<VecModel<SharedString>>,
    locks: RefCell<Vec<Value>>,
    shelves: RefCell<Vec<Value>>,
    shelf: RefCell<Value>,
    selected: Cell<Option<u64>>,
    /// A shelf a link asked for: selected when the shelves load, even when the list leaves
    /// it out.
    wanted: Cell<Option<u64>>,
    checks: RefCell<Value>,
    /// The signed-in name on the server of the workspace (root, name), to mark one's own.
    me: RefCell<(String, String)>,
    my_open: RefCell<Vec<Value>>,
    lock_paths: RefCell<Vec<String>>,
    confirm: RefCell<Confirm>,
    shelve: RefCell<ShelveWhat>,
    generation: Cell<u64>,
}

impl TeamState {
    pub(super) fn new(window: &AppWindow) -> Self {
        let st = Self {
            lock_rows: Rc::new(VecModel::default()),
            shelf_rows: Rc::new(VecModel::default()),
            shelf_files: Rc::new(VecModel::default()),
            shelf_checks: Rc::new(VecModel::default()),
            shelf_diff: Rc::new(VecModel::default()),
            shelve_targets: Rc::new(VecModel::default()),
            locks: RefCell::new(Vec::new()),
            shelves: RefCell::new(Vec::new()),
            shelf: RefCell::new(Value::Null),
            selected: Cell::new(None),
            wanted: Cell::new(None),
            checks: RefCell::new(Value::Null),
            me: RefCell::new((String::new(), String::new())),
            my_open: RefCell::new(Vec::new()),
            lock_paths: RefCell::new(Vec::new()),
            confirm: RefCell::new(Confirm::None),
            shelve: RefCell::new(ShelveWhat::None),
            generation: Cell::new(0),
        };
        let t = window.global::<Team>();
        t.set_lock_rows(Rc::clone(&st.lock_rows).into());
        t.set_shelf_rows(Rc::clone(&st.shelf_rows).into());
        t.set_shelf_files(Rc::clone(&st.shelf_files).into());
        t.set_shelf_checks(Rc::clone(&st.shelf_checks).into());
        t.set_shelf_diff(Rc::clone(&st.shelf_diff).into());
        t.set_shelve_targets(Rc::clone(&st.shelve_targets).into());
        t.set_shelves(ShelvesState {
            whose: "all".into(),
            state: "open".into(),
            ..ShelvesState::default()
        });
        st
    }
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>) {
    let t = window.global::<Team>();
    let a = Rc::clone(app);
    t.on_refresh_locks(move || a.load_locks());
    let a = Rc::clone(app);
    t.on_filter_locks(move || a.render_locks());
    let a = Rc::clone(app);
    t.on_unlock(move |p| a.release(vec![p.to_string()], false));
    let a = Rc::clone(app);
    t.on_unlock_mine(move || {
        let mine: Vec<String> = a
            .team
            .locks
            .borrow()
            .iter()
            .filter(|l| l["mine"].as_bool() == Some(true))
            .filter_map(|l| l["path"].as_str().map(str::to_owned))
            .collect();
        a.release(mine, false);
    });
    let a = Rc::clone(app);
    t.on_ask_break(move |p| a.ask_break(&p));
    let a = Rc::clone(app);
    t.on_confirmed(move || a.confirmed());
    let a = Rc::clone(app);
    t.on_lock_selected(move || a.ask_lock());
    let a = Rc::clone(app);
    t.on_unlock_selected(move || a.unlock_selected());
    let a = Rc::clone(app);
    t.on_submit_lock(move || a.submit_lock());
    let a = Rc::clone(app);
    t.on_refresh_shelves(move || a.load_shelves());
    let a = Rc::clone(app);
    t.on_set_whose(move |w| {
        a.set_shelves_filter(|s| s.whose = w.clone());
    });
    let a = Rc::clone(app);
    t.on_set_state(move |st| {
        a.set_shelves_filter(|s| s.state = st.clone());
    });
    let a = Rc::clone(app);
    t.on_select_shelf(move |id| a.select_shelf(u64::try_from(id).ok()));
    let a = Rc::clone(app);
    t.on_unshelve(move || a.unshelve());
    let a = Rc::clone(app);
    t.on_ask_submit(move || a.ask_submit());
    let a = Rc::clone(app);
    t.on_submit_shelf(move || a.submit_shelf());
    let a = Rc::clone(app);
    t.on_review_new_version(move || {
        if let Some(w) = a.window.upgrade() {
            w.set_dialog(Dialog::None);
        }
        a.load_shelves();
    });
    let a = Rc::clone(app);
    t.on_ask_delete_shelf(move || a.ask_delete_shelf());
    let a = Rc::clone(app);
    t.on_shelve_picked(move || {
        let what = ShelveWhat::Files {
            paths: a.picked_tracked(),
            untracked: a.picked_untracked(),
        };
        a.ask_shelve(what);
    });
    let a = Rc::clone(app);
    t.on_shelve_changelist(move |n| a.ask_shelve(ShelveWhat::List(n.to_string())));
    let a = Rc::clone(app);
    t.on_submit_shelve(move || a.submit_shelve());
    let a = Rc::clone(app);
    t.on_target_chosen(move |i| a.target_chosen(i));
    let a = Rc::clone(app);
    t.on_set_layout(move |l| a.set_shelf_layout(&l));
    let a = Rc::clone(app);
    t.on_dismiss_error(move || {
        if let Some(w) = a.window.upgrade() {
            let t = w.global::<Team>();
            let mut l = t.get_locks();
            l.error = ErrorInfo::default();
            t.set_locks(l);
            let mut s = t.get_shelves();
            s.error = ErrorInfo::default();
            t.set_shelves(s);
        }
    });
    let a = Rc::clone(app);
    window.global::<Reading>().on_open_link(move |url| {
        if let Err(e) = crate::desk::open_link(&url) {
            if let Some(w) = a.window.upgrade() {
                w.set_workspace_error(error_info(&e));
            }
        }
    });
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The paths an error names in `details.paths`.
fn error_paths(e: &ErrorView) -> Vec<String> {
    e.details["paths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl App {
    fn team_root(&self) -> Option<(AppWindow, String)> {
        let w = self.window.upgrade()?;
        let root = self.shown_root.borrow().clone()?;
        Some((w, root))
    }

    fn team_current(&self, generation: u64) -> bool {
        self.team.generation.get() == generation
    }

    // --- Locks -----------------------------------------------------------------------------

    pub(super) fn load_locks(&self) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let generation = self.team.generation.get() + 1;
        self.team.generation.set(generation);
        let t = w.global::<Team>();
        let mut l = t.get_locks();
        l.busy = true;
        t.set_locks(l);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.locks(root, false).await;
            if !me.team_current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            let mut l = LocksState {
                loaded: true,
                ..LocksState::default()
            };
            match got {
                Ok(v) => {
                    *me.team.locks.borrow_mut() =
                        v["locks"].as_array().cloned().unwrap_or_default();
                }
                Err(e) if e.code == "module_disabled" => {
                    me.team.locks.borrow_mut().clear();
                    l.unavailable = true;
                }
                Err(e) => l.error = error_info(&e),
            }
            t.set_locks(l);
            me.render_locks();
        });
    }

    fn render_locks(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let locks = self.team.locks.borrow();
        let mut l = t.get_locks();
        l.held = i32::try_from(locks.len()).unwrap_or(i32::MAX);
        l.mine = i32::try_from(
            locks
                .iter()
                .filter(|l| l["mine"].as_bool() == Some(true))
                .count(),
        )
        .unwrap_or(i32::MAX);
        t.set_locks(l);
        self.team.lock_rows.set_vec(team::lock_rows(
            &locks,
            &t.get_lock_filter(),
            t.get_mine_only(),
        ));
    }

    /// Releases locks (`force` breaks other people's), then lists them again.
    fn release(&self, paths: Vec<String>, force: bool) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let t = w.global::<Team>();
        let mut l = t.get_locks();
        l.busy = true;
        l.error = ErrorInfo::default();
        t.set_locks(l);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.unlock(root.clone(), paths, force).await;
            let Some(w) = me.window.upgrade() else { return };
            match got {
                Ok(_) => {
                    if w.get_dialog() == Dialog::Confirm {
                        w.set_dialog(Dialog::None);
                    }
                }
                Err(e) => {
                    if w.get_dialog() == Dialog::Confirm {
                        let t = w.global::<Team>();
                        let mut c = t.get_confirm();
                        c.busy = false;
                        c.error = error_info(&e);
                        t.set_confirm(c);
                    } else {
                        let t = w.global::<Team>();
                        let mut l = t.get_locks();
                        l.error = error_info(&e);
                        t.set_locks(l);
                    }
                }
            }
            me.load_locks();
            me.refresh(root);
        });
    }

    fn ask_break(&self, path: &str) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let locks = self.team.locks.borrow();
        let Some(l) = locks.iter().find(|l| l["path"].as_str() == Some(path)) else {
            return;
        };
        let owner = l["owner"]["name"].as_str().unwrap_or("");
        let comment = l["comment"]
            .as_str()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default();
        *self.team.confirm.borrow_mut() = Confirm::BreakLock(path.to_owned());
        w.global::<Team>().set_confirm(ConfirmState {
            title: "Break this lock?".into(),
            body: format!(
                "{path} is locked by {owner}{comment}. Breaking it lets anyone commit the file, and \
                 {owner} may lose work if they commit after someone else. Only depot admins can \
                 break locks."
            )
            .into(),
            confirm: "Break lock".into(),
            ..ConfirmState::default()
        });
        w.set_dialog(Dialog::Confirm);
    }

    fn confirmed(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let mut c = t.get_confirm();
        c.busy = true;
        c.error = ErrorInfo::default();
        t.set_confirm(c);
        let action = self.team.confirm.borrow().clone();
        match action {
            Confirm::None => w.set_dialog(Dialog::None),
            Confirm::BreakLock(p) => self.release(vec![p], true),
            Confirm::DeleteShelf(id) => self.delete_shelf(id),
            Confirm::ArchiveBranch(name) => self.archive_branch(name),
            Confirm::ForceView => self.apply_view(true),
        }
    }

    /// Asks `title` with `body`, doing `action` when `confirm` is pressed.
    pub(super) fn ask_confirm(
        &self,
        action: Confirm,
        title: String,
        body: String,
        confirm: String,
    ) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        *self.team.confirm.borrow_mut() = action;
        w.global::<Team>().set_confirm(ConfirmState {
            title: title.into(),
            body: body.into(),
            confirm: confirm.into(),
            ..ConfirmState::default()
        });
        w.set_dialog(Dialog::Confirm);
    }

    /// Shows why a confirmed action failed, in the confirm dialog.
    pub(super) fn confirm_failed(&self, e: &ErrorView) {
        if let Some(w) = self.window.upgrade() {
            let t = w.global::<Team>();
            let mut c = t.get_confirm();
            c.busy = false;
            c.error = error_info(e);
            t.set_confirm(c);
        }
    }

    fn ask_lock(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(path) = self.selected_file() else {
            return;
        };
        *self.team.lock_paths.borrow_mut() = vec![path.clone()];
        let t = w.global::<Team>();
        t.set_lock_comment(SharedString::new());
        t.set_lock_dialog(LockDialogState {
            title: "Lock file".into(),
            what: path.into(),
            ..LockDialogState::default()
        });
        w.set_dialog(Dialog::Lock);
    }

    fn submit_lock(&self) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let t = w.global::<Team>();
        let mut d = t.get_lock_dialog();
        if d.busy {
            return;
        }
        d.busy = true;
        d.error = ErrorInfo::default();
        d.holders = SharedString::new();
        t.set_lock_dialog(d);
        let comment = t.get_lock_comment().trim().to_owned();
        let paths = self.team.lock_paths.borrow().clone();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me
                .desk
                .lock(
                    root.clone(),
                    paths,
                    (!comment.is_empty()).then_some(comment),
                )
                .await;
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            match got {
                Ok(_) => {
                    if w.get_dialog() == Dialog::Lock {
                        w.set_dialog(Dialog::None);
                    }
                    me.changes_done("Locked.");
                    me.refresh(root);
                }
                Err(e) => {
                    let mut d = t.get_lock_dialog();
                    d.busy = false;
                    d.error = error_info(&e);
                    t.set_lock_dialog(d);
                    if e.code == "locked" {
                        let named = error_paths(&e);
                        if let Ok(v) = me.desk.locks(root, false).await {
                            let all = v["locks"].as_array().cloned().unwrap_or_default();
                            let mut d = t.get_lock_dialog();
                            d.holders = team::holders(&all, &named).into();
                            t.set_lock_dialog(d);
                        }
                    }
                }
            }
        });
    }

    fn unlock_selected(&self) {
        let Some(path) = self.selected_file() else {
            return;
        };
        let Some((_, root)) = self.team_root() else {
            return;
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            match me.desk.unlock(root.clone(), vec![path], false).await {
                Ok(_) => me.changes_done("Unlocked."),
                Err(e) => {
                    if let Some(w) = me.window.upgrade() {
                        let mut c = w.get_changes();
                        c.error = error_info(&e);
                        w.set_changes(c);
                    }
                }
            }
            me.refresh(root);
        });
    }

    // --- Shelves ---------------------------------------------------------------------------

    fn set_shelves_filter(&self, f: impl FnOnce(&mut ShelvesState)) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let mut s = t.get_shelves();
        f(&mut s);
        t.set_shelves(s);
        self.load_shelves();
    }

    pub(super) fn load_shelves(&self) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let generation = self.team.generation.get() + 1;
        self.team.generation.set(generation);
        let t = w.global::<Team>();
        let s = t.get_shelves();
        let mine = s.whose == "mine";
        let state = s.state.to_string();
        let Some(me) = self.me() else { return };
        spawn(async move {
            if me.team.me.borrow().0 != root {
                if let Ok(who) = me.desk.whoami(root.clone()).await {
                    *me.team.me.borrow_mut() =
                        (root.clone(), who["name"].as_str().unwrap_or("").to_owned());
                }
            }
            let got = me.desk.shelves(root.clone(), mine, Some(state)).await;
            if !me.team_current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            let mut s = t.get_shelves();
            s.loaded = true;
            s.unavailable = false;
            match got {
                Ok(v) => {
                    let list = v["shelves"].as_array().cloned().unwrap_or_default();
                    let keep = me
                        .team
                        .selected
                        .get()
                        .filter(|id| list.iter().any(|x| x["id"].as_u64() == Some(*id)));
                    let selected = me
                        .team
                        .wanted
                        .take()
                        .or(keep)
                        .or_else(|| list.first().and_then(|x| x["id"].as_u64()));
                    *me.team.shelves.borrow_mut() = list;
                    me.team.selected.set(selected);
                    t.set_shelves(s);
                    me.render_shelves();
                    me.load_shelf(root.clone(), selected, generation);
                    me.load_shelf_checks(root, generation);
                    return;
                }
                Err(e) if e.code == "module_disabled" => s.unavailable = true,
                Err(e) => s.error = error_info(&e),
            }
            t.set_shelves(s);
        });
    }

    fn render_shelves(&self) {
        let rows = team::shelf_rows(
            &self.team.shelves.borrow(),
            &self.team.me.borrow().1,
            self.team.selected.get(),
            &self.team.checks.borrow(),
        );
        self.team.shelf_rows.set_vec(rows);
        let shelf = self.team.shelf.borrow();
        if !shelf.is_null() {
            self.team
                .shelf_checks
                .set_vec(team::check_rows(team::checks_of(
                    &self.team.checks.borrow(),
                    &team::shelf_target(&shelf),
                )));
        }
    }

    /// Selects shelf `id` the next time the shelves load (a `lode://` link to it).
    pub(super) fn team_want(&self, id: u64) {
        self.team.wanted.set(Some(id));
        self.team.selected.set(Some(id));
    }

    fn select_shelf(&self, id: Option<u64>) {
        let Some((_, root)) = self.team_root() else {
            return;
        };
        self.team.selected.set(id);
        self.render_shelves();
        self.load_shelf(root.clone(), id, self.team.generation.get());
        self.load_shelf_checks(root, self.team.generation.get());
    }

    fn load_shelf(&self, root: String, id: Option<u64>, generation: u64) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let Some(id) = id else {
            *self.team.shelf.borrow_mut() = Value::Null;
            t.set_shelf(ShelfDetail::default());
            self.team.shelf_files.set_vec(Vec::new());
            self.team.shelf_diff.set_vec(Vec::new());
            self.team.shelf_checks.set_vec(Vec::new());
            return;
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.shelf(root, id).await;
            if !me.team_current(generation) || me.team.selected.get() != Some(id) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            match got {
                Ok(v) => {
                    let (d, files) = team::shelf_detail(&v, &me.team.me.borrow().1);
                    t.set_shelf(d);
                    me.team.shelf_files.set_vec(files);
                    me.team
                        .shelf_diff
                        .set_vec(team::shelf_diff_rows(&v, me.read_layout()));
                    let mut s = t.get_shelves();
                    s.layout = me.read_layout().name().into();
                    t.set_shelves(s);
                    *me.team.shelf.borrow_mut() = v;
                    me.render_shelves();
                }
                Err(e) => {
                    let mut s = t.get_shelves();
                    s.error = error_info(&e);
                    t.set_shelves(s);
                }
            }
        });
    }

    /// Loads the checks of the listed shelves and the one shown.
    fn load_shelf_checks(&self, root: String, generation: u64) {
        let mut targets: Vec<String> = self
            .team
            .shelves
            .borrow()
            .iter()
            .map(team::shelf_target)
            .collect();
        let shelf = self.team.shelf.borrow();
        if !shelf.is_null() {
            let t = team::shelf_target(&shelf);
            if !targets.contains(&t) {
                targets.push(t);
            }
        }
        drop(shelf);
        if targets.is_empty() {
            return;
        }
        let Some(me) = self.me() else { return };
        spawn(async move {
            let Ok(v) = me.desk.checks(root, targets).await else {
                return;
            };
            if !me.team_current(generation) {
                return;
            }
            *me.team.checks.borrow_mut() = v;
            me.render_shelves();
        });
    }

    fn set_shelf_layout(&self, layout: &str) {
        let layout = crate::diffs::Layout::parse(layout);
        self.set_read_layout(layout);
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let mut s = t.get_shelves();
        s.layout = layout.name().into();
        t.set_shelves(s);
        let shelf = self.team.shelf.borrow();
        if !shelf.is_null() {
            self.team
                .shelf_diff
                .set_vec(team::shelf_diff_rows(&shelf, layout));
        }
    }

    fn unshelve(&self) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let t = w.global::<Team>();
        let mut d = t.get_shelf();
        if !d.shown || !d.busy.is_empty() {
            return;
        }
        let id = u64::try_from(d.id).unwrap_or(0);
        d.busy = "Unshelving".into();
        t.set_shelf(d);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.unshelve(root.clone(), id).await;
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            let mut d = t.get_shelf();
            d.busy = SharedString::new();
            t.set_shelf(d);
            match got {
                Ok(v) => {
                    let (counts, files, conflicts) = team::unshelved(&v);
                    t.set_unshelved(UnshelvedState {
                        title: format!("Unshelved shelf {id}").into(),
                        counts: counts.into(),
                        files: files.into(),
                        conflicts,
                    });
                    w.set_dialog(Dialog::Unshelved);
                    me.refresh(root);
                }
                Err(e) => {
                    let mut s = t.get_shelves();
                    s.error = error_info(&e);
                    t.set_shelves(s);
                }
            }
        });
    }

    fn ask_submit(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        let d = t.get_shelf();
        if !d.shown {
            return;
        }
        let shelf = self.team.shelf.borrow();
        let description = shelf["description"].as_str().unwrap_or("");
        t.set_submit_text(description.into());
        t.set_submit(SubmitState {
            title: format!("Submit shelf {}, version {}", d.id, d.version).into(),
            note: format!(
                "Commits the shelf on {} exactly as shown. You are the submitter; {} stays the author.",
                shelf["branch"].as_str().unwrap_or(""),
                d.owner
            )
            .into(),
            ..SubmitState::default()
        });
        w.set_dialog(Dialog::SubmitShelf);
    }

    fn submit_shelf(&self) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let t = w.global::<Team>();
        let mut st = t.get_submit();
        if st.busy {
            return;
        }
        st.busy = true;
        st.error = ErrorInfo::default();
        st.missing = SharedString::new();
        t.set_submit(st);
        let d = t.get_shelf();
        let (id, version) = (
            u64::try_from(d.id).unwrap_or(0),
            u64::try_from(d.version).unwrap_or(0),
        );
        let text = t.get_submit_text().trim().to_owned();
        let original = self.team.shelf.borrow()["description"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let message = (text != original).then_some(text);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.shelf_submit(root, id, version, message).await;
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            match got {
                Ok(v) => {
                    if w.get_dialog() == Dialog::SubmitShelf {
                        w.set_dialog(Dialog::None);
                    }
                    let mut s = t.get_shelves();
                    s.notice = format!(
                        "Submitted shelf {id} as r{} on {}.",
                        v["rev"].as_u64().unwrap_or(0),
                        v["branch"].as_str().unwrap_or("")
                    )
                    .into();
                    t.set_shelves(s);
                    me.load_shelves();
                }
                Err(e) => {
                    let mut st = t.get_submit();
                    st.busy = false;
                    st.stale = e.code == "conflict";
                    if e.code == "checks_required" {
                        st.missing = team::missing_checks(&e.details).into();
                        st.error = ErrorInfo {
                            shown: true,
                            message: format!(
                                "Required checks have not passed for version {version}."
                            )
                            .into(),
                            next: "Submit again once they pass.".into(),
                            code: "checks_required".into(),
                        };
                    } else {
                        st.error = error_info(&e);
                    }
                    t.set_submit(st);
                }
            }
        });
    }

    fn ask_delete_shelf(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let d = w.global::<Team>().get_shelf();
        if !d.shown {
            return;
        }
        *self.team.confirm.borrow_mut() = Confirm::DeleteShelf(u64::try_from(d.id).unwrap_or(0));
        w.global::<Team>().set_confirm(ConfirmState {
            title: format!("Delete shelf {}?", d.id).into(),
            body: "Nobody will be able to unshelve or submit it any more. Changes still in your \
                   workspace are not touched."
                .into(),
            confirm: "Delete shelf".into(),
            ..ConfirmState::default()
        });
        w.set_dialog(Dialog::Confirm);
    }

    fn delete_shelf(&self, id: u64) {
        let Some((_, root)) = self.team_root() else {
            return;
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.shelf_delete(root, id).await;
            let Some(w) = me.window.upgrade() else { return };
            let t = w.global::<Team>();
            match got {
                Ok(_) => {
                    if w.get_dialog() == Dialog::Confirm {
                        w.set_dialog(Dialog::None);
                    }
                    let mut s = t.get_shelves();
                    s.notice = format!("Deleted shelf {id}.").into();
                    t.set_shelves(s);
                    me.team.selected.set(None);
                    me.load_shelves();
                }
                Err(e) => {
                    let mut c = t.get_confirm();
                    c.busy = false;
                    c.error = error_info(&e);
                    t.set_confirm(c);
                }
            }
        });
    }

    fn ask_shelve(&self, what: ShelveWhat) {
        let Some((w, root)) = self.team_root() else {
            return;
        };
        let (title, text) = match &what {
            ShelveWhat::Files { paths, untracked } => (
                format!("Shelve {}", plural(paths.len() + untracked.len(), "file")),
                String::new(),
            ),
            ShelveWhat::List(name) => (
                format!("Shelve changelist {name}"),
                self.list_contents(name).0,
            ),
            ShelveWhat::None => return,
        };
        *self.team.shelve.borrow_mut() = what;
        let t = w.global::<Team>();
        t.set_shelve(ShelveDialogState {
            title: title.into(),
            ..ShelveDialogState::default()
        });
        t.set_shelve_text(text.into());
        t.set_shelve_keep(false);
        t.set_shelve_target(0);
        self.team
            .shelve_targets
            .set_vec(vec![SharedString::from("A new shelf")]);
        self.team.my_open.borrow_mut().clear();
        w.set_dialog(Dialog::Shelve);
        // My open shelves, to update one instead; without them only a new shelf is offered.
        let Some(me) = self.me() else { return };
        spawn(async move {
            let Ok(v) = me.desk.shelves(root, true, Some("open".into())).await else {
                return;
            };
            let mine = v["shelves"].as_array().cloned().unwrap_or_default();
            let mut labels = vec![SharedString::from("A new shelf")];
            labels.extend(mine.iter().map(|s| {
                SharedString::from(format!(
                    "Shelf {} (version {}): {}",
                    s["id"].as_u64().unwrap_or(0),
                    s["version"].as_u64().unwrap_or(0),
                    crate::history::summary(s["description"].as_str().unwrap_or(""))
                ))
            }));
            *me.team.my_open.borrow_mut() = mine;
            me.team.shelve_targets.set_vec(labels);
        });
    }

    fn target_chosen(&self, index: i32) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let t = w.global::<Team>();
        t.set_shelve_target(index);
        let pick = usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.team.my_open.borrow().get(i).cloned());
        if let Some(s) = pick {
            if t.get_shelve_text().trim().is_empty() {
                t.set_shelve_text(s["description"].as_str().unwrap_or("").into());
            }
        }
    }

    fn submit_shelve(&self) {
        let Some((w, _)) = self.team_root() else {
            return;
        };
        let t = w.global::<Team>();
        let d = t.get_shelve();
        if d.busy {
            return;
        }
        // Validation owns the modal while this request is in flight.
        let update = usize::try_from(t.get_shelve_target())
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| {
                self.team
                    .my_open
                    .borrow()
                    .get(i)
                    .and_then(|s| s["id"].as_u64())
            });
        let text = t.get_shelve_text().trim().to_owned();
        let keep = t.get_shelve_keep();
        let what = self.team.shelve.borrow().clone();
        let mut request = ShelveRequest {
            message: (!text.is_empty()).then_some(text),
            keep,
            update,
            ..Default::default()
        };
        let mut add = Vec::new();
        match what {
            ShelveWhat::Files { paths, untracked } => {
                request.paths = paths.into_iter().chain(untracked.iter().cloned()).collect();
                add = untracked;
            }
            ShelveWhat::List(name) => request.changelist = Some(name),
            ShelveWhat::None => {}
        }
        self.start_validation(crate::validation::Action::Shelve, request, add);
    }
}
