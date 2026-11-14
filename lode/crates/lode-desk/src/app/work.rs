//! The changes view of an open workspace (T131): status, picking, commit, revert, add and the
//! changelist dialogs.

use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;

use serde_json::Value;
use slint::{Model, SharedString};

use super::{error_info, spawn, usage, App, AppWindow, ChangesState, Dialog, ErrorInfo};
use super::{ListDialogState, MoveTarget};
use crate::changes::{self, Pending, NEW_FILES};
use crate::desk::ErrorView;

/// What the app keeps for one open workspace.
#[derive(Debug, Default)]
pub(super) struct WorkspaceState {
    pending: Pending,
    loaded: bool,
    picked: BTreeSet<String>,
    selected: Option<String>,
    /// The commit message typed while another tab was showing.
    message: String,
    /// Bumped by each refresh; only the newest answer is shown.
    generation: u64,
    /// The view the workspace is on.
    pub(super) view: super::read::View,
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>) {
    let a = Rc::clone(app);
    window.on_tab_shown(move |_| a.tab_shown());
    let a = Rc::clone(app);
    window.on_files_changed(move |root| a.files_changed(&root));
    let a = Rc::clone(app);
    window.on_toggle_file(move |p| a.toggle_file(&p));
    let a = Rc::clone(app);
    window.on_toggle_group(move |k| a.toggle_group(&k));
    let a = Rc::clone(app);
    window.on_toggle_all(move || a.toggle_all());
    let a = Rc::clone(app);
    window.on_select_file(move |p| a.select_file(&p));
    let a = Rc::clone(app);
    window.on_refresh_changes(move || a.refresh_shown());
    let a = Rc::clone(app);
    window.on_commit(move || a.commit());
    let a = Rc::clone(app);
    window.on_show_dialog(move |d, name| a.show_list_dialog(d, &name));
    let a = Rc::clone(app);
    window.on_submit_dialog(move || a.submit_list_dialog());
    let a = Rc::clone(app);
    window.on_move_to(move |name| a.move_to(name.to_string()));
    let a = Rc::clone(app);
    window.on_use_message(move |m| {
        if let Some(w) = a.window.upgrade() {
            w.set_commit_message(m);
            w.set_dialog(Dialog::None);
        }
    });
    let a = Rc::clone(app);
    window.on_add_selected(move || a.add_selected());
}

