//! Lode Desk, the Lode desktop client.
//!
//! The window is a React app (`ui/`). Each Tauri command here runs one `lode-ops` operation
//! through [`desk::Desk`] and returns the same JSON document `lode <command> --json` prints;
//! progress and warnings arrive as `lode://progress` and `lode://warning` events, and a
//! watched workspace whose files changed as `lode://changed` with its root. Operations
//! open the workspace only while they run, so `lode` and editor plugins work next to the app.

#![forbid(unsafe_code)]

pub mod desk;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;

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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
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
            watch,
            unwatch,
        ])
        .run(tauri::generate_context!())
}
