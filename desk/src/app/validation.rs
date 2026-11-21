//! Drive validation in the originating workspace, retaining the selection across refusals.
use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::desk::ShelveRequest;
use crate::validation::{self as model, Action, Control, Event};

#[derive(Clone)]
struct Request {
    root: String,
    action: Action,
    options: ShelveRequest,
    add: Vec<String>,
    origin: Dialog,
}

#[derive(Default)]
pub(super) struct State {
    request: RefCell<Option<Request>>,
    control: RefCell<Option<Arc<Control>>>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(control) = self.control.get_mut().take() {
            control.answer(false);
            control.cancel();
        }
    }
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>, open_browser: OpenBrowser) {
    let v = window.global::<Validation>();
    let a = app.clone();
    v.on_check(move || {
        if !a.picked_untracked().is_empty() {
            a.changes_done("Add the picked new files before running Check changes.");
            return;
        }
        let mut paths = a.picked_tracked();
        if paths.is_empty() {
            if let Some(p) = a.selected_file() {
                let root = a.shown_root.borrow();
                if root.as_ref().and_then(|r| {
                    a.work
                        .borrow()
                        .get(r)
                        .and_then(|st| st.pending.file(&p).map(|f| f.status == "untracked"))
                }) == Some(true)
                {
                    a.changes_done("Add this file before running Check changes.");
                    return;
                }
                paths.push(p);
            }
        }
        if !paths.is_empty() {
            let msg = a
                .window
                .upgrade()
                .map(|w| w.get_commit_message().trim().to_owned())
                .filter(|s| !s.is_empty());
            a.start_validation(
                Action::Check,
                ShelveRequest {
                    paths,
                    message: msg,
                    ..Default::default()
                },
                Vec::new(),
            );
        }
    });
    let a = app.clone();
    v.on_cancel(move || {
        if let Some(c) = a.validation.control.borrow().as_ref() {
            c.cancel();
        }
    });
    let a = app.clone();
    v.on_consent(move |allow| {
        if let Some(c) = a.validation.control.borrow().as_ref() {
            c.answer(allow);
        }
    });
    let a = app.clone();
    v.on_show_script(move |p| {
        let Some(w) = a.window.upgrade() else { return };
        let Some(req) = a.validation.request.borrow().clone() else {
            return;
        };
        let v = w.global::<Validation>();
        if v.get_stage() != "trust" || !v.get_scripts().iter().any(|s| s == p) {
            return;
        }
        let preview = Desk::script_preview(Path::new(&req.root), p.as_str())
            .unwrap_or_else(|e| format!("Cannot read script: {e}"));
        v.set_preview(preview.into());
    });
    let a = app.clone();
    v.on_close(move || a.close_validation());
    let a = app.clone();
    v.on_retry(move || a.repeat_validation(Action::Check, false));
    let a = app.clone();
    v.on_bypass(move |commit| {
        let Some(w) = a.window.upgrade() else { return };
        let v = w.global::<Validation>();
        if commit && v.get_allow_commit() {
            a.repeat_validation(Action::Commit, true)
        }
        if !commit && v.get_allow_shelve() {
            a.repeat_validation(Action::Shelve, !v.get_shelf_required())
        }
    });
    let a = app.clone();
    v.on_problem(move |path, line| {
        let Some(w) = a.window.upgrade() else { return };
        let Some(req) = a.validation.request.borrow().clone() else {
            return;
        };
        if a.shown_root.borrow().as_ref() != Some(&req.root) {
            return;
        }
        // Script output is untrusted. Only select a path already present in Changes.
        if !a
            .work
            .borrow()
            .get(&req.root)
            .is_some_and(|st| st.pending.file(&path).is_some())
        {
            return;
        }
        a.validation.request.borrow_mut().take();
        w.set_dialog(Dialog::None);
        w.global::<Reading>().invoke_show_pane(Pane::Changes);
        a.select_problem(&path, line);
    });
    v.on_help(move || {
        open_browser("https://github.com/hutler/Lode/blob/main/docs/agent-validation.md#trust-the-scripts-and-select-python");
    });
}

