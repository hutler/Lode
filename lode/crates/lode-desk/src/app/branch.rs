//! Branches, switches, merges, conflicts and the workspace view (T134). The dialogs and views
//! draw the global `Branching`; this module fills it and answers its callbacks.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde_json::Value;
use slint::{ComponentHandle, SharedString, VecModel};

use super::read::View;
use super::team::Confirm;
use super::{error_info, human_bytes, spawn, App, AppWindow, Dialog, ErrorInfo};
use super::{BranchRow, BranchesState, Branching, ConflictState, FolderRow, MergeState};
use super::{NewBranchState, OutcomeRow, OutcomeState, RegionRow, SideLine, ViewState};
use crate::branches::{self, parse_rev, rev_ok, RevField};
use crate::desk::{ErrorView, Using};
use crate::merge::{self, Take, COMPARE_LIMIT};
use crate::views::{self, Spec};

/// How many files an outcome lists before "and N more".
const OUTCOME_LIMIT: usize = 40;

/// The models and loaded state behind `Branching`.
pub(super) struct BranchState {
    rows: Rc<VecModel<BranchRow>>,
    /// The depot's branches as `Desk::branches` lists them.
    list: RefCell<Vec<Value>>,
    bases: Rc<VecModel<SharedString>>,
    base_names: RefCell<Vec<String>>,
    sources: Rc<VecModel<SharedString>>,
    source_names: RefCell<Vec<String>>,
    outcome_rows: Rc<VecModel<OutcomeRow>>,
    base_lines: Rc<VecModel<SideLine>>,
    mine_lines: Rc<VecModel<SideLine>>,
    theirs_lines: Rc<VecModel<SideLine>>,
    regions: Rc<VecModel<RegionRow>>,
    folders: Rc<VecModel<FolderRow>>,
    /// The depot's top folder, and the folders opened in the tree, by path.
    roots: RefCell<Vec<Value>>,
    children: RefCell<HashMap<String, Vec<Value>>>,
    open: RefCell<BTreeSet<String>>,
    /// The patterns being edited, and the workspace's.
    spec: RefCell<Spec>,
    saved: RefCell<Spec>,
    /// Bumped each time the branch list, a conflict or the view loads.
    generation: Cell<u64>,
}

