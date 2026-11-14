//! History, blame and diffs of an open workspace (T132). The views draw the global `Reading`;
//! this module fills it and answers its callbacks. One workspace's view shows at a time, and
//! each workspace remembers which one it was on.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use serde_json::Value;
use slint::{ComponentHandle, Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use super::{error_info, human_bytes, spawn, App, AppWindow, ErrorInfo};
use super::{BlameLine, BlameState, DiffNote, DiffRow, DiffState, HistoryState, ImageState};
use super::{Pane, Reading, RevDetail, RevFile, RevRow, WorkspaceTab};
use crate::desk::{ErrorView, Side, IMAGE_LIMIT};
use crate::diffs::{self, Facts, Hunk, Layout};
use crate::history::{self, PAGE};

/// What a workspace shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum View {
    #[default]
    Changes,
    /// A branch's history (`path` none) or a file's, with a revision selected.
    History {
        path: Option<String>,
        rev: Option<u64>,
    },
    /// A file at a revision, or at the workspace's.
    Blame { path: String, rev: Option<u64> },
    /// A file as a revision changed it, or the working copy against its base.
    Diff { path: String, rev: Option<u64> },
}

/// The models and loaded state behind `Reading`.
pub(super) struct ReadState {
    revs: Rc<VecModel<RevRow>>,
    files: Rc<VecModel<RevFile>>,
    blame: Rc<VecModel<BlameLine>>,
    diff: Rc<VecModel<DiffRow>>,
    /// Bumped each time a view opens; answers for an older one are dropped.
    generation: Cell<u64>,
    /// The loaded revisions of the history, newest first.
    revisions: RefCell<Vec<Value>>,
    branch: RefCell<String>,
    selected: Cell<Option<u64>>,
    /// The diff's hunks, laid out again when the layout changes.
    hunks: RefCell<Vec<Hunk>>,
    layout: Cell<Layout>,
    image_mode: RefCell<String>,
    prefs_file: std::path::PathBuf,
}

/// The layout and image comparison chosen last, kept between runs.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Prefs {
    #[serde(default)]
    diff_layout: String,
    #[serde(default)]
    image_mode: String,
}