impl App {
    pub(super) fn start_validation(
        &self,
        action: Action,
        options: ShelveRequest,
        add: Vec<String>,
    ) {
        if self.validation.control.borrow().is_some() {
            return;
        }
        let (Some(w), Some(root)) = (self.window.upgrade(), self.shown_root.borrow().clone())
        else {
            return;
        };
        self.launch_validation(
            Request {
                root,
                action,
                options,
                add,
                origin: w.get_dialog(),
            },
            false,
        );
    }

    fn repeat_validation(&self, action: Action, bypass: bool) {
        let Some(mut req) = self.validation.request.borrow().clone() else {
            return;
        };
        if self.validation.control.borrow().is_some()
            || self.shown_root.borrow().as_ref() != Some(&req.root)
        {
            return;
        }
        if action == Action::Shelve && req.action != Action::Shelve {
            req.options.keep = true;
            if req.options.message.is_none() {
                req.options.message = Some("Changes for review".into());
            }
        }
        req.action = action;
        self.launch_validation(req, bypass);
    }

    pub(super) fn close_validation(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let v = w.global::<Validation>();
        if let Some(c) = self.validation.control.borrow().as_ref() {
            if v.get_stage() == "trust" {
                c.answer(false);
            } else if v.get_stage() == "checking" {
                c.cancel();
            }
            return;
        }
        let origin = self
            .validation
            .request
            .borrow_mut()
            .take()
            .map_or(Dialog::None, |r| r.origin);
        w.set_dialog(origin);
    }