impl BranchState {
    pub(super) fn new(window: &AppWindow) -> Self {
        let st = Self {
            rows: Rc::new(VecModel::default()),
            list: RefCell::new(Vec::new()),
            bases: Rc::new(VecModel::default()),
            base_names: RefCell::new(Vec::new()),
            sources: Rc::new(VecModel::default()),
            source_names: RefCell::new(Vec::new()),
            outcome_rows: Rc::new(VecModel::default()),
            base_lines: Rc::new(VecModel::default()),
            mine_lines: Rc::new(VecModel::default()),
            theirs_lines: Rc::new(VecModel::default()),
            regions: Rc::new(VecModel::default()),
            folders: Rc::new(VecModel::default()),
            roots: RefCell::new(Vec::new()),
            children: RefCell::new(HashMap::new()),
            open: RefCell::new(BTreeSet::new()),
            spec: RefCell::new(Spec::default()),
            saved: RefCell::new(Spec::default()),
            generation: Cell::new(0),
        };
        let b = window.global::<Branching>();
        b.set_rows(Rc::clone(&st.rows).into());
        b.set_bases(Rc::clone(&st.bases).into());
        b.set_sources(Rc::clone(&st.sources).into());
        b.set_outcome_rows(Rc::clone(&st.outcome_rows).into());
        b.set_base_lines(Rc::clone(&st.base_lines).into());
        b.set_mine_lines(Rc::clone(&st.mine_lines).into());
        b.set_theirs_lines(Rc::clone(&st.theirs_lines).into());
        b.set_regions(Rc::clone(&st.regions).into());
        b.set_folders(Rc::clone(&st.folders).into());
        st
    }
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>) {
    let b = window.global::<Branching>();
    let a = Rc::clone(app);
    b.on_open_branches(move || a.open_branches());
    let a = Rc::clone(app);
    b.on_filter_changed(move || a.render_branches());
    let a = Rc::clone(app);
    b.on_archived_toggled(move || a.load_branches(None));
    let a = Rc::clone(app);
    b.on_select_branch(move |n| a.select_branch(n.to_string()));
    let a = Rc::clone(app);
    b.on_rev_edited(move || a.render_branches());
    let a = Rc::clone(app);
    b.on_switch_branch(move || a.switch_branch());
    let a = Rc::clone(app);
    b.on_update(move || a.update_workspace());
    let a = Rc::clone(app);
    b.on_ask_new_branch(move || a.ask_new_branch());
    let a = Rc::clone(app);
    b.on_new_edited(move || a.new_edited());
    let a = Rc::clone(app);
    b.on_create_branch(move || a.create_branch());
    let a = Rc::clone(app);
    b.on_back_to_branches(move || a.back_to_branches());
    let a = Rc::clone(app);
    b.on_ask_merge(move || a.ask_merge());
    let a = Rc::clone(app);
    b.on_merge_edited(move || a.merge_edited());
    let a = Rc::clone(app);
    b.on_merge_branch(move || a.merge_branch());
    let a = Rc::clone(app);
    b.on_ask_archive(move || a.ask_archive());
    let a = Rc::clone(app);
    b.on_show_changes(move || {
        if let Some(w) = a.window.upgrade() {
            w.set_dialog(Dialog::None);
        }
        a.open_view(View::Changes);
    });
    let a = Rc::clone(app);
    b.on_abandon_merge(move || a.abandon_merge());
    let a = Rc::clone(app);
    b.on_resolve(move |p| {
        a.open_view(View::Conflict {
            path: p.to_string(),
        })
    });
    let a = Rc::clone(app);
    b.on_take_region(move |i, t| a.take_region(i, &t));
    let a = Rc::clone(app);
    b.on_result_edited(move || a.render_regions());
    let a = Rc::clone(app);
    b.on_use_side(move |s| {
        let using = if s == "theirs" {
            Using::Theirs
        } else {
            Using::Mine
        };
        a.resolve_using(using);
    });
    let a = Rc::clone(app);
    b.on_mark_resolved(move || a.resolve_using(Using::Merged));
    let a = Rc::clone(app);
    b.on_save_result(move || a.save_result());
    let a = Rc::clone(app);
    b.on_toggle_folder(move |p| a.toggle_folder(p.to_string()));
    let a = Rc::clone(app);
    b.on_tick_folder(move |p, on| a.tick(&p, on));
    let a = Rc::clone(app);
    b.on_patterns_edited(move || a.patterns_edited());
    let a = Rc::clone(app);
    b.on_reset_view(move || a.reset_view());
    let a = Rc::clone(app);
    b.on_preview_view(move || a.preview_view());
    let a = Rc::clone(app);
    b.on_apply_view(move || a.apply_view(false));
    let a = Rc::clone(app);
    b.on_ask_force(move || a.ask_force());
    let a = Rc::clone(app);
    b.on_dismiss_error(move || {
        if let Some(w) = a.window.upgrade() {
            let b = w.global::<Branching>();
            let mut s = b.get_branches();
            s.error = ErrorInfo::default();
            b.set_branches(s);
            let mut c = b.get_conflict();
            c.error = ErrorInfo::default();
            b.set_conflict(c);
            let mut v = b.get_view();
            v.error = ErrorInfo::default();
            b.set_view(v);
        }
    });
}

fn side_lines(text: Option<&str>, base: Option<&[String]>) -> Vec<SideLine> {
    let Some(text) = text else {
        return Vec::new();
    };
    let own = merge::lines(text);
    let changed = base.and_then(|b| merge::changed_lines(b, &own, COMPARE_LIMIT));
    own.into_iter()
        .enumerate()
        .map(|(i, l)| SideLine {
            n: i32::try_from(i + 1).unwrap_or(i32::MAX),
            text: if l.is_empty() { " ".into() } else { l.into() },
            changed: changed.as_ref().is_some_and(|c| c.contains(&i)),
        })
        .collect()
}

fn side_note(side: &Value, missing: &str) -> String {
    match side["size"].as_u64() {
        Some(n) if side.is_object() => human_bytes(n),
        _ => missing.to_owned(),
    }
}

fn changed_note(lines: &[SideLine]) -> String {
    let n = lines.iter().filter(|l| l.changed).count();
    match n {
        0 => String::new(),
        1 => "1 line changed".into(),
        _ => format!("{n} lines changed"),
    }
}

impl App {
    fn branching(&self) -> Option<(AppWindow, String)> {
        let w = self.window.upgrade()?;
        let root = self.shown_root.borrow().clone()?;
        Some((w, root))
    }

    fn start_progress(w: &AppWindow) {
        let b = w.global::<Branching>();
        b.set_progress(-1.0);
        b.set_progress_text(SharedString::new());
    }

    // --- The branch list ------------------------------------------------------------------

    fn open_branches(&self) {
        let Some((w, _)) = self.branching() else {
            return;
        };
        let b = w.global::<Branching>();
        b.set_filter(SharedString::new());
        b.set_rev_text(SharedString::new());
        b.set_branches(BranchesState::default());
        self.branch.rows.set_vec(Vec::new());
        w.set_dialog(Dialog::Branches);
        self.load_branches(None);
    }

