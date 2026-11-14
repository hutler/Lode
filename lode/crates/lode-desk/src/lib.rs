//! Lode Desk, the Lode desktop client.
//!
//! The window is a React app (`ui/`). Each Tauri command here runs one `lode-ops` operation
//! through [`desk::Desk`] and returns the same JSON document `lode <command> --json` prints;
//! progress and warnings arrive as `lode://progress` and `lode://warning` events, and a
//! watched workspace whose files changed as `lode://changed` with its root. Operations
//! open the workspace only while they run, so `lode` and editor plugins work next to the app.
//!
//! A `lode://` link that starts the app, or reaches it while it runs, is kept until the
//! window takes it with `pending_link`; `lode://open-link` tells the window one is waiting.
//! Only one copy of the app runs: a second start hands its link to the first and exits.

#![forbid(unsafe_code)]

pub mod desk;
pub mod links;
pub mod watch;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lode_ops::output::{ProgressEvent, Ui};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::desk::{Desk, ErrorView, RecentWorkspace, ShelveRequest, Side};

/// Forwards an operation's reports to the window as events.
struct WindowUi {
    app: AppHandle,
}

impl Ui for WindowUi {
    fn line(&self, _text: &str) {}

    fn warn(&self, text: &str) {
        let _ = self.app.emit("lode://warning", text);
    }

    fn progress(&self, event: &ProgressEvent<'_>) {
        let _ = self.app.emit("lode://progress", event);
    }
}

/// The `lode://` link waiting for the window, if any.
#[derive(Default)]
struct PendingLink(Mutex<Option<String>>);

/// Keeps `link` for the window and tells it one is waiting.
fn hand_over(app: &AppHandle, link: String) {
    if let Some(pending) = app.try_state::<PendingLink>() {
        if let Ok(mut slot) = pending.0.lock() {
            *slot = Some(link);
        }
    }
    let _ = app.emit("lode://open-link", ());
}