/// Reads the remembered commit messages; none when the file is missing or damaged.
pub(super) fn load_messages(file: &Path) -> Vec<String> {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The paths a refusal names (`details.paths`), one per line.
fn refused(e: &ErrorView) -> SharedString {
    e.details["paths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
        .into()
}

impl App {
    /// The root of the workspace whose tab is showing.
    fn current_root(&self, w: &AppWindow) -> Option<String> {
        let tab = usize::try_from(w.get_current_tab()).ok()?.checked_sub(1)?;
        self.workspaces.row_data(tab).map(|t| t.root.to_string())
    }

    fn tab_shown(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let root = self.current_root(&w);
        if *self.shown_root.borrow() != root {
            self.show_root(&w, root);
        }
    }

    /// Shows `root`'s changes (keeping the message typed for the one shown before) and loads
    /// them again.
    pub(super) fn show_root(&self, w: &AppWindow, root: Option<String>) {
        let before = self.shown_root.replace(root.clone());
        if let Some(b) = before.filter(|b| Some(b) != root.as_ref()) {
            if let Some(st) = self.work.borrow_mut().get_mut(&b) {
                st.message = w.get_commit_message().to_string();
            }
            let message = root
                .as_ref()
                .and_then(|r| self.work.borrow().get(r).map(|s| s.message.clone()))
                .unwrap_or_default();
            w.set_commit_message(message.into());
            let mut changes = w.get_changes();
            changes.done = SharedString::new();
            changes.error = ErrorInfo::default();
            changes.refused = SharedString::new();
            w.set_changes(changes);
        }
        self.render(w);
        if let Some(r) = root {
            self.work.borrow_mut().entry(r.clone()).or_default();
            self.restore_view(&r);
            self.refresh(r);
        }
    }

    fn files_changed(&self, root: &str) {
        if self.shown_root.borrow().as_deref() == Some(root) {
            self.refresh(root.to_owned());
        }
    }

    fn refresh_shown(&self) {
        if let Some(r) = self.shown_root.borrow().clone() {
            self.refresh(r);
        }
    }

    /// Loads `root`'s status and changelists again.
    pub(super) fn refresh(&self, root: String) {
        let generation = {
            let mut work = self.work.borrow_mut();
            let st = work.entry(root.clone()).or_default();
            st.generation += 1;
            st.generation
        };
        let Some(me) = self.me() else { return };
        spawn(async move {
            let status = me.desk.status(root.clone()).await;
            let lists = match &status {
                Ok(_) => me.desk.changelists(root.clone()).await,
                Err(e) => Err(e.clone()),
            };
            let Some(w) = me.window.upgrade() else { return };
            {
                let mut work = me.work.borrow_mut();
                let Some(st) = work.get_mut(&root) else {
                    return;
                };
                if st.generation != generation {
                    return;
                }
                match (status, lists) {
                    (Ok(s), Ok(l)) => {
                        st.pending = Pending::from_json(&s, &l);
                        st.loaded = true;
                        let present: BTreeSet<String> = st.pending.paths().into_iter().collect();
                        st.picked.retain(|p| present.contains(p));
                        if st.selected.as_ref().is_some_and(|s| !present.contains(s)) {
                            st.selected = None;
                        }
                        me.update_tab(&root, &st.pending);
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        if me.shown_root.borrow().as_deref() == Some(root.as_str()) {
                            let mut changes = w.get_changes();
                            changes.error = error_info(&e);
                            w.set_changes(changes);
                        }
                    }
                }
            }
            if me.shown_root.borrow().as_deref() == Some(root.as_str()) {
                me.render(&w);
            }
        });
    }

    /// This controller again, for a task that outlives the call.
    pub(super) fn me(&self) -> Option<Rc<Self>> {
        self.this.borrow().upgrade()
    }

    /// The tab's branch and revision as the newest status says.
    fn update_tab(&self, root: &str, p: &Pending) {
        let Some(i) = self.workspaces.iter().position(|t| t.root == root) else {
            return;
        };
        if let Some(mut tab) = self.workspaces.row_data(i) {
            let rev: SharedString = p.rev.map(|r| r.to_string()).unwrap_or_default().into();
            if tab.rev != rev || tab.branch != p.branch.as_str() {
                tab.rev = rev;
                tab.branch = p.branch.as_str().into();
                self.workspaces.set_row_data(i, tab);
            }
        }
    }

    /// Fills the rows and the view's state from what is kept for the shown workspace.
    fn render(&self, w: &AppWindow) {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let st = root.as_ref().and_then(|r| work.get(r));
        let mut changes = w.get_changes();
        match st {
            Some(st) => {
                let rows = changes::rows(&st.pending, &st.picked, st.selected.as_deref());
                self.change_rows.set_vec(rows);
                let total = st.pending.paths().len();
                changes.loaded = st.loaded;
                changes.total = i32::try_from(total).unwrap_or(i32::MAX);
                changes.picked = i32::try_from(st.picked.len()).unwrap_or(i32::MAX);
                changes.conflicted = i32::try_from(st.pending.conflicted()).unwrap_or(i32::MAX);
                changes.merging = st.pending.merging().into();
                changes.details = st
                    .selected
                    .as_deref()
                    .and_then(|p| st.pending.file(p))
                    .map(changes::details)
                    .unwrap_or_default();
                let suggestion = changes::merge_message(&st.pending.merges);
                if !suggestion.is_empty() && w.get_commit_message().trim().is_empty() {
                    w.set_commit_message(suggestion.into());
                }
            }
            None => {
                self.change_rows.set_vec(Vec::new());
                changes = ChangesState::default();
            }
        }
        w.set_changes(changes);
    }

    /// Runs `f` on the shown workspace's state, then shows the result.
    fn with_shown(&self, f: impl FnOnce(&mut WorkspaceState)) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(root) = self.shown_root.borrow().clone() else {
            return;
        };
        if let Some(st) = self.work.borrow_mut().get_mut(&root) {
            f(st);
        }
        self.render(&w);
    }

    fn toggle_file(&self, path: &str) {
        self.with_shown(|st| {
            if !st.picked.remove(path) {
                st.picked.insert(path.to_owned());
            }
        });
    }

    fn toggle_group(&self, key: &str) {
        self.with_shown(|st| {
            let paths: Vec<String> = st
                .pending
                .groups()
                .into_iter()
                .find(|g| g.key == key)
                .map(|g| g.files.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default();
            let all = paths.iter().all(|p| st.picked.contains(p));
            for p in paths {
                if all {
                    st.picked.remove(&p);
                } else {
                    st.picked.insert(p);
                }
            }
        });
    }

    fn toggle_all(&self) {
        self.with_shown(|st| {
            let paths = st.pending.paths();
            if paths.iter().all(|p| st.picked.contains(p)) {
                st.picked.clear();
            } else {
                st.picked.extend(paths);
            }
        });
    }

    fn select_file(&self, path: &str) {
        self.with_shown(|st| st.selected = Some(path.to_owned()));
    }

    /// The selected file, or every picked file when the selected one is picked; files not
    /// added yet are left out unless `untracked`.
    fn targets(&self, untracked: bool) -> Vec<String> {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
            return Vec::new();
        };
        let Some(sel) = st.selected.clone() else {
            return Vec::new();
        };
        let paths: Vec<String> = if st.picked.contains(&sel) {
            st.pending
                .paths()
                .into_iter()
                .filter(|p| st.picked.contains(p))
                .collect()
        } else {
            vec![sel]
        };
        paths
            .into_iter()
            .filter(|p| {
                let new = st.pending.file(p).is_some_and(|f| f.status == "untracked");
                new == untracked
            })
            .collect()
    }

    /// Runs `op` in the shown workspace with the view busy, then loads it again.
    fn run(
        &self,
        busy: &str,
        op: impl FnOnce(
                Rc<Self>,
                String,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<String, ErrorView>>>,
            > + 'static,
    ) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(root) = self.shown_root.borrow().clone() else {
            return;
        };
        let Some(me) = self.me() else { return };
        let mut changes = w.get_changes();
        if !changes.busy.is_empty() {
            return;
        }
        changes.busy = busy.into();
        changes.done = SharedString::new();
        changes.error = ErrorInfo::default();
        changes.refused = SharedString::new();
        changes.progress = -1.0;
        changes.progress_text = SharedString::new();
        w.set_changes(changes);
        spawn(async move {
            let result = op(Rc::clone(&me), root.clone()).await;
            let Some(w) = me.window.upgrade() else { return };
            let mut changes = w.get_changes();
            changes.busy = SharedString::new();
            match result {
                Ok(done) => changes.done = done.into(),
                Err(e) => {
                    changes.refused = refused(&e);
                    changes.error = error_info(&e);
                }
            }
            w.set_changes(changes);
            me.refresh(root);
        });
    }

    fn commit(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let message = w.get_commit_message().trim().to_owned();
        let (merging, picked, untracked) = {
            let root = self.shown_root.borrow().clone();
            let work = self.work.borrow();
            let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
                return;
            };
            let picked: Vec<String> = st
                .pending
                .paths()
                .into_iter()
                .filter(|p| st.picked.contains(p))
                .collect();
            let untracked: Vec<String> = picked
                .iter()
                .filter(|p| st.pending.file(p).is_some_and(|f| f.status == "untracked"))
                .cloned()
                .collect();
            (!st.pending.merges.is_empty(), picked, untracked)
        };
        if message.is_empty() || (!merging && picked.is_empty()) {
            return;
        }
        self.run("Committing", move |me, root| {
            Box::pin(async move {
                let r = if merging {
                    // A merge is committed whole.
                    me.desk.commit_all(root.clone(), message.clone()).await?
                } else {
                    if !untracked.is_empty() {
                        me.desk.add(root.clone(), untracked).await?;
                    }
                    me.desk
                        .commit(root.clone(), picked, message.clone())
                        .await?
                };
                me.committed(&root, &message);
                Ok(format!(
                    "Committed r{} ({}).",
                    r["rev"],
                    plural(
                        usize::try_from(r["files"].as_u64().unwrap_or(0)).unwrap_or(0),
                        "file"
                    )
                ))
            })
        });
    }

    /// After a commit: the message is remembered and cleared, and nothing is picked.
    fn committed(&self, root: &str, message: &str) {
        {
            let mut m = self.messages.borrow_mut();
            changes::remember_message(&mut m, message);
            if let Some(dir) = self.messages_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(text) = serde_json::to_vec_pretty(&*m) {
                // Losing the list of messages is no reason to fail a commit.
                let _ = std::fs::write(&self.messages_file, text);
            }
        }
        if let Some(st) = self.work.borrow_mut().get_mut(root) {
            st.picked.clear();
        }
        if self.shown_root.borrow().as_deref() == Some(root) {
            if let Some(w) = self.window.upgrade() {
                if w.get_commit_message().trim() == message {
                    w.set_commit_message(SharedString::new());
                }
            }
        }
    }

    fn add_selected(&self) {
        let paths = self.targets(true);
        if paths.is_empty() {
            return;
        }
        self.run("Adding", move |me, root| {
            Box::pin(async move {
                let n = paths.len();
                me.desk.add(root, paths).await?;
                Ok(format!("Added {}.", plural(n, "file")))
            })
        });
    }

    fn show_list_dialog(&self, dialog: Dialog, name: &str) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        if w.get_dialog() != Dialog::None || !w.get_changes().busy.is_empty() {
            return;
        }
        let root = self.shown_root.borrow().clone();
        let mut state = ListDialogState {
            name: name.into(),
            ..ListDialogState::default()
        };
        {
            let work = self.work.borrow();
            let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
                return;
            };
            let group = st.pending.groups().into_iter().find(|g| g.key == name);
            let files = group.as_ref().map_or(0, |g| g.files.len());
            state.files = i32::try_from(files).unwrap_or(i32::MAX);
            let description = group.map(|g| g.description).unwrap_or_default();
            match dialog {
                Dialog::NewChangelist => {
                    let picked = st
                        .picked
                        .iter()
                        .filter(|p| st.pending.file(p).is_some_and(|f| f.status != "untracked"))
                        .count();
                    state.picked = i32::try_from(picked).unwrap_or(i32::MAX);
                    state.move_picked = true;
                }
                Dialog::RenameChangelist => state.value = name.into(),
                Dialog::CommitChangelist => state.value = description.into(),
                Dialog::Messages => {
                    let m: Vec<SharedString> = self
                        .messages
                        .borrow()
                        .iter()
                        .map(|m| m.as_str().into())
                        .collect();
                    self.messages_model.set_vec(m);
                }
                _ => {}
            }
        }
        if matches!(dialog, Dialog::Revert | Dialog::MoveTo) {
            let paths = self.targets(false);
            if paths.is_empty() {
                return;
            }
            state.what = match paths.as_slice() {
                [one] => one.as_str().into(),
                many => plural(many.len(), "file").into(),
            };
            if dialog == Dialog::MoveTo {
                state.what = format!("Moves {}.", state.what).into();
                self.move_targets.set_vec(self.move_targets_for(&paths));
            }
            *self.dialog_paths.borrow_mut() = paths;
        }
        w.set_list_dialog(state);
        w.set_dialog(dialog);
    }

    /// The changelists `paths` can move to: every one some of them are not in yet.
    fn move_targets_for(&self, paths: &[String]) -> Vec<MoveTarget> {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
            return Vec::new();
        };
        let list_of = |p: &String| {
            st.pending
                .file(p)
                .and_then(|f| f.changelist.clone())
                .unwrap_or_else(|| "default".to_owned())
        };
        let mut names = vec!["default".to_owned()];
        names.extend(st.pending.named_lists().map(|l| l.name.clone()));
        names
            .into_iter()
            .filter(|n| paths.iter().any(|p| &list_of(p) != n))
            .map(|n| MoveTarget {
                label: if n == "default" {
                    "Changes (no changelist)".into()
                } else {
                    n.as_str().into()
                },
                name: n.into(),
            })
            .collect()
    }

    fn move_to(&self, name: String) {
        let paths = self.dialog_paths.borrow().clone();
        self.dialog_op(move |me, root| {
            Box::pin(async move {
                let n = paths.len();
                me.desk.changelist_move(root, name.clone(), paths).await?;
                let to = if name == "default" {
                    "Changes".to_owned()
                } else {
                    name
                };
                Ok(format!("Moved {} to {to}.", plural(n, "file")))
            })
        });
    }

    fn submit_list_dialog(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let state = w.get_list_dialog();
        if state.busy {
            return;
        }
        let name = state.name.to_string();
        let value = state.value.trim().to_owned();
        match w.get_dialog() {
            Dialog::NewChangelist => {
                if !changes::valid_list_name(&value) {
                    return self.dialog_error(&usage(NAME_RULE));
                }
                let description = state.description.trim().to_owned();
                let paths: Vec<String> = if state.move_picked {
                    self.picked_tracked()
                } else {
                    Vec::new()
                };
                self.dialog_op(move |me, root| {
                    Box::pin(async move {
                        me.desk
                            .changelist_new(root.clone(), value.clone(), description)
                            .await?;
                        if !paths.is_empty() {
                            me.desk.changelist_move(root, value.clone(), paths).await?;
                        }
                        Ok(format!("Made changelist {value}."))
                    })
                });
            }
            Dialog::RenameChangelist => {
                if !changes::valid_list_name(&value) {
                    return self.dialog_error(&usage(NAME_RULE));
                }
                let (description, files) = self.list_contents(&name);
                self.dialog_op(move |me, root| {
                    Box::pin(async move {
                        // A new list with the same description takes the files, then the old
                        // one goes.
                        me.desk
                            .changelist_new(root.clone(), value.clone(), description)
                            .await?;
                        if !files.is_empty() {
                            me.desk
                                .changelist_move(root.clone(), value.clone(), files)
                                .await?;
                        }
                        me.desk.changelist_delete(root, name.clone()).await?;
                        Ok(format!("Renamed changelist {name} to {value}."))
                    })
                });
            }
            Dialog::CommitChangelist => {
                if value.is_empty() {
                    return;
                }
                self.dialog_op(move |me, root| {
                    Box::pin(async move {
                        let r = me
                            .desk
                            .commit_changelist(root.clone(), name.clone(), Some(value.clone()))
                            .await?;
                        me.committed(&root, &value);
                        Ok(format!(
                            "Committed r{} ({}) from {name}.",
                            r["rev"],
                            plural(
                                usize::try_from(r["files"].as_u64().unwrap_or(0)).unwrap_or(0),
                                "file"
                            )
                        ))
                    })
                });
            }
            Dialog::DeleteChangelist => {
                self.dialog_op(move |me, root| {
                    Box::pin(async move {
                        me.desk.changelist_delete(root, name.clone()).await?;
                        Ok(format!("Deleted changelist {name}."))
                    })
                });
            }
            Dialog::Revert => {
                let paths = self.dialog_paths.borrow().clone();
                self.dialog_op(move |me, root| {
                    Box::pin(async move {
                        let n = paths.len();
                        me.desk.revert(root, paths).await?;
                        Ok(format!("Reverted {}.", plural(n, "file")))
                    })
                });
            }
            _ => {}
        }
    }

    /// Picked files that are tracked, in list order.
    /// The picked files not added yet.
    pub(super) fn picked_untracked(&self) -> Vec<String> {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
            return Vec::new();
        };
        st.pending
            .paths()
            .into_iter()
            .filter(|p| st.picked.contains(p))
            .filter(|p| st.pending.file(p).is_some_and(|f| f.status == "untracked"))
            .collect()
    }

    /// The file selected in the changes view.
    pub(super) fn selected_file(&self) -> Option<String> {
        let root = self.shown_root.borrow().clone()?;
        self.work.borrow().get(&root)?.selected.clone()
    }

    /// Shows a line under the commit box, as an operation's result.
    pub(super) fn changes_done(&self, text: &str) {
        if let Some(w) = self.window.upgrade() {
            let mut c = w.get_changes();
            c.done = text.into();
            w.set_changes(c);
        }
    }

    pub(super) fn picked_tracked(&self) -> Vec<String> {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
            return Vec::new();
        };
        st.pending
            .paths()
            .into_iter()
            .filter(|p| st.picked.contains(p))
            .filter(|p| st.pending.file(p).is_some_and(|f| f.status != "untracked"))
            .collect()
    }

    /// A changelist's description and files.
    pub(super) fn list_contents(&self, name: &str) -> (String, Vec<String>) {
        let root = self.shown_root.borrow().clone();
        let work = self.work.borrow();
        let Some(st) = root.as_ref().and_then(|r| work.get(r)) else {
            return (String::new(), Vec::new());
        };
        st.pending
            .groups()
            .into_iter()
            .find(|g| g.key == name && g.key != NEW_FILES)
            .map(|g| {
                (
                    g.description,
                    g.files.iter().map(|f| f.path.clone()).collect(),
                )
            })
            .unwrap_or_default()
    }

    fn dialog_error(&self, e: &ErrorView) {
        if let Some(w) = self.window.upgrade() {
            let mut state = w.get_list_dialog();
            state.error = error_info(e);
            state.refused = refused(e);
            w.set_list_dialog(state);
        }
    }

    /// Runs a dialog's operation with the dialog busy; closes it and says what happened when
    /// it worked, shows the error in it when not.
    fn dialog_op(
        &self,
        op: impl FnOnce(
                Rc<Self>,
                String,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<String, ErrorView>>>,
            > + 'static,
    ) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(root) = self.shown_root.borrow().clone() else {
            return;
        };
        let Some(me) = self.me() else { return };
        let mut state = w.get_list_dialog();
        state.busy = true;
        state.error = ErrorInfo::default();
        state.refused = SharedString::new();
        w.set_list_dialog(state);
        let dialog = w.get_dialog();
        spawn(async move {
            let result = op(Rc::clone(&me), root.clone()).await;
            let Some(w) = me.window.upgrade() else { return };
            let mut state = w.get_list_dialog();
            state.busy = false;
            match result {
                Ok(done) => {
                    w.set_list_dialog(state);
                    if w.get_dialog() == dialog {
                        w.set_dialog(Dialog::None);
                    }
                    let mut changes = w.get_changes();
                    changes.done = done.into();
                    changes.error = ErrorInfo::default();
                    changes.refused = SharedString::new();
                    w.set_changes(changes);
                }
                Err(e) => {
                    state.error = error_info(&e);
                    state.refused = refused(&e);
                    w.set_list_dialog(state);
                }
            }
            me.refresh(root);
        });
    }
}

const NAME_RULE: &str =
    "a changelist name is 1 to 64 letters, digits, dots, dashes or underscores, and not \"default\"";