    /// Loads the branch list, selecting `select` (or keeping the selection, or the current).
    fn load_branches(&self, select: Option<String>) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let generation = self.branch.generation.get() + 1;
        self.branch.generation.set(generation);
        let all = w.global::<Branching>().get_show_archived();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.branches(root, all).await;
            if me.branch.generation.get() != generation {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut s = b.get_branches();
            match got {
                Ok(v) => {
                    let list = v["branches"].as_array().cloned().unwrap_or_default();
                    s.current = v["current"].as_str().unwrap_or("").into();
                    let want = select.unwrap_or_else(|| s.selected.to_string());
                    s.selected = if list.iter().any(|x| x["name"] == want.as_str()) {
                        want.into()
                    } else {
                        s.current.clone()
                    };
                    s.loaded = true;
                    *me.branch.list.borrow_mut() = list;
                }
                Err(e) => s.error = error_info(&e),
            }
            b.set_branches(s);
            me.render_branches();
        });
    }

    fn selected_branch(&self, s: &BranchesState) -> Option<Value> {
        self.branch
            .list
            .borrow()
            .iter()
            .find(|b| b["name"] == s.selected.as_str())
            .cloned()
    }

    fn render_branches(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let mut s = b.get_branches();
        let filter = b.get_filter();
        let rows =
            branches::branch_rows(&self.branch.list.borrow(), &s.current, &filter, &s.selected);
        s.empty = if rows.is_empty() && s.loaded && !filter.trim().is_empty() {
            format!("No branch matches “{}”.", filter.trim()).into()
        } else {
            SharedString::new()
        };
        self.branch.rows.set_vec(rows);
        let pick = self.selected_branch(&s);
        let head = pick
            .as_ref()
            .and_then(|p| p["head_rev"].as_u64())
            .unwrap_or(0);
        s.is_current = pick.is_some() && s.selected == s.current;
        s.active = pick
            .as_ref()
            .is_some_and(|p| p["state"].as_str() != Some("archived"));
        s.head = i32::try_from(head).unwrap_or(i32::MAX);
        s.bad_rev = !s.is_current && !rev_ok(parse_rev(&b.get_rev_text()), head);
        b.set_branches(s);
    }

    fn select_branch(&self, name: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let mut s = b.get_branches();
        if !s.busy.is_empty() {
            return;
        }
        s.selected = name.into();
        s.error = ErrorInfo::default();
        b.set_branches(s);
        b.set_rev_text(SharedString::new());
        self.render_branches();
    }

    /// Runs a switch or update and shows what it did.
    fn sync_with(&self, busy: String, rev: Option<u64>, branch: Option<String>) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let b = w.global::<Branching>();
        let mut s = b.get_branches();
        if !s.busy.is_empty() {
            return;
        }
        s.busy = busy.into();
        s.error = ErrorInfo::default();
        b.set_branches(s);
        Self::start_progress(&w);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = match branch {
                Some(name) => me.desk.switch(root.clone(), name, rev).await,
                None => me.desk.update(root.clone(), rev).await,
            };
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut s = b.get_branches();
            s.busy = SharedString::new();
            match got {
                Ok(v) => {
                    b.set_branches(s);
                    me.show_outcome(&branches::sync_outcome(&v), &v);
                    me.after_files_moved(root);
                }
                Err(e) => {
                    s.error = error_info(&e);
                    b.set_branches(s);
                }
            }
        });
    }

    fn switch_branch(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        self.render_branches();
        let b = w.global::<Branching>();
        let s = b.get_branches();
        if s.selected.is_empty() || s.is_current || s.bad_rev {
            return;
        }
        let rev = match parse_rev(&b.get_rev_text()) {
            RevField::Rev(r) => Some(r),
            _ => None,
        };
        let name = s.selected.to_string();
        self.sync_with(format!("Switching to {name}…"), rev, Some(name));
    }

    fn update_workspace(&self) {
        self.sync_with("Updating…".into(), None, None);
    }

    /// After a switch, update, merge or view change: the changes, the tab and the open view.
    fn after_files_moved(&self, root: String) {
        self.refresh(root);
    }

    fn show_outcome(&self, o: &branches::Outcome, result: &Value) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let (rows, more) = branches::outcome_rows(result, OUTCOME_LIMIT);
        self.branch.outcome_rows.set_vec(rows);
        w.global::<Branching>().set_outcome(OutcomeState {
            title: o.title.as_str().into(),
            text: o.text.as_str().into(),
            conflicts: o.conflicts.as_str().into(),
            notes: o.notes.as_str().into(),
            more: if more > 0 {
                format!("and {more} more").into()
            } else {
                SharedString::new()
            },
            done: o.done,
        });
        w.set_dialog(Dialog::Outcome);
    }

    // --- New branch -----------------------------------------------------------------------

    fn ask_new_branch(&self) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let b = w.global::<Branching>();
        let s = b.get_branches();
        let tab = self.workspaces_row(&root);
        let (branch, rev) = tab
            .as_ref()
            .map(|t| (t.branch.to_string(), t.rev.to_string()))
            .unwrap_or_default();
        let from = if s.selected.is_empty() {
            s.current.to_string()
        } else {
            s.selected.to_string()
        };
        let active: Vec<String> = self
            .branch
            .list
            .borrow()
            .iter()
            .filter(|x| x["state"].as_str() != Some("archived"))
            .filter_map(|x| x["name"].as_str().map(str::to_owned))
            .collect();
        let index = active.iter().position(|n| *n == from).unwrap_or(0);
        self.branch
            .bases
            .set_vec(active.iter().map(SharedString::from).collect::<Vec<_>>());
        *self.branch.base_names.borrow_mut() = active;
        b.set_base_index(i32::try_from(index).unwrap_or(0));
        b.set_base_rev(SharedString::new());
        b.set_new_name(SharedString::new());
        b.set_new_here(from == branch);
        b.set_new_switch(true);
        b.set_new_branch(NewBranchState {
            here: format!("This workspace: {branch} at r{rev}").into(),
            ..NewBranchState::default()
        });
        w.set_dialog(Dialog::NewBranch);
        self.new_edited();
    }

    /// The base branch chosen in the new-branch dialog, and its head.
    fn chosen(&self, index: i32, names: &RefCell<Vec<String>>) -> (String, u64) {
        let name = usize::try_from(index)
            .ok()
            .and_then(|i| names.borrow().get(i).cloned())
            .unwrap_or_default();
        let head = self
            .branch
            .list
            .borrow()
            .iter()
            .find(|x| x["name"] == name.as_str())
            .and_then(|x| x["head_rev"].as_u64())
            .unwrap_or(0);
        (name, head)
    }

    fn new_edited(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let (_, head) = self.chosen(b.get_base_index(), &self.branch.base_names);
        let mut n = b.get_new_branch();
        n.head = format!("head (r{head})").into();
        n.bad_rev = !b.get_new_here() && !rev_ok(parse_rev(&b.get_base_rev()), head);
        b.set_new_branch(n);
    }

    fn create_branch(&self) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        self.new_edited();
        let b = w.global::<Branching>();
        let mut n = b.get_new_branch();
        let name = b.get_new_name().trim().to_owned();
        if n.busy || n.bad_rev || name.is_empty() {
            return;
        }
        let from = if b.get_new_here() {
            None
        } else {
            let (base, _) = self.chosen(b.get_base_index(), &self.branch.base_names);
            Some(match parse_rev(&b.get_base_rev()) {
                RevField::Rev(r) => format!("{base}@r{r}"),
                _ => base,
            })
        };
        let switch = b.get_new_switch();
        n.busy = true;
        n.error = ErrorInfo::default();
        b.set_new_branch(n);
        Self::start_progress(&w);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me
                .desk
                .branch_create(root.clone(), name.clone(), from, switch)
                .await;
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut n = b.get_new_branch();
            n.busy = false;
            b.set_new_branch(n.clone());
            match got {
                Ok(v) if v["switch"].is_object() => {
                    me.show_outcome(&branches::sync_outcome(&v["switch"]), &v["switch"]);
                    me.after_files_moved(root);
                }
                Ok(_) => {
                    w.set_dialog(Dialog::Branches);
                    me.load_branches(Some(name));
                }
                Err(e) => {
                    n.error = error_info(&e);
                    b.set_new_branch(n);
                }
            }
        });
    }

    fn back_to_branches(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        w.set_dialog(Dialog::Branches);
        self.load_branches(None);
    }

    // --- Merge ----------------------------------------------------------------------------

    fn ask_merge(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let s = b.get_branches();
        let list = self.branch.list.borrow();
        let others: Vec<&Value> = list
            .iter()
            .filter(|x| x["state"].as_str() != Some("archived") && x["name"] != s.current.as_str())
            .collect();
        let names: Vec<String> = others
            .iter()
            .filter_map(|x| x["name"].as_str().map(str::to_owned))
            .collect();
        let labels: Vec<SharedString> = others
            .iter()
            .map(|x| {
                format!(
                    "{} (r{})",
                    x["name"].as_str().unwrap_or(""),
                    x["head_rev"].as_u64().unwrap_or(0)
                )
                .into()
            })
            .collect();
        drop(list);
        let index = names
            .iter()
            .position(|n| *n == s.selected.as_str())
            .unwrap_or(0);
        self.branch.sources.set_vec(labels);
        *self.branch.source_names.borrow_mut() = names;
        b.set_source_index(i32::try_from(index).unwrap_or(0));
        b.set_merge_one(false);
        b.set_merge_rev(SharedString::new());
        b.set_merge(MergeState {
            title: format!("Merge into {}", s.current).into(),
            ..MergeState::default()
        });
        w.set_dialog(Dialog::Merge);
        self.merge_edited();
    }

    fn merge_edited(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let (_, head) = self.chosen(b.get_source_index(), &self.branch.source_names);
        let mut m = b.get_merge();
        m.head = format!("r{head}").into();
        m.bad_rev = b.get_merge_one()
            && match parse_rev(&b.get_merge_rev()) {
                RevField::Rev(r) => !rev_ok(RevField::Rev(r), head),
                _ => true,
            };
        b.set_merge(m);
    }

    fn merge_branch(&self) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        self.merge_edited();
        let b = w.global::<Branching>();
        let mut m = b.get_merge();
        let (source, _) = self.chosen(b.get_source_index(), &self.branch.source_names);
        if m.busy || m.bad_rev || source.is_empty() {
            return;
        }
        let rev = match (b.get_merge_one(), parse_rev(&b.get_merge_rev())) {
            (true, RevField::Rev(r)) => Some(r),
            _ => None,
        };
        m.busy = true;
        m.error = ErrorInfo::default();
        b.set_merge(m);
        Self::start_progress(&w);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.merge(root.clone(), source, rev).await;
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut m = b.get_merge();
            m.busy = false;
            match got {
                Ok(v) => {
                    b.set_merge(m);
                    me.show_outcome(&branches::merge_outcome(&v), &v);
                    me.after_files_moved(root);
                }
                Err(e) => {
                    m.error = error_info(&e);
                    b.set_merge(m);
                }
            }
        });
    }

    fn ask_archive(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let name = w.global::<Branching>().get_branches().selected.to_string();
        if name.is_empty() {
            return;
        }
        self.ask_confirm(
            Confirm::ArchiveBranch(name.clone()),
            format!("Archive {name}?"),
            "The branch leaves the list and nobody can commit to it any more. Its history stays, and workspaces on it keep their files.".into(),
            "Archive branch".into(),
        );
    }

    pub(super) fn archive_branch(&self, name: String) {
        let Some((_, root)) = self.branching() else {
            return;
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.branch_archive(root, name).await;
            let Some(w) = me.window.upgrade() else { return };
            match got {
                Ok(_) => {
                    w.set_dialog(Dialog::Branches);
                    me.load_branches(None);
                }
                Err(e) => me.confirm_failed(&e),
            }
        });
    }

    fn abandon_merge(&self) {
        let Some((_, root)) = self.branching() else {
            return;
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            match me.desk.abandon_merge(root.clone()).await {
                Ok(_) => me.changes_done(
                    "The merge was abandoned: the next commit no longer records it. The files stay as they are.",
                ),
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

    // --- Conflicts ------------------------------------------------------------------------

    pub(super) fn load_conflict(&self, path: String) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let generation = self.branch.generation.get() + 1;
        self.branch.generation.set(generation);
        let b = w.global::<Branching>();
        b.set_conflict(ConflictState {
            path: path.as_str().into(),
            name: path.rsplit('/').next().unwrap_or(&path).into(),
            ..ConflictState::default()
        });
        b.set_result_text(SharedString::new());
        for m in [
            &self.branch.base_lines,
            &self.branch.mine_lines,
            &self.branch.theirs_lines,
        ] {
            m.set_vec(Vec::new());
        }
        self.branch.regions.set_vec(Vec::new());
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.conflict(root, path).await;
            if me.branch.generation.get() != generation {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut c = b.get_conflict();
            match got {
                Ok(v) => me.show_conflict(&w, &mut c, &v),
                Err(e) => c.error = error_info(&e),
            }
            b.set_conflict(c);
            me.render_regions();
        });
    }

    fn show_conflict(&self, w: &AppWindow, c: &mut ConflictState, v: &Value) {
        let b = w.global::<Branching>();
        c.loaded = true;
        c.from = match v["theirs_rev"].as_u64() {
            Some(r) => format!("r{r}"),
            None => "a shelf".into(),
        }
        .into();
        c.text = v["text"].as_bool() == Some(true);
        let result = v["result"]["text"].as_str();
        c.editable = c.text && result.is_some();
        c.theirs_file = v["theirs_file"].as_str().unwrap_or("").into();
        let base = v["base"]["text"].as_str().map(merge::lines);
        let base_rows = side_lines(v["base"]["text"].as_str(), None);
        let mine_rows = side_lines(v["mine"]["text"].as_str(), base.as_deref());
        let theirs_rows = side_lines(v["theirs"]["text"].as_str(), base.as_deref());
        let with_changes = |side: &Value, rows: &[SideLine], missing: &str| {
            let size = side_note(side, missing);
            match changed_note(rows) {
                ch if ch.is_empty() => size,
                ch => format!("{size} · {ch}"),
            }
        };
        if c.editable {
            c.base_note = side_note(&v["base"], "no file").into();
            c.mine_note = with_changes(&v["mine"], &mine_rows, "no file").into();
            c.theirs_note = with_changes(&v["theirs"], &theirs_rows, "no file").into();
        } else {
            c.mine_note = side_note(&v["mine"], "Deleted here").into();
            c.theirs_note = side_note(&v["theirs"], "Deleted there").into();
        }
        self.branch.base_lines.set_vec(base_rows);
        self.branch.mine_lines.set_vec(mine_rows);
        self.branch.theirs_lines.set_vec(theirs_rows);
        b.set_result_text(result.unwrap_or("").into());
    }

    fn render_regions(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let regions = merge::regions(&b.get_result_text());
        let total = regions.len();
        let rows: Vec<RegionRow> = regions
            .into_iter()
            .enumerate()
            .map(|(i, (mine, theirs))| {
                let side = |l: Vec<String>| {
                    if l.is_empty() {
                        "(nothing)".to_owned()
                    } else {
                        l.join("\n")
                    }
                };
                RegionRow {
                    index: i32::try_from(i).unwrap_or(0),
                    label: format!("Conflict {} of {total}", i + 1).into(),
                    mine: side(mine).into(),
                    theirs: side(theirs).into(),
                }
            })
            .collect();
        self.branch.regions.set_vec(rows);
        let mut c = b.get_conflict();
        c.regions_left = i32::try_from(total).unwrap_or(i32::MAX);
        b.set_conflict(c);
    }

    fn take_region(&self, index: i32, take: &str) {
        let (Some(w), Ok(i), Some(take)) = (
            self.window.upgrade(),
            usize::try_from(index),
            Take::parse(take),
        ) else {
            return;
        };
        let b = w.global::<Branching>();
        b.set_result_text(merge::take_region(&b.get_result_text(), i, take).into());
        self.render_regions();
    }

    /// Runs a resolve of the shown conflict, then goes back to Changes with `how` said.
    fn resolving(&self, how: &'static str, content: Option<String>, using: Using) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let b = w.global::<Branching>();
        let mut c = b.get_conflict();
        if c.busy || !c.loaded {
            return;
        }
        c.busy = true;
        c.error = ErrorInfo::default();
        let path = c.path.to_string();
        b.set_conflict(c);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = match content {
                Some(text) => me.desk.resolve_with(root.clone(), path.clone(), text).await,
                None => {
                    me.desk
                        .resolve(root.clone(), vec![path.clone()], using)
                        .await
                }
            };
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut c = b.get_conflict();
            c.busy = false;
            match got {
                Ok(_) => {
                    b.set_conflict(c);
                    me.open_view(View::Changes);
                    me.changes_done(&format!("Resolved {path}: {how}."));
                    me.refresh(root);
                }
                Err(e) => {
                    c.error = error_info(&e);
                    b.set_conflict(c);
                }
            }
        });
    }

    fn resolve_using(&self, using: Using) {
        let how = match using {
            Using::Mine => "kept your version",
            Using::Theirs => "took their version",
            Using::Merged => "marked it resolved",
        };
        self.resolving(how, None, using);
    }

    fn save_result(&self) {
        self.render_regions();
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        if b.get_conflict().regions_left > 0 {
            return;
        }
        let text = b.get_result_text().to_string();
        self.resolving("saved the edited result", Some(text), Using::Merged);
    }

    // --- The workspace view ---------------------------------------------------------------

    pub(super) fn load_view(&self, keep_done: bool) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        let generation = self.branch.generation.get() + 1;
        self.branch.generation.set(generation);
        let b = w.global::<Branching>();
        let old = b.get_view();
        b.set_view(ViewState {
            depot: self
                .workspaces_row(&root)
                .map(|t| t.depot)
                .unwrap_or_default(),
            done: if keep_done {
                old.done
            } else {
                SharedString::new()
            },
            ..ViewState::default()
        });
        self.branch.children.borrow_mut().clear();
        self.branch.open.borrow_mut().clear();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let summary = me.desk.view(root.clone()).await;
            let tree = match &summary {
                Ok(_) => me.desk.view_tree(root, None).await,
                Err(e) => Err(e.clone()),
            };
            if me.branch.generation.get() != generation {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut v = b.get_view();
            match (summary, tree) {
                (Ok(s), Ok(t)) => {
                    let spec = Spec::from_json(&s["view"]);
                    *me.branch.saved.borrow_mut() = spec.clone();
                    *me.branch.roots.borrow_mut() =
                        t["entries"].as_array().cloned().unwrap_or_default();
                    v.loaded = true;
                    v.server_label =
                        format!("In the view at r{}", s["rev"].as_u64().unwrap_or(0)).into();
                    let facts = |x: &Value| {
                        format!(
                            "{}, {}",
                            branches::count(x["files"].as_u64().unwrap_or(0), "file"),
                            human_bytes(x["bytes"].as_u64().unwrap_or(0))
                        )
                    };
                    v.server = facts(&s["server"]).into();
                    v.disk = facts(&s["disk"]).into();
                    b.set_view(v);
                    me.set_spec(spec, true);
                }
                (Err(e), _) | (_, Err(e)) => {
                    v.error = error_info(&e);
                    b.set_view(v);
                }
            }
        });
    }

    /// Makes `spec` the edited patterns; `texts` writes them into the pattern fields too.
    fn set_spec(&self, spec: Spec, texts: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        if texts {
            b.set_include_text(spec.include.join("\n").into());
            b.set_exclude_text(spec.exclude.join("\n").into());
        }
        let mut v = b.get_view();
        v.dirty = spec != *self.branch.saved.borrow();
        v.preview = false;
        v.stopped = false;
        b.set_view(v);
        *self.branch.spec.borrow_mut() = spec;
        self.render_folders();
    }

    fn render_folders(&self) {
        fn walk(
            out: &mut Vec<FolderRow>,
            entries: &[Value],
            parent: &str,
            depth: i32,
            spec: &Spec,
            open: &BTreeSet<String>,
            children: &HashMap<String, Vec<Value>>,
        ) {
            for e in entries.iter().filter(|e| e["kind"] == "dir") {
                let name = e["name"].as_str().unwrap_or("");
                let path = if parent.is_empty() {
                    name.to_owned()
                } else {
                    format!("{parent}/{name}")
                };
                let st = views::folder_state(spec, &path);
                let is_open = open.contains(&path);
                out.push(FolderRow {
                    path: path.as_str().into(),
                    name: name.into(),
                    depth,
                    open: is_open,
                    on: st.on,
                    mixed: st.mixed,
                    locked_by: st.locked_by.unwrap_or_default().into(),
                    files: SharedString::new(),
                });
                if let (true, Some(kids)) = (is_open, children.get(&path)) {
                    walk(out, kids, &path, depth + 1, spec, open, children);
                    let files = kids.iter().filter(|k| k["kind"] != "dir").count();
                    if files > 0 {
                        out.push(FolderRow {
                            path: format!("{path}/").into(),
                            depth: depth + 1,
                            files: branches::count(u64::try_from(files).unwrap_or(0), "file")
                                .into(),
                            ..FolderRow::default()
                        });
                    }
                }
            }
        }
        let mut rows = Vec::new();
        walk(
            &mut rows,
            &self.branch.roots.borrow(),
            "",
            0,
            &self.branch.spec.borrow(),
            &self.branch.open.borrow(),
            &self.branch.children.borrow(),
        );
        self.branch.folders.set_vec(rows);
    }

    fn toggle_folder(&self, path: String) {
        if self.branch.open.borrow_mut().remove(&path) {
            self.render_folders();
            return;
        }
        if self.branch.children.borrow().contains_key(&path) {
            self.branch.open.borrow_mut().insert(path);
            self.render_folders();
            return;
        }
        let Some((_, root)) = self.branching() else {
            return;
        };
        let generation = self.branch.generation.get();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.view_tree(root, Some(path.clone())).await;
            if me.branch.generation.get() != generation {
                return;
            }
            match got {
                Ok(t) => {
                    let kids = t["entries"].as_array().cloned().unwrap_or_default();
                    me.branch.children.borrow_mut().insert(path.clone(), kids);
                    me.branch.open.borrow_mut().insert(path);
                    me.render_folders();
                }
                Err(e) => me.view_failed(&e),
            }
        });
    }

    fn view_failed(&self, e: &ErrorView) {
        if let Some(w) = self.window.upgrade() {
            let b = w.global::<Branching>();
            let mut v = b.get_view();
            v.error = error_info(e);
            v.checking = false;
            v.applying = false;
            b.set_view(v);
        }
    }

    fn tick(&self, path: &str, on: bool) {
        let spec = views::tick_folder(&self.branch.spec.borrow(), path, on);
        self.clear_done();
        self.set_spec(spec, true);
    }

    fn clear_done(&self) {
        if let Some(w) = self.window.upgrade() {
            let b = w.global::<Branching>();
            let mut v = b.get_view();
            v.done = SharedString::new();
            b.set_view(v);
        }
    }

    fn patterns_edited(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let b = w.global::<Branching>();
        let spec = Spec {
            include: views::parse_patterns(&b.get_include_text()),
            exclude: views::parse_patterns(&b.get_exclude_text()),
        };
        self.clear_done();
        self.set_spec(spec, false);
    }

    fn reset_view(&self) {
        let saved = self.branch.saved.borrow().clone();
        self.set_spec(saved, true);
    }

    fn preview_view(&self) {
        self.patterns_edited();
        let Some((w, root)) = self.branching() else {
            return;
        };
        let b = w.global::<Branching>();
        let mut v = b.get_view();
        if v.checking || v.applying {
            return;
        }
        v.checking = true;
        v.error = ErrorInfo::default();
        b.set_view(v);
        let spec = self.branch.spec.borrow().clone();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me
                .desk
                .view_apply(root, spec.include, spec.exclude, false, true)
                .await;
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut v = b.get_view();
            v.checking = false;
            match got {
                Ok(p) => {
                    let list = |x: &Value| -> Vec<String> {
                        x.as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    let fetch = list(&p["fetch"]["paths"]);
                    let remove = list(&p["remove"]["paths"]);
                    let blocked = list(&p["blocked"]);
                    let untracked = list(&p["untracked"]);
                    v.preview = true;
                    v.preview_text = format!(
                        "Fetch {} ({}); remove {} from disk.",
                        branches::count(p["fetch"]["files"].as_u64().unwrap_or(0), "file"),
                        human_bytes(p["fetch"]["bytes"].as_u64().unwrap_or(0)),
                        branches::count(p["remove"]["files"].as_u64().unwrap_or(0), "file"),
                    )
                    .into();
                    v.preview_paths = fetch
                        .iter()
                        .map(|f| format!("+ {f}"))
                        .chain(remove.iter().map(|r| format!("− {r}")))
                        .take(200)
                        .collect::<Vec<_>>()
                        .join("\n")
                        .into();
                    v.blocked = blocked.join("\n").into();
                    v.untracked = untracked.join("\n").into();
                    v.stopped = !blocked.is_empty() || !untracked.is_empty();
                }
                Err(e) => v.error = error_info(&e),
            }
            b.set_view(v);
        });
    }

    fn ask_force(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let v = w.global::<Branching>().get_view();
        let blocked = v.blocked.lines().count();
        let mut body = String::new();
        if blocked > 0 {
            body.push_str(&format!(
                "The changes to {} are lost when they leave the view. ",
                branches::count(u64::try_from(blocked).unwrap_or(0), "file")
            ));
        }
        if !v.untracked.is_empty() {
            body.push_str("Untracked files in the way are overwritten. ");
        }
        body.push_str("This cannot be undone.");
        self.ask_confirm(
            Confirm::ForceView,
            "Drop local changes?".into(),
            body,
            "Drop and apply".into(),
        );
    }

    pub(super) fn apply_view(&self, force: bool) {
        let Some((w, root)) = self.branching() else {
            return;
        };
        if force {
            w.set_dialog(Dialog::None);
        }
        let b = w.global::<Branching>();
        let mut v = b.get_view();
        if v.checking || v.applying {
            return;
        }
        v.applying = true;
        v.error = ErrorInfo::default();
        b.set_view(v);
        Self::start_progress(&w);
        let spec = self.branch.spec.borrow().clone();
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me
                .desk
                .view_apply(root.clone(), spec.include, spec.exclude, force, false)
                .await;
            let Some(w) = me.window.upgrade() else { return };
            let b = w.global::<Branching>();
            let mut v = b.get_view();
            v.applying = false;
            match got {
                Ok(r) => {
                    v.done = format!(
                        "View updated: {} fetched, {} removed.",
                        branches::count(r["added"].as_u64().unwrap_or(0), "file"),
                        r["removed"].as_u64().unwrap_or(0)
                    )
                    .into();
                    v.preview = false;
                    b.set_view(v);
                    me.load_view(true);
                    me.after_files_moved(root);
                }
                Err(e) => {
                    v.error = error_info(&e);
                    b.set_view(v);
                }
            }
        });
    }
}