/// Brings the main window to the front.
fn focus(app: &AppHandle) {
    if let Some(window) = app.webview_windows().values().next() {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Takes the waiting `lode://` link, as it was given; the window parses it.
#[tauri::command]
fn pending_link(pending: State<'_, PendingLink>) -> Option<String> {
    pending.0.lock().ok().and_then(|mut slot| slot.take())
}

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[tauri::command]
fn recent_workspaces(desk: State<'_, Desk>) -> Vec<RecentWorkspace> {
    desk.recent()
}

#[tauri::command]
fn forget_workspace(desk: State<'_, Desk>, path: String) -> Result<(), ErrorView> {
    desk.forget(&path)
}

#[tauri::command]
async fn signed_in(desk: State<'_, Desk>, server: String) -> Result<Value, ErrorView> {
    desk.signed_in(server).await
}

#[tauri::command]
async fn sign_in(
    desk: State<'_, Desk>,
    server: String,
    name: String,
    password: String,
) -> Result<Value, ErrorView> {
    desk.sign_in(server, name, password).await
}

#[tauri::command]
async fn open_workspace(desk: State<'_, Desk>, path: String) -> Result<Value, ErrorView> {
    desk.open_workspace(path).await
}

#[tauri::command]
async fn checkout(
    desk: State<'_, Desk>,
    target: String,
    dir: String,
    branch: String,
) -> Result<Value, ErrorView> {
    desk.checkout(target, dir, branch).await
}

#[tauri::command]
async fn add(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
) -> Result<Value, ErrorView> {
    desk.add(workspace, paths).await
}

#[tauri::command]
async fn revert(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
) -> Result<Value, ErrorView> {
    desk.revert(workspace, paths).await
}

#[tauri::command]
async fn commit(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
    message: String,
) -> Result<Value, ErrorView> {
    desk.commit(workspace, paths, message).await
}

#[tauri::command]
async fn log(
    desk: State<'_, Desk>,
    workspace: String,
    path: Option<String>,
    before: Option<u64>,
    max: u32,
) -> Result<Value, ErrorView> {
    desk.log(workspace, path, before, max).await
}

#[tauri::command]
async fn revision(desk: State<'_, Desk>, workspace: String, rev: u64) -> Result<Value, ErrorView> {
    desk.revision(workspace, rev).await
}

#[tauri::command]
async fn blame(
    desk: State<'_, Desk>,
    workspace: String,
    path: String,
    rev: Option<u64>,
) -> Result<Value, ErrorView> {
    desk.blame(workspace, path, rev).await
}

#[tauri::command]
async fn diff(desk: State<'_, Desk>, workspace: String, path: String) -> Result<Value, ErrorView> {
    desk.diff(workspace, path).await
}

#[tauri::command]
async fn diff_revision(
    desk: State<'_, Desk>,
    workspace: String,
    rev: u64,
    path: String,
) -> Result<Value, ErrorView> {
    desk.diff_revision(workspace, rev, path).await
}

#[tauri::command]
async fn commit_changelist(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
    message: Option<String>,
) -> Result<Value, ErrorView> {
    desk.commit_changelist(workspace, name, message).await
}

#[tauri::command]
async fn changelists(desk: State<'_, Desk>, workspace: String) -> Result<Value, ErrorView> {
    desk.changelists(workspace).await
}

#[tauri::command]
async fn changelist_new(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
    description: String,
) -> Result<Value, ErrorView> {
    desk.changelist_new(workspace, name, description).await
}

#[tauri::command]
async fn changelist_move(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
    paths: Vec<String>,
) -> Result<Value, ErrorView> {
    desk.changelist_move(workspace, name, paths).await
}

#[tauri::command]
async fn changelist_delete(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
) -> Result<Value, ErrorView> {
    desk.changelist_delete(workspace, name).await
}

#[tauri::command]
async fn lock(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
    comment: Option<String>,
) -> Result<Value, ErrorView> {
    desk.lock(workspace, paths, comment).await
}

#[tauri::command]
async fn unlock(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
    force: bool,
) -> Result<Value, ErrorView> {
    desk.unlock(workspace, paths, force).await
}

#[tauri::command]
async fn locks(desk: State<'_, Desk>, workspace: String, mine: bool) -> Result<Value, ErrorView> {
    desk.locks(workspace, mine).await
}

#[tauri::command]
async fn shelve(
    desk: State<'_, Desk>,
    workspace: String,
    request: ShelveRequest,
) -> Result<Value, ErrorView> {
    desk.shelve(workspace, request).await
}

#[tauri::command]
async fn shelves(
    desk: State<'_, Desk>,
    workspace: String,
    mine: bool,
    state: Option<String>,
) -> Result<Value, ErrorView> {
    desk.shelves(workspace, mine, state).await
}

#[tauri::command]
async fn shelf(desk: State<'_, Desk>, workspace: String, id: u64) -> Result<Value, ErrorView> {
    desk.shelf(workspace, id).await
}

#[tauri::command]
async fn shelf_submit(
    desk: State<'_, Desk>,
    workspace: String,
    id: u64,
    version: u64,
    message: Option<String>,
) -> Result<Value, ErrorView> {
    desk.shelf_submit(workspace, id, version, message).await
}

#[tauri::command]
async fn shelf_delete(
    desk: State<'_, Desk>,
    workspace: String,
    id: u64,
) -> Result<Value, ErrorView> {
    desk.shelf_delete(workspace, id).await
}

#[tauri::command]
async fn unshelve(desk: State<'_, Desk>, workspace: String, id: u64) -> Result<Value, ErrorView> {
    desk.unshelve(workspace, id).await
}

#[tauri::command]
async fn checks(
    desk: State<'_, Desk>,
    workspace: String,
    targets: Vec<String>,
) -> Result<Value, ErrorView> {
    desk.checks(workspace, targets).await
}

#[tauri::command]
fn open_link(url: String) -> Result<(), ErrorView> {
    desk::open_link(&url)
}

/// Raw bytes in the format of [`desk::encode_side`].
#[tauri::command]
async fn image_side(
    desk: State<'_, Desk>,
    workspace: String,
    path: String,
    rev: Option<u64>,
    side: Side,
) -> Result<tauri::ipc::Response, ErrorView> {
    let content = desk.image_side(workspace, path, rev, side).await?;
    Ok(tauri::ipc::Response::new(desk::encode_side(content)))
}

#[tauri::command]
async fn branches(desk: State<'_, Desk>, workspace: String, all: bool) -> Result<Value, ErrorView> {
    desk.branches(workspace, all).await
}

#[tauri::command]
async fn branch_create(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
    from: Option<String>,
    switch: bool,
) -> Result<Value, ErrorView> {
    desk.branch_create(workspace, name, from, switch).await
}

#[tauri::command]
async fn branch_archive(
    desk: State<'_, Desk>,
    workspace: String,
    name: String,
) -> Result<Value, ErrorView> {
    desk.branch_archive(workspace, name).await
}

#[tauri::command]
async fn switch(
    desk: State<'_, Desk>,
    workspace: String,
    branch: String,
    rev: Option<u64>,
) -> Result<Value, ErrorView> {
    desk.switch(workspace, branch, rev).await
}

#[tauri::command]
async fn update(
    desk: State<'_, Desk>,
    workspace: String,
    rev: Option<u64>,
) -> Result<Value, ErrorView> {
    desk.update(workspace, rev).await
}

#[tauri::command]
async fn commit_all(
    desk: State<'_, Desk>,
    workspace: String,
    message: String,
) -> Result<Value, ErrorView> {
    desk.commit_all(workspace, message).await
}

#[tauri::command]
async fn merge(
    desk: State<'_, Desk>,
    workspace: String,
    source: String,
    rev: Option<u64>,
) -> Result<Value, ErrorView> {
    desk.merge(workspace, source, rev).await
}

#[tauri::command]
async fn abandon_merge(desk: State<'_, Desk>, workspace: String) -> Result<Value, ErrorView> {
    desk.abandon_merge(workspace).await
}

#[tauri::command]
async fn conflict(
    desk: State<'_, Desk>,
    workspace: String,
    path: String,
) -> Result<Value, ErrorView> {
    desk.conflict(workspace, path).await
}

#[tauri::command]
async fn resolve(
    desk: State<'_, Desk>,
    workspace: String,
    paths: Vec<String>,
    using: desk::Using,
) -> Result<Value, ErrorView> {
    desk.resolve(workspace, paths, using).await
}

#[tauri::command]
async fn resolve_with(
    desk: State<'_, Desk>,
    workspace: String,
    path: String,
    content: String,
) -> Result<Value, ErrorView> {
    desk.resolve_with(workspace, path, content).await
}

#[tauri::command]
async fn view(desk: State<'_, Desk>, workspace: String) -> Result<Value, ErrorView> {
    desk.view(workspace).await
}

#[tauri::command]
async fn view_tree(
    desk: State<'_, Desk>,
    workspace: String,
    path: Option<String>,
) -> Result<Value, ErrorView> {
    desk.view_tree(workspace, path).await
}

#[tauri::command]
async fn view_apply(
    desk: State<'_, Desk>,
    workspace: String,
    include: Vec<String>,
    exclude: Vec<String>,
    force: bool,
    dry_run: bool,
) -> Result<Value, ErrorView> {
    desk.view_apply(workspace, include, exclude, force, dry_run)
        .await
}

#[tauri::command]
fn watch(desk: State<'_, Desk>, workspace: String) -> Result<(), ErrorView> {
    desk.watch(workspace)
}

#[tauri::command]
fn unwatch(desk: State<'_, Desk>, workspace: String) {
    desk.unwatch(&workspace);
}

#[tauri::command]
async fn status(desk: State<'_, Desk>, workspace: String) -> Result<Value, ErrorView> {
    desk.status(workspace).await
}

/// Starts the app.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default()
        // First, so a second start hands over its link before anything else runs.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            focus(app);
            if let Some(link) = links::from_args(args) {
                hand_over(app, link);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(PendingLink(Mutex::new(links::from_args(std::env::args()))))
        .setup(|app| {
            use tauri_plugin_deep_link::DeepLinkExt;
            // macOS delivers links as events, at start and while running.
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                if let Some(url) = event.urls().into_iter().next() {
                    hand_over(&handle, url.to_string());
                }
            });
            // Installers register the scheme; a build run from source, or an AppImage, does
            // it here.
            #[cfg(any(windows, target_os = "linux"))]
            if cfg!(debug_assertions) || std::env::var_os("APPIMAGE").is_some() {
                let _ = app.deep_link().register_all();
            }

            let paths = app.path();
            let home = paths.home_dir().unwrap_or_else(|_| PathBuf::from("."));
            let data = paths.app_data_dir()?;
            let ui = Arc::new(WindowUi {
                app: app.handle().clone(),
            });
            let handle = app.handle().clone();
            let desk = Desk::new(home, &data, ui).with_change_hook(move |root| {
                let _ = handle.emit("lode://changed", root);
            });
            app.manage(desk);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pending_link,
            app_version,
            recent_workspaces,
            forget_workspace,
            signed_in,
            sign_in,
            open_workspace,
            checkout,
            status,
            add,
            revert,
            commit,
            log,
            revision,
            blame,
            diff,
            diff_revision,
            commit_changelist,
            changelists,
            changelist_new,
            changelist_move,
            changelist_delete,
            lock,
            unlock,
            locks,
            shelve,
            shelves,
            shelf,
            shelf_submit,
            shelf_delete,
            unshelve,
            checks,
            open_link,
            image_side,
            branches,
            branch_create,
            branch_archive,
            switch,
            update,
            commit_all,
            merge,
            abandon_merge,
            conflict,
            resolve,
            resolve_with,
            view,
            view_tree,
            view_apply,
            watch,
            unwatch,
        ])
        .run(tauri::generate_context!())
}