impl ReadState {
    pub(super) fn new(window: &AppWindow, data_dir: &Path) -> Self {
        let prefs_file = data_dir.join("prefs.json");
        let prefs: Prefs = std::fs::read(&prefs_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let st = Self {
            revs: Rc::new(VecModel::default()),
            files: Rc::new(VecModel::default()),
            blame: Rc::new(VecModel::default()),
            diff: Rc::new(VecModel::default()),
            generation: Cell::new(0),
            revisions: RefCell::new(Vec::new()),
            branch: RefCell::new(String::new()),
            selected: Cell::new(None),
            hunks: RefCell::new(Vec::new()),
            layout: Cell::new(Layout::parse(&prefs.diff_layout)),
            image_mode: RefCell::new(match prefs.image_mode.as_str() {
                m @ ("swipe" | "onion") => m.to_owned(),
                _ => "side".to_owned(),
            }),
            prefs_file,
        };
        let r = window.global::<Reading>();
        r.set_revs(Rc::clone(&st.revs).into());
        r.set_files(Rc::clone(&st.files).into());
        r.set_blame_lines(Rc::clone(&st.blame).into());
        r.set_diff_rows(Rc::clone(&st.diff).into());
        r.set_mono(
            if cfg!(windows) {
                "Consolas"
            } else if cfg!(target_os = "macos") {
                "Menlo"
            } else {
                "DejaVu Sans Mono"
            }
            .into(),
        );
        st
    }

    fn save_prefs(&self) {
        let p = Prefs {
            diff_layout: self.layout.get().name().into(),
            image_mode: self.image_mode.borrow().clone(),
        };
        if let Ok(b) = serde_json::to_vec_pretty(&p) {
            // A preference only; the app works the same when it cannot be kept.
            let _ = std::fs::write(&self.prefs_file, b);
        }
    }
}

pub(super) fn connect(window: &AppWindow, app: &Rc<App>) {
    let r = window.global::<Reading>();
    let a = Rc::clone(app);
    r.on_show_pane(move |p| {
        let view = match p {
            Pane::Changes => View::Changes,
            _ => View::History {
                path: None,
                rev: None,
            },
        };
        a.open_view(view);
    });
    let a = Rc::clone(app);
    r.on_select_rev(move |rev| a.select_rev(u64::try_from(rev).ok()));
    let a = Rc::clone(app);
    r.on_step_rev(move |d| a.step_rev(d));
    let a = Rc::clone(app);
    r.on_more_revs(move || a.more_revs());
    let a = Rc::clone(app);
    r.on_refresh_history(move || {
        let rev = a.read.selected.get();
        a.open_view(View::History { path: None, rev });
    });
    let a = Rc::clone(app);
    r.on_branch_history(move || {
        a.open_view(View::History {
            path: None,
            rev: None,
        });
    });
    let a = Rc::clone(app);
    r.on_file_history(move |p| {
        a.open_view(View::History {
            path: Some(p.to_string()),
            rev: None,
        });
    });
    let a = Rc::clone(app);
    r.on_blame_file(move |p, rev| {
        a.open_view(View::Blame {
            path: p.to_string(),
            rev: u64::try_from(rev).ok(),
        });
    });
    let a = Rc::clone(app);
    r.on_diff_file(move |p, rev| {
        a.open_view(View::Diff {
            path: p.to_string(),
            rev: u64::try_from(rev).ok(),
        });
    });
    let a = Rc::clone(app);
    r.on_show_revision(move |rev| {
        a.open_view(View::History {
            path: None,
            rev: u64::try_from(rev).ok(),
        });
    });
    let a = Rc::clone(app);
    r.on_set_layout(move |l| a.set_layout(Layout::parse(&l)));
    let a = Rc::clone(app);
    r.on_set_image_mode(move |m| a.set_image_mode(&m));
    let a = Rc::clone(app);
    r.on_back(move || a.back());
    let a = Rc::clone(app);
    r.on_dismiss_error(move || {
        if let Some(w) = a.window.upgrade() {
            let r = w.global::<Reading>();
            let mut h = r.get_history();
            h.error = ErrorInfo::default();
            r.set_history(h);
            let mut b = r.get_blame();
            b.error = ErrorInfo::default();
            r.set_blame(b);
            let mut d = r.get_diff();
            d.error = ErrorInfo::default();
            r.set_diff(d);
        }
    });
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn size_text(n: Option<u64>) -> SharedString {
    n.map_or_else(|| "none".into(), |n| human_bytes(n).into())
}

/// An image's pixels, or why it cannot be shown.
type Decoded = Result<SharedPixelBuffer<Rgba8Pixel>, String>;

fn decode(bytes: &[u8]) -> Decoded {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    Ok(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

/// Decodes on a thread of its own, so a large image does not stall the window.
async fn decode_off_thread(bytes: Vec<u8>) -> Decoded {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(decode(&bytes));
    });
    rx.await
        .unwrap_or_else(|_| Err("the image could not be read".into()))
}

impl App {
    fn reading(&self) -> Option<(AppWindow, String)> {
        let w = self.window.upgrade()?;
        let root = self.shown_root.borrow().clone()?;
        Some((w, root))
    }

    /// Shows the view `root` was on (after a tab change).
    pub(super) fn restore_view(&self, root: &str) {
        let view = self
            .work
            .borrow()
            .get(root)
            .map(|s| s.view.clone())
            .unwrap_or_default();
        self.open_view(view);
    }

    /// Opens `view` in the workspace that shows, and loads what it needs.
    fn open_view(&self, view: View) {
        let Some((w, root)) = self.reading() else {
            return;
        };
        if let Some(st) = self.work.borrow_mut().get_mut(&root) {
            st.view = view.clone();
        }
        let generation = self.read.generation.get() + 1;
        self.read.generation.set(generation);
        let r = w.global::<Reading>();
        match view {
            View::Changes => r.set_pane(Pane::Changes),
            View::History { path, rev } => {
                r.set_pane(Pane::History);
                self.load_history(&w, root, path, rev, generation);
            }
            View::Blame { path, rev } => {
                r.set_pane(Pane::Blame);
                self.load_blame(&w, root, path, rev, generation);
            }
            View::Diff { path, rev } => {
                r.set_pane(Pane::Diff);
                self.load_diff(&w, root, path, rev, generation);
            }
        }
    }

    /// The tab of the workspace that shows.
    fn current_tab(&self, _w: &AppWindow) -> Option<WorkspaceTab> {
        let root = self.shown_root.borrow().clone()?;
        (0..self.workspaces.row_count())
            .filter_map(|i| self.workspaces.row_data(i))
            .find(|t| t.root == root.as_str())
    }

    fn current(&self, generation: u64) -> bool {
        self.read.generation.get() == generation
    }

    fn load_history(
        &self,
        w: &AppWindow,
        root: String,
        path: Option<String>,
        rev: Option<u64>,
        generation: u64,
    ) {
        let r = w.global::<Reading>();
        let tab = self.current_tab(w);
        let branch = tab
            .as_ref()
            .map(|t| t.branch.to_string())
            .unwrap_or_default();
        let depot = tab
            .as_ref()
            .map(|t| t.depot.to_string())
            .unwrap_or_default();
        *self.read.revisions.borrow_mut() = Vec::new();
        *self.read.branch.borrow_mut() = branch.clone();
        self.read.selected.set(rev);
        self.read.revs.set_vec(Vec::new());
        self.read.files.set_vec(Vec::new());
        r.set_detail(RevDetail::default());
        r.set_history(HistoryState {
            path: path.clone().unwrap_or_default().into(),
            title: match &path {
                Some(p) => format!("History of {}", name_of(p)),
                None => format!("History of {branch}"),
            }
            .into(),
            subtitle: path.clone().unwrap_or(depot).into(),
            loading: true,
            lanes: 1,
            scroll_to: -1,
            ..HistoryState::default()
        });
        if rev.is_some() {
            self.load_detail(root.clone(), rev, generation);
        }
        self.load_page(root, path, None, generation);
    }

    /// Loads the page of revisions older than `before` and adds it to the list.
    fn load_page(&self, root: String, path: Option<String>, before: Option<u64>, generation: u64) {
        let Some(me) = self.me() else { return };
        spawn(async move {
            let page = me.desk.log(root.clone(), path.clone(), before, PAGE).await;
            if !me.current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let r = w.global::<Reading>();
            let mut h = r.get_history();
            h.loading = false;
            match page {
                Ok(p) => {
                    let new = p["revisions"].as_array().cloned().unwrap_or_default();
                    h.more = p["more"].as_bool().unwrap_or(false);
                    if let Some(b) = p["branch"].as_str() {
                        *me.read.branch.borrow_mut() = b.to_owned();
                    }
                    let first = before.is_none() && me.read.selected.get().is_none();
                    me.read.revisions.borrow_mut().extend(new);
                    if first {
                        let newest = me
                            .read
                            .revisions
                            .borrow()
                            .first()
                            .and_then(|v| v["rev"].as_u64());
                        if newest.is_some() {
                            me.read.selected.set(newest);
                            me.load_detail(root.clone(), newest, generation);
                        }
                    }
                    let revisions = me.read.revisions.borrow();
                    h.empty = if revisions.is_empty() {
                        if path.is_some() {
                            "No revision changed this file."
                        } else {
                            "No revisions yet."
                        }
                    } else {
                        ""
                    }
                    .into();
                    h.lanes = if revisions
                        .iter()
                        .any(|v| v["merged_from"].as_array().is_some_and(|m| !m.is_empty()))
                    {
                        2
                    } else {
                        1
                    };
                    let rows = history::rows(
                        &revisions,
                        &me.read.branch.borrow(),
                        h.more,
                        me.read.selected.get(),
                    );
                    sync(&me.read.revs, rows);
                }
                Err(e) => h.error = error_info(&e),
            }
            r.set_history(h);
        });
    }

    fn more_revs(&self) {
        let Some((w, root)) = self.reading() else {
            return;
        };
        let r = w.global::<Reading>();
        let mut h = r.get_history();
        if !h.more || h.loading {
            return;
        }
        let Some(oldest) = self
            .read
            .revisions
            .borrow()
            .last()
            .and_then(|v| v["rev"].as_u64())
        else {
            return;
        };
        h.loading = true;
        let path = (!h.path.is_empty()).then(|| h.path.to_string());
        r.set_history(h);
        self.load_page(root, path, Some(oldest), self.read.generation.get());
    }

    fn select_rev(&self, rev: Option<u64>) {
        let Some((w, root)) = self.reading() else {
            return;
        };
        self.read.selected.set(rev);
        if let Some(st) = self.work.borrow_mut().get_mut(&root) {
            if let View::History { rev: r, .. } = &mut st.view {
                *r = rev;
            }
        }
        let mut at = -1;
        for i in 0..self.read.revs.row_count() {
            if let Some(mut row) = self.read.revs.row_data(i) {
                let on = u64::try_from(row.rev).ok() == rev;
                if on {
                    at = i32::try_from(i).unwrap_or(-1);
                }
                if row.selected != on {
                    row.selected = on;
                    self.read.revs.set_row_data(i, row);
                }
            }
        }
        let r = w.global::<Reading>();
        let mut h = r.get_history();
        h.scroll_to = at;
        r.set_history(h);
        self.load_detail(root, rev, self.read.generation.get());
    }

    /// Selects the revision `step` rows below (or above, when negative) the selected one.
    fn step_rev(&self, step: i32) {
        let revisions = self.read.revisions.borrow();
        let at = revisions
            .iter()
            .position(|v| v["rev"].as_u64() == self.read.selected.get());
        let next = match at {
            Some(i) => i.checked_add_signed(isize::try_from(step).unwrap_or(0)),
            None => Some(0),
        };
        let Some(rev) = next
            .and_then(|i| revisions.get(i))
            .and_then(|v| v["rev"].as_u64())
        else {
            return;
        };
        drop(revisions);
        self.select_rev(Some(rev));
    }

    fn load_detail(&self, root: String, rev: Option<u64>, generation: u64) {
        let Some(me) = self.me() else { return };
        let Some(rev) = rev else {
            if let Some(w) = self.window.upgrade() {
                w.global::<Reading>().set_detail(RevDetail::default());
                self.read.files.set_vec(Vec::new());
            }
            return;
        };
        spawn(async move {
            let got = me.desk.revision(root, rev).await;
            if !me.current(generation) || me.read.selected.get() != Some(rev) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let r = w.global::<Reading>();
            match got {
                Ok(v) => {
                    let (d, files) = history::detail(&v, &me.read.branch.borrow());
                    r.set_detail(d);
                    me.read.files.set_vec(files);
                }
                Err(e) => {
                    let mut h = r.get_history();
                    h.error = error_info(&e);
                    r.set_history(h);
                }
            }
        });
    }

    fn load_blame(
        &self,
        w: &AppWindow,
        root: String,
        path: String,
        rev: Option<u64>,
        generation: u64,
    ) {
        let r = w.global::<Reading>();
        self.read.blame.set_vec(Vec::new());
        r.set_blame(BlameState {
            path: path.as_str().into(),
            name: name_of(&path).into(),
            loading: true,
            ..BlameState::default()
        });
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = me.desk.blame(root, path, rev).await;
            if !me.current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let r = w.global::<Reading>();
            let mut b = r.get_blame();
            b.loading = false;
            match got {
                Ok(v) => {
                    b.at = v["rev"]
                        .as_u64()
                        .map(|r| format!(" at r{r}"))
                        .unwrap_or_default()
                        .into();
                    b.truncated = v["truncated"].as_bool().unwrap_or(false);
                    me.read.blame.set_vec(history::blame_lines(&v));
                }
                Err(e) => b.error = error_info(&e),
            }
            r.set_blame(b);
        });
    }