    fn launch_validation(&self, req: Request, bypass: bool) {
        let (Some(w), Some(me)) = (self.window.upgrade(), self.me()) else {
            return;
        };
        let v = w.global::<Validation>();
        let generation = v.get_generation().wrapping_add(1);
        v.set_generation(generation);
        v.set_rows(ModelRc::default());
        v.set_preview("".into());
        v.set_detail("".into());
        v.set_stage("working".into());
        v.set_heading(
            match req.action {
                Action::Check => "Checking changes…",
                Action::Commit => "Committing…",
                Action::Shelve => "Shelving…",
            }
            .into(),
        );
        let branch = self
            .work
            .borrow()
            .get(&req.root)
            .map(|st| st.pending.branch.clone())
            .unwrap_or_default();
        let (commit_allowed, scripts) = Desk::validation_policy(Path::new(&req.root), &branch);
        v.set_scripts(ModelRc::from(
            scripts
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>()
                .as_slice(),
        ));
        w.set_dialog(Dialog::Validation);
        let control = Arc::new(Control::default());
        *self.validation.control.borrow_mut() = Some(control.clone());
        *self.validation.request.borrow_mut() = Some(req.clone());
        let weak = w.as_weak();
        spawn(async move {
            let result = async {
                if !req.add.is_empty() {
                    me.desk.add(req.root.clone(), req.add.clone()).await?;
                }
                me.desk
                    .check_or_send(
                        req.root.clone(),
                        req.action,
                        req.options.clone(),
                        bypass,
                        control,
                        move |event| {
                            let _ = weak.upgrade_in_event_loop(move |w| {
                                let v = w.global::<Validation>();
                                if let Event::Python(note) = &event {
                                    python_banner(&w, note);
                                    return;
                                }
                                if v.get_generation() != generation
                                    || w.get_dialog() != Dialog::Validation
                                {
                                    return;
                                }
                                match event {
                                    Event::Checking(note) => {
                                        v.set_detail("".into());
                                        v.set_stage("checking".into());
                                        v.set_heading(note.into());
                                    }
                                    Event::Sending => {
                                        v.set_detail("".into());
                                        v.set_stage("working".into());
                                        v.set_heading("Sending changes…".into());
                                    }
                                    Event::Progress(text) => {
                                        if v.get_stage() == "working" {
                                            v.set_detail(text.into());
                                        }
                                    }
                                    Event::Trust(question) => {
                                        v.set_stage("trust".into());
                                        v.set_heading("Run depot scripts on this computer?".into());
                                        v.set_detail(question.into());
                                    }
                                    Event::Python(_) => {}
                                }
                            });
                        },
                    )
                    .await
            }
            .await;
            let Some(w) = me.window.upgrade() else { return };
            let v = w.global::<Validation>();
            if v.get_generation() != generation {
                return;
            }
            v.set_generation(generation.wrapping_add(1));
            me.validation.control.borrow_mut().take();
            if let Some(r) = me.validation.request.borrow_mut().as_mut() {
                r.add.clear();
            }
            if me.shown_root.borrow().as_ref() != Some(&req.root) {
                me.validation.request.borrow_mut().take();
                w.set_dialog(Dialog::None);
                return;
            }
            match result {
                Ok(value) if req.action == Action::Check => {
                    show_results(&w, "Validation results", &value["results"]);
                    v.set_detail(
                        if value["results"].as_array().is_some_and(Vec::is_empty) {
                            "No configured checks for this workspace."
                        } else {
                            ""
                        }
                        .into(),
                    );
                }
                Ok(value) => {
                    w.set_dialog(Dialog::None);
                    me.validation.request.borrow_mut().take();
                    let files = value["files"].as_u64().unwrap_or(0);
                    let count = format!("{files} file{}", if files == 1 { "" } else { "s" });
                    let done = if req.action == Action::Commit {
                        me.committed(&req.root, req.options.message.as_deref().unwrap_or(""));
                        let list = req
                            .options
                            .changelist
                            .as_ref()
                            .map(|n| format!(" from {n}"))
                            .unwrap_or_default();
                        format!("Committed r{} ({count}){list}.", value["rev"])
                    } else {
                        let place = if value["updated"] == true {
                            format!(
                                "Updated shelf {} to version {}",
                                value["shelf"], value["version"]
                            )
                        } else {
                            format!("Shelved as shelf {}", value["shelf"])
                        };
                        let kept = if value["kept"] == true {
                            ""
                        } else {
                            "; the files were reverted here"
                        };
                        format!("{place} ({count}){kept}.")
                    };
                    let mut c = w.get_changes();
                    c.error = ErrorInfo::default();
                    c.refused = "".into();
                    w.set_changes(c);
                    me.changes_done(&done);
                    me.refresh(req.root);
                }
                Err(e) if e.code == "validation_failed" || e.code == "shelf_required" => {
                    let gated = e.code == "shelf_required";
                    show_results(
                        &w,
                        if gated {
                            "This branch requires a shelf"
                        } else {
                            "Validation failed"
                        },
                        &e.details["results"],
                    );
                    v.set_detail(
                        if gated {
                            e.message.as_str()
                        } else {
                            "Review the problems below, then edit the files and check again."
                        }
                        .into(),
                    );
                    v.set_retryable(!gated);
                    v.set_shelf_required(gated);
                    v.set_allow_shelve(true);
                    v.set_allow_commit(
                        !gated
                            && commit_allowed
                            && req
                                .options
                                .message
                                .as_ref()
                                .is_some_and(|m| !m.trim().is_empty()),
                    );
                }
                Err(e) => {
                    if e.code == "python_missing" {
                        python_banner(&w, &e.message);
                    }
                    w.set_dialog(req.origin);
                    me.validation.request.borrow_mut().take();
                    if e.code == "cancelled" || e.code == "validation_untrusted" {
                        me.changes_done("Validation stopped. Nothing was sent.");
                    } else if req.origin == Dialog::Shelve {
                        let team = w.global::<Team>();
                        let mut d = team.get_shelve();
                        d.error = error_info(&e);
                        team.set_shelve(d);
                    } else if req.origin == Dialog::CommitChangelist {
                        let mut d = w.get_list_dialog();
                        d.error = error_info(&e);
                        w.set_list_dialog(d);
                    } else {
                        let mut c = w.get_changes();
                        c.error = error_info(&e);
                        c.refused = super::work::refused(&e);
                        w.set_changes(c);
                    }
                    me.refresh(req.root);
                }
            }
        });
    }
}

fn python_banner(w: &AppWindow, text: &str) {
    let v = w.global::<Validation>();
    if !v.get_python_seen() {
        v.set_python_seen(true);
        v.set_python_warning(text.into());
    }
}

fn show_results(w: &AppWindow, heading: &str, results: &Value) {
    let v = w.global::<Validation>();
    v.set_stage("results".into());
    v.set_heading(heading.into());
    v.set_allow_commit(false);
    v.set_allow_shelve(false);
    v.set_shelf_required(false);
    v.set_retryable(false);
    let rows = model::rows(results)
        .into_iter()
        .map(|r| ValidationRow {
            kind: r.kind.into(),
            label: r.label.into(),
            path: r.path.into(),
            line: r.line,
        })
        .collect::<Vec<_>>();
    v.set_rows(ModelRc::from(rows.as_slice()));
}
