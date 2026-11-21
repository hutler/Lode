//! The open folder is read at the workspace revision. Only one page of previews is decoded.
use super::read::View;
use super::{human_bytes, spawn, App, AppWindow, AssetCard, Assets, DepotFolder, Pane, Reading};
use crate::asset_preview;
use serde_json::Value;
use slint::{ComponentHandle, Image, ModelRc, SharedPixelBuffer};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;

const PAGE: usize = 18;
#[derive(Default)]
pub(super) struct Browser {
    // Parent -> its visible child directory paths. Only expanded nodes have entries.
    dirs: RefCell<BTreeMap<String, Vec<String>>>,
    cards: RefCell<Vec<AssetCard>>,
    key: RefCell<(String, String, String)>,
    page: Cell<usize>,
    generation: Cell<u64>,
    watch_generation: Cell<u64>,
}

struct Entry {
    path: String,
    name: String,
    size: u64,
    local: bool,
    writable: bool,
    pixels: Option<image::RgbaImage>,
}
struct Page {
    dirs: Vec<String>,
    files: Vec<Entry>,
    total: usize,
    index: usize,
}

fn page_data(root: &Path, folder: &str, listing: &Value, local_only: bool, index: usize) -> Page {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let canonical = root.canonicalize().ok();
    for entry in listing["entries"].as_array().into_iter().flatten() {
        let Some(name) = entry["name"].as_str() else {
            continue;
        };
        let path = if folder.is_empty() {
            name.to_owned()
        } else {
            format!("{folder}/{name}")
        };
        if name.contains('/') || lode_core::DepotPath::parse(&path).is_err() {
            continue;
        }
        if entry["kind"] == "dir" {
            if !local_only
                || canonical.as_ref().is_some_and(|r| {
                    r.join(&path)
                        .canonicalize()
                        .is_ok_and(|p| p.starts_with(r) && p.is_dir())
                })
            {
                dirs.push(path);
            }
        } else {
            let regular = entry["kind"] == "file";
            if local_only
                && (!regular
                    || canonical
                        .as_deref()
                        .and_then(|r| asset_preview::local_file(r, &path))
                        .is_none())
            {
                continue;
            }
            files.push((
                name,
                path,
                entry["size"].as_u64().unwrap_or_default(),
                regular,
            ));
        }
    }
    dirs.sort();
    files.sort_by(|a, b| a.0.cmp(b.0));
    let total = files.len();
    let index = index.min(total.saturating_sub(1) / PAGE);
    let files = files
        .into_iter()
        .skip(index * PAGE)
        .take(PAGE)
        .map(|(name, path, size, regular)| {
            let local = canonical
                .as_deref()
                .filter(|_| regular)
                .and_then(|r| asset_preview::local_file(r, &path));
            let writable = local
                .as_ref()
                .and_then(|p| p.metadata().ok())
                .is_some_and(|m| !m.permissions().readonly());
            let pixels = local.as_deref().and_then(asset_preview::load);
            Entry {
                path,
                name: name.into(),
                size,
                local: local.is_some(),
                writable,
                pixels,
            }
        })
        .collect();
    Page {
        dirs,
        files,
        total,
        index,
    }
}

pub(super) fn connect(w: &AppWindow, app: &Rc<App>) {
    let ui = w.global::<Assets>();
    let a = app.clone();
    ui.on_browse(move |path| {
        let folder = path.to_string();
        let current = a.window.upgrade().is_some_and(|w| {
            w.global::<Reading>().get_pane() == Pane::Files
                && w.global::<Assets>().get_folder() == path
        });
        if current && !folder.is_empty() && a.assets.dirs.borrow().contains_key(&folder) {
            a.assets
                .dirs
                .borrow_mut()
                .retain(|k, _| k != &folder && !k.starts_with(&format!("{folder}/")));
            if let Some(w) = a.window.upgrade() {
                a.folder_rows(&w);
            }
            return;
        }
        a.assets.page.set(0);
        a.open_view(View::Files(folder));
    });
    let a = app.clone();
    ui.on_refresh(move || a.refresh_assets());
    let a = app.clone();
    ui.on_watch(move || {
        let Some(w) = a.window.upgrade() else { return };
        let ui = w.global::<Assets>();
        let watch =
            !ui.get_watching() && !ui.get_selected().mine && !ui.get_selected().holder.is_empty();
        let generation = a.assets.watch_generation.get() + 1;
        a.assets.watch_generation.set(generation);
        ui.set_watching(watch);
        if watch {
            a.watch_asset(generation);
        }
    });
    let a = app.clone();
    ui.on_change_page(move |d| {
        a.assets
            .page
            .set(a.assets.page.get().saturating_add_signed(d as isize));
        a.refresh_assets();
    });
    let a = app.clone();
    ui.on_scope(move |local| {
        if let Some(w) = a.window.upgrade() {
            w.global::<Assets>().set_local_only(local);
            a.assets.page.set(0);
            a.assets.dirs.borrow_mut().clear();
            a.open_view(View::Files(String::new()));
        }
    });
    let a = app.clone();
    ui.on_pick(move |p| {
        if let Some(w) = a.window.upgrade() {
            if let Some(c) = a.assets.cards.borrow().iter().find(|c| c.path == p) {
                a.stop_asset_watch(&w);
                w.global::<Assets>().set_selected(c.clone());
            }
        }
    });
    let a = app.clone();
    ui.on_take(move || {
        if let Some(w) = a.window.upgrade() {
            let ui = w.global::<Assets>();
            let s = ui.get_selected();
            if !ui.get_busy() && ui.get_lock_known() && s.local && s.holder.is_empty() {
                a.ask_lock_path(&w, s.path.into());
            }
        }
    });
    let a = app.clone();
    ui.on_release(move || {
        if let Some(w) = a.window.upgrade() {
            let ui = w.global::<Assets>();
            let s = ui.get_selected();
            if !ui.get_busy() && ui.get_lock_known() && s.mine {
                ui.set_busy(true);
                a.release(vec![s.path.into()], false);
            }
        }
    });
}