    fn load_diff(
        &self,
        w: &AppWindow,
        root: String,
        path: String,
        rev: Option<u64>,
        generation: u64,
    ) {
        let r = w.global::<Reading>();
        self.read.diff.set_vec(Vec::new());
        self.read.hunks.borrow_mut().clear();
        r.set_image(ImageState::default());
        r.set_diff(DiffState {
            path: path.as_str().into(),
            name: name_of(&path).into(),
            r#where: match rev {
                Some(r) => format!("{path} in r{r}"),
                None => format!("{path} (working copy)"),
            }
            .into(),
            back_label: if rev.is_some() {
                "Back to History"
            } else {
                "Back to Changes"
            }
            .into(),
            loading: true,
            layout: self.read.layout.get().name().into(),
            ..DiffState::default()
        });
        let Some(me) = self.me() else { return };
        spawn(async move {
            let got = match rev {
                None => me.desk.diff(root.clone(), path.clone()).await,
                Some(r) => me.desk.diff_revision(root.clone(), r, path.clone()).await,
            };
            if !me.current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let r = w.global::<Reading>();
            let mut d = r.get_diff();
            d.loading = false;
            match got {
                Ok(Value::Null) => d.none = true,
                Ok(v) => {
                    let image = me.show_facts(&mut d, &Facts::from_json(&v));
                    if image {
                        r.set_diff(d);
                        me.load_images(root, path, rev, generation);
                        return;
                    }
                }
                Err(e) => d.error = error_info(&e),
            }
            r.set_diff(d);
        });
    }

    /// Fills the diff's header and body from its facts; true when it is an image diff whose
    /// sides are still to be loaded.
    fn show_facts(&self, d: &mut DiffState, f: &Facts) -> bool {
        d.binary = f.binary;
        d.insertions = i32::try_from(f.insertions).unwrap_or(i32::MAX);
        d.deletions = i32::try_from(f.deletions).unwrap_or(i32::MAX);
        if let Some(old) = f.old_path.as_ref().filter(|o| **o != f.path) {
            d.r#where = format!("{old} → {}", d.r#where).into();
        }
        let note = |why: &str, title: &str| DiffNote {
            shown: true,
            title: title.into(),
            ext: diffs::extension(&f.path).into(),
            before: size_text(f.old_size),
            after: size_text(f.new_size),
            mode: f.mode_change().unwrap_or_default().into(),
            why: why.into(),
        };
        if f.binary {
            if diffs::is_image(&f.path) {
                if f.old_size.unwrap_or(0) > IMAGE_LIMIT || f.new_size.unwrap_or(0) > IMAGE_LIMIT {
                    d.note = note("Images over 64 MB are compared by size only.", "Image");
                    return false;
                }
                d.image = true;
                return true;
            }
            d.note = if f.too_big() {
                note(
                    "Lode compares files over 16 MB by size only.",
                    "Too large to compare as text",
                )
            } else {
                note("Binary content is not shown as a diff.", "Binary file")
            };
            return false;
        }
        if f.patch.len() > diffs::PLAIN_LIMIT {
            d.plain = format!(
                "This diff is large ({}), so it is shown as plain text, without line numbers or highlights.",
                human_bytes(f.patch.len() as u64)
            )
            .into();
            self.read.diff.set_vec(diffs::plain_rows(&f.patch));
            return false;
        }
        let hunks = diffs::parse_patch(&f.patch);
        if hunks.is_empty() {
            d.empty = match f.mode_change() {
                Some(m) => format!("Only the mode changed: {m}."),
                None => "The content is the same.".into(),
            }
            .into();
        }
        self.read
            .diff
            .set_vec(diffs::rows(&hunks, self.read.layout.get()));
        *self.read.hunks.borrow_mut() = hunks;
        false
    }

    fn load_images(&self, root: String, path: String, rev: Option<u64>, generation: u64) {
        let Some(me) = self.me() else { return };
        if let Some(w) = self.window.upgrade() {
            w.global::<Reading>().set_image(ImageState {
                loading: true,
                mode: self.read.image_mode.borrow().as_str().into(),
                ..ImageState::default()
            });
        }
        spawn(async move {
            let old = me
                .desk
                .image_side(root.clone(), path.clone(), rev, Side::Old)
                .await;
            let new = me.desk.image_side(root, path, rev, Side::New).await;
            let sides = match (old, new) {
                (Ok(o), Ok(n)) => (o, n),
                (Err(e), _) | (_, Err(e)) => {
                    me.image_failed(generation, Some(&e), "");
                    return;
                }
            };
            let mut decoded = Vec::new();
            for side in [sides.0, sides.1] {
                decoded.push(match side {
                    None => None,
                    Some(c) => match c.bytes {
                        None => {
                            me.image_failed(generation, None, "Images over 64 MB are compared by size only.");
                            return;
                        }
                        Some(b) => match decode_off_thread(b).await {
                            Ok(px) => Some((px, c.size)),
                            Err(_) => {
                                me.image_failed(
                                    generation,
                                    None,
                                    "The app cannot show this image format, so it is compared by size only.",
                                );
                                return;
                            }
                        },
                    },
                });
            }
            if !me.current(generation) {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let caption =
                |label: &str, s: &Option<(SharedPixelBuffer<Rgba8Pixel>, u64)>| -> SharedString {
                    s.as_ref()
                        .map(|(px, size)| {
                            format!(
                                "{label}   {} × {}   {}",
                                px.width(),
                                px.height(),
                                human_bytes(*size)
                            )
                        })
                        .unwrap_or_default()
                        .into()
                };
            let (old, new) = (decoded.remove(0), decoded.remove(0));
            let both = old.is_some() && new.is_some();
            let mode = if both {
                me.read.image_mode.borrow().clone()
            } else {
                "side".to_owned()
            };
            w.global::<Reading>().set_image(ImageState {
                loading: false,
                both,
                mode: mode.into(),
                has_old: old.is_some(),
                has_new: new.is_some(),
                old_caption: caption("Before", &old),
                new_caption: caption("After", &new),
                note: if both {
                    ""
                } else if old.is_some() {
                    "The file was deleted."
                } else {
                    "The file is new."
                }
                .into(),
                old: old.map(|(px, _)| Image::from_rgba8(px)).unwrap_or_default(),
                new: new.map(|(px, _)| Image::from_rgba8(px)).unwrap_or_default(),
            });
        });
    }

    /// The images cannot be shown: the error, or a note with the sizes and why.
    fn image_failed(&self, generation: u64, e: Option<&ErrorView>, why: &str) {
        if !self.current(generation) {
            return;
        }
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let r = w.global::<Reading>();
        r.set_image(ImageState::default());
        let mut d = r.get_diff();
        d.image = false;
        match e {
            Some(e) => d.error = error_info(e),
            None => {
                d.note = DiffNote {
                    shown: true,
                    title: "Image".into(),
                    ext: diffs::extension(&d.path).into(),
                    why: why.into(),
                    ..DiffNote::default()
                };
            }
        }
        r.set_diff(d);
    }

    fn set_layout(&self, layout: Layout) {
        self.read.layout.set(layout);
        self.read.save_prefs();
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let r = w.global::<Reading>();
        let mut d = r.get_diff();
        d.layout = layout.name().into();
        r.set_diff(d);
        let hunks = self.read.hunks.borrow();
        if !hunks.is_empty() {
            self.read.diff.set_vec(diffs::rows(&hunks, layout));
        }
    }

    fn set_image_mode(&self, mode: &str) {
        let mode = match mode {
            "swipe" | "onion" => mode,
            _ => "side",
        };
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let r = w.global::<Reading>();
        let mut i = r.get_image();
        if !i.both && mode != "side" {
            return;
        }
        *self.read.image_mode.borrow_mut() = mode.to_owned();
        self.read.save_prefs();
        i.mode = mode.into();
        r.set_image(i);
    }

    fn back(&self) {
        let Some((_, root)) = self.reading() else {
            return;
        };
        let view = self.work.borrow().get(&root).map(|s| s.view.clone());
        match view {
            Some(View::Diff { rev: Some(rev), .. }) => self.open_view(View::History {
                path: None,
                rev: Some(rev),
            }),
            _ => self.open_view(View::Changes),
        }
    }
}

/// Makes `model` hold `rows`, changing only the rows that differ, so a list that grows keeps
/// its place.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old = model.row_count();
    for (i, row) in rows.iter().enumerate().take(old) {
        if model.row_data(i).as_ref() != Some(row) {
            model.set_row_data(i, row.clone());
        }
    }
    if rows.len() < old {
        for i in (rows.len()..old).rev() {
            model.remove(i);
        }
    } else {
        model.extend(rows.into_iter().skip(old));
    }
}