impl App {
    pub(super) fn stop_asset_watch(&self, w: &AppWindow) {
        self.assets
            .watch_generation
            .set(self.assets.watch_generation.get() + 1);
        w.global::<Assets>().set_watching(false);
    }
    fn watch_asset(&self, generation: u64) {
        let weak = self.this.borrow().clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
            let Some(me) = weak.upgrade() else { return };
            if me.assets.watch_generation.get() != generation {
                return;
            }
            let Some(root) = me.shown_root.borrow().clone() else {
                return;
            };
            let Some(w) = me.window.upgrade() else { return };
            let selected = w.global::<Assets>().get_selected();
            spawn(async move {
                let result = me.desk.locks(root, false).await;
                if me.assets.watch_generation.get() != generation {
                    return;
                }
                let Some(w) = me.window.upgrade() else { return };
                let ui = w.global::<Assets>();
                match result {
                    Err(e) => {
                        me.stop_asset_watch(&w);
                        ui.set_lock_known(false);
                        ui.set_message(e.message.into());
                    }
                    Ok(v) => {
                        let lock = v["locks"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|l| l["path"].as_str() == Some(selected.path.as_str()));
                        if lock.is_some_and(|l| {
                            l["workspace"].as_str() == Some(selected.workspace.as_str())
                                && l["owner"]["name"].as_str() == Some(selected.holder.as_str())
                                && l["comment"].as_str().unwrap_or("") == selected.comment.as_str()
                        }) {
                            me.watch_asset(generation);
                        } else {
                            me.refresh_assets();
                        }
                    }
                }
            });
        });
    }
    pub(super) fn reset_assets(&self, w: &AppWindow) {
        self.stop_asset_watch(w);
        self.assets.generation.set(self.assets.generation.get() + 1);
        self.assets.dirs.borrow_mut().clear();
        self.assets.cards.borrow_mut().clear();
        self.assets.page.set(0);
        let ui = w.global::<Assets>();
        ui.set_selected(AssetCard::default());
        ui.set_folders(ModelRc::default());
        ui.set_rows(ModelRc::default());
        ui.set_folder("".into());
        ui.set_message("".into());
        ui.set_lock_known(false);
        ui.set_busy(false);
    }
    fn folder_rows(&self, w: &AppWindow) {
        fn append(
            parent: &str,
            depth: i32,
            dirs: &BTreeMap<String, Vec<String>>,
            out: &mut Vec<DepotFolder>,
        ) {
            if let Some(children) = dirs.get(parent) {
                for child in children {
                    out.push(DepotFolder {
                        path: child.into(),
                        caption: child.rsplit('/').next().unwrap_or(child).into(),
                        depth,
                        expanded: dirs.contains_key(child),
                    });
                    append(child, depth + 1, dirs, out);
                }
            }
        }
        let mut rows = Vec::new();
        append("", 0, &self.assets.dirs.borrow(), &mut rows);
        w.global::<Assets>().set_folders(rows.as_slice().into());
    }
    pub(super) fn reload_assets_for(&self, root: &str) {
        if self.shown_root.borrow().as_deref() == Some(root) {
            self.refresh_assets();
        }
    }
    fn refresh_assets(&self) {
        if let Some(w) = self.window.upgrade() {
            if w.global::<Reading>().get_pane() == Pane::Files {
                self.load_assets(w.global::<Assets>().get_folder().into());
            }
        }
    }
    pub(super) fn load_assets(&self, folder: String) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let Some(root) = self.shown_root.borrow().clone() else {
            return;
        };
        let Some(tab) = self.workspaces_row(&root) else {
            return;
        };
        let key = (root.clone(), tab.branch.to_string(), tab.rev.to_string());
        if *self.assets.key.borrow() != key {
            self.reset_assets(&w);
            *self.assets.key.borrow_mut() = key;
        }
        self.stop_asset_watch(&w);
        let ui = w.global::<Assets>();
        let local = ui.get_local_only();
        let idx = self.assets.page.get();
        let keep = if ui.get_folder().as_str() == folder {
            ui.get_selected().path
        } else {
            Default::default()
        };
        ui.set_folder(folder.as_str().into());
        ui.set_busy(true);
        ui.set_message("Loading folder…".into());
        ui.set_selected(AssetCard::default());
        ui.set_lock_known(false);
        ui.set_previous(false);
        ui.set_next(false);
        ui.set_rows(ModelRc::default());
        ui.set_page_label("".into());
        self.assets.cards.borrow_mut().clear();
        let generation = self.assets.generation.get() + 1;
        self.assets.generation.set(generation);
        let Some(me) = self.me() else { return };
        spawn(async move {
            let listing = me.desk.view_tree(root.clone(), Some(folder.clone())).await;
            let live = || {
                me.assets.generation.get() == generation
                    && me
                        .window
                        .upgrade()
                        .is_some_and(|w| w.global::<Reading>().get_pane() == Pane::Files)
            };
            if !live() {
                return;
            }
            let result = async {
                let listing = listing.map_err(|e| e.message)?;
                let locks = me.desk.locks(root.clone(), false).await;
                let dir = folder.clone();
                let (send, recv) = tokio::sync::oneshot::channel();
                std::thread::Builder::new()
                    .name("desk-asset-previews".into())
                    .spawn(move || {
                        let _ = send.send(page_data(Path::new(&root), &dir, &listing, local, idx));
                    })
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((recv.await.map_err(|e| e.to_string())?, locks))
            }
            .await;
            if !live() {
                return;
            }
            let Some(w) = me.window.upgrade() else { return };
            let ui = w.global::<Assets>();
            ui.set_busy(false);
            let (page, locks) = match result {
                Ok(v) => v,
                Err(e) => {
                    ui.set_message(e.into());
                    return;
                }
            };
            let mut message = if page.total == 0 {
                "No files in this folder. Select a subfolder in the tree.".to_owned()
            } else {
                String::new()
            };
            ui.set_lock_known(locks.is_ok());
            let locks = locks.unwrap_or_else(|e| {
                message = format!("Cannot read locks: {}", e.message);
                Value::Null
            });
            let by_path: HashMap<_, _> = locks["locks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| Some((l["path"].as_str()?, l)))
                .collect();
            {
                let dirs = &mut *me.assets.dirs.borrow_mut();
                dirs.insert(folder.clone(), page.dirs);
                // Rebuild the ancestry when restoring a previously opened workspace tab.
                let mut child = folder.as_str();
                while !child.is_empty() {
                    let parent = child.rsplit_once('/').map_or("", |(p, _)| p);
                    let siblings = dirs.entry(parent.into()).or_default();
                    if !siblings.iter().any(|s| s == child) {
                        siblings.push(child.into());
                        siblings.sort();
                    }
                    child = parent;
                }
            }
            let cards: Vec<_> = page
                .files
                .into_iter()
                .map(|f| {
                    let l = by_path
                        .get(f.path.as_str())
                        .copied()
                        .unwrap_or(&Value::Null);
                    let kind = crate::changes::file_kind(&f.path);
                    let img = f.pixels.map(|pixels| {
                        Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                            pixels.as_raw(),
                            pixels.width(),
                            pixels.height(),
                        ))
                    });
                    AssetCard {
                        path: f.path.into(),
                        name: f.name.into(),
                        kind: kind.into(),
                        local: f.local,
                        writable: f.writable,
                        preview: img.is_some(),
                        picture: img.unwrap_or_default(),
                        size: human_bytes(f.size).into(),
                        holder: l["owner"]["name"].as_str().unwrap_or("").into(),
                        mine: l["mine"].as_bool().unwrap_or(false),
                        workspace: l["workspace"].as_str().unwrap_or("").into(),
                        comment: l["comment"].as_str().unwrap_or("").into(),
                    }
                })
                .collect();
            ui.set_rows(cards.as_slice().into());
            if let Some(c) = cards.iter().find(|c| c.path == keep) {
                ui.set_selected(c.clone());
            }
            me.assets.page.set(page.index);
            ui.set_previous(page.index > 0);
            ui.set_next((page.index + 1) * PAGE < page.total);
            ui.set_page_label(if page.total == 0 {
                "0 files".into()
            } else {
                format!(
                    "{}–{} of {}",
                    page.index * PAGE + 1,
                    page.index * PAGE + cards.len(),
                    page.total
                )
                .into()
            });
            ui.set_message(message.into());
            *me.assets.cards.borrow_mut() = cards;
            me.folder_rows(&w);
        });
    }
}
