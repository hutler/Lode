//! What the app does, without Tauri: each method runs one `lode-ops` operation and returns
//! its JSON document. The Tauri commands in `lib.rs` are thin wrappers around these, so the
//! tests drive this type directly against a test server.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lode_ops::cmd::{
    branch, changelist, checkout, checks, commit, conflict, diff, files, history, info, locks,
    login, merge, revert, shelve, status, update, view,
};
use lode_ops::context::{Ctx, Globals};
use lode_ops::error::{OpError, OpResult};
use lode_ops::output::Ui;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::watch::{Watcher, QUIET};

/// An operation's error as the UI receives it: the same fields as `lode --json`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ErrorView {
    /// Stable code, such as `unauthenticated` or `not_a_workspace`.
    pub code: String,
    /// What happened and what to do.
    pub message: String,
    /// Structured data, such as affected paths.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

impl From<OpError> for ErrorView {
    fn from(e: OpError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            details: e.details,
        }
    }
}

/// A workspace opened before, for the welcome page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentWorkspace {
    /// Workspace root.
    pub path: String,
    /// Depot name.
    pub depot: String,
    /// Server URL.
    pub server: String,
    /// Branch when last opened.
    pub branch: String,
}

/// What to shelve, as the window sends it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShelveRequest {
    /// Depot paths; all changes when empty (and no changelist).
    pub paths: Vec<String>,
    /// Only the files of this changelist.
    pub changelist: Option<String>,
    /// Description; the changelist's or the updated shelf's when None.
    pub message: Option<String>,
    /// Leave the changes in the workspace.
    pub keep: bool,
    /// Replace the content of this open shelf of mine.
    pub update: Option<u64>,
}

/// Images larger than this are not shown in an image diff (64 MB).
pub const IMAGE_LIMIT: u64 = 64 * 1024 * 1024;

/// Which side of a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Before the change.
    Old,
    /// After the change.
    New,
}

/// One side of an image diff as the window receives it, raw rather than JSON so large images
/// stay cheap: one byte (0 no file, 1 too large to show, 2 content follows), the size as eight
/// little-endian bytes, then the content.
pub fn encode_side(side: Option<diff::Content>) -> Vec<u8> {
    match side {
        None => {
            let mut out = vec![0];
            out.extend_from_slice(&0u64.to_le_bytes());
            out
        }
        Some(c) => {
            let mut out = Vec::with_capacity(9 + c.bytes.as_ref().map_or(0, Vec::len));
            out.push(if c.bytes.is_some() { 2 } else { 1 });
            out.extend_from_slice(&c.size.to_le_bytes());
            if let Some(b) = c.bytes {
                out.extend_from_slice(&b);
            }
            out
        }
    }
}

/// Text sides of a conflict larger than this are not sent to the window (8 MB).
pub const CONFLICT_LIMIT: u64 = 8 * 1024 * 1024;

/// How to resolve a conflict, as `lode resolve` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Using {
    /// The local side.
    Mine,
    /// The incoming side.
    Theirs,
    /// The file as it is now.
    Merged,
}

/// One side of a conflict for the window: its size, and its text for a text merge.
fn side_json(side: Option<diff::Content>, text: bool) -> Value {
    match side {
        None => Value::Null,
        Some(c) => serde_json::json!({
            "size": c.size,
            "text": c.bytes.filter(|_| text).map(|b| String::from_utf8_lossy(&b).into_owned()),
        }),
    }
}

/// A conflict's sides as the window receives them.
pub fn conflict_json(s: conflict::ConflictSides) -> Value {
    serde_json::json!({
        "path": s.path,
        "theirs_rev": s.theirs_rev,
        "text": s.text,
        "base": side_json(s.base, s.text),
        "mine": side_json(s.mine, s.text),
        "theirs": side_json(s.theirs, s.text),
        "result": side_json(s.result, s.text),
        "theirs_file": s.theirs_file,
    })
}

/// How many workspaces the welcome page remembers.
pub const RECENT_MAX: usize = 20;

/// The app's state.
pub struct Desk {
    /// Where relative paths and sign-in checks start (the user's home directory).
    home: PathBuf,
    /// Overrides the lode config directory (tests); `None` uses the same one as `lode`.
    config_dir: Option<PathBuf>,
    /// `recent.json` in the app's own data directory.
    recent_file: PathBuf,
    /// Serializes writes to `recent_file`.
    recent_lock: Mutex<()>,
    ui: Arc<dyn Ui>,
    /// Called with a workspace root when files in it changed.
    on_change: Arc<dyn Fn(String) + Send + Sync>,
    /// Running watches by workspace root.
    watches: Mutex<HashMap<String, Watcher>>,
}

impl std::fmt::Debug for Desk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desk")
            .field("home", &self.home)
            .field("config_dir", &self.config_dir)
            .field("recent_file", &self.recent_file)
            .finish()
    }
}

/// Runs an operation on a thread of its own with a single-threaded runtime. Operations hold
/// the workspace database across awaits, which cannot move between threads, so they never
/// run on the app's shared runtime.
pub async fn run_op<F, Fut, T>(f: F) -> Result<T, ErrorView>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = OpResult<T>>,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("lode-op".into())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(f()),
                Err(e) => Err(OpError::from(e)),
            };
            let _ = tx.send(result);
        });
    if let Err(e) = spawned {
        return Err(OpError::from(e).into());
    }
    match rx.await {
        Ok(r) => r.map_err(ErrorView::from),
        Err(_) => Err(ErrorView {
            code: "internal".into(),
            message: "the operation stopped without an answer".into(),
            details: Value::Null,
        }),
    }
}

impl Desk {
    /// The app state. `data_dir` holds the app's own files.
    pub fn new(home: PathBuf, data_dir: &Path, ui: Arc<dyn Ui>) -> Self {
        Self {
            home,
            config_dir: None,
            recent_file: data_dir.join("recent.json"),
            recent_lock: Mutex::new(()),
            ui,
            on_change: Arc::new(|_| {}),
            watches: Mutex::new(HashMap::new()),
        }
    }

    /// Calls `hook` with a workspace root after files in a watched workspace changed.
    pub fn with_change_hook(mut self, hook: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.on_change = Arc::new(hook);
        self
    }

    /// Uses `dir` as the lode config directory instead of the user's (tests).
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = Some(dir);
        self
    }

    fn ctx(&self, dir: &Path, server: Option<String>) -> OpResult<Ctx> {
        let g = Globals {
            server,
            ..Globals::default()
        };
        let ctx = Ctx::in_dir(g, dir, Arc::clone(&self.ui))?;
        Ok(match &self.config_dir {
            Some(c) => ctx.with_config_dir(c),
            None => ctx,
        })
    }

    /// Who is signed in to `server`, or null when nobody is (or the token was revoked).
    pub async fn signed_in(&self, server: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(&self.home, Some(server))?;
        let result = run_op(move || async move { login::whoami(&ctx).await }).await;
        match result {
            Ok(v) => Ok(v),
            Err(e) if e.code == "unauthenticated" => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }

    /// Signs in with a name and password and stores the token where `lode` keeps it.
    pub async fn sign_in(
        &self,
        server: String,
        name: String,
        password: String,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(&self.home, None)?;
        run_op(move || async move {
            login::login(
                &ctx,
                login::LoginArgs {
                    url: Some(server),
                    name: Some(name),
                    password: Some(password),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// The single sign-on `server` offers, or null.
    pub async fn sso_offer(&self, server: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(&self.home, None)?;
        run_op(move || async move { login::sso_offer(&ctx, &server).await }).await
    }

    /// Signs in through the company's sign-in page in the browser and stores the token
    /// where `lode` keeps it.
    pub async fn sign_in_sso(&self, server: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(&self.home, None)?;
        run_op(move || async move {
            login::login(
                &ctx,
                login::LoginArgs {
                    url: Some(server),
                    sso: true,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// Opens the workspace at `path` and remembers it.
    pub async fn open_workspace(&self, path: String) -> Result<Value, ErrorView> {
        let dir = PathBuf::from(&path);
        if !dir.is_dir() {
            return Err(ErrorView {
                code: "not_found".into(),
                message: format!("{path} is not a folder"),
                details: Value::Null,
            });
        }
        let ctx = self.ctx(&dir, None)?;
        let v = run_op(move || async move { info::info(&ctx).await }).await?;
        self.remember(&v)?;
        Ok(v)
    }

    /// Checks out `target` (`<server>/<depot>`) into `dir` and remembers the new workspace.
    pub async fn checkout(
        &self,
        target: String,
        dir: String,
        branch: String,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(&self.home, None)?;
        let v = run_op(move || async move {
            checkout::checkout(
                &ctx,
                checkout::CheckoutArgs {
                    target,
                    dir: Some(PathBuf::from(dir)),
                    branch,
                    ..Default::default()
                },
            )
            .await
        })
        .await?;
        if let Some(root) = v["workspace"].as_str() {
            self.open_workspace(root.to_owned()).await?;
        }
        Ok(v)
    }

    /// `lode status` in `workspace`.
    pub async fn status(&self, workspace: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { status::status(&ctx, status::StatusArgs::default()).await })
            .await
    }

    /// Starts `lode add` of `paths` (depot paths) in `workspace`.
    pub async fn add(&self, workspace: String, paths: Vec<String>) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            files::add(
                &ctx,
                files::AddArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// `lode revert` of `paths` (depot paths) in `workspace`.
    pub async fn revert(&self, workspace: String, paths: Vec<String>) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            revert::revert(
                &ctx,
                revert::RevertArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// `lode commit -m message paths…` in `workspace`. Untracked files among `paths` must be
    /// added first.
    pub async fn commit(
        &self,
        workspace: String,
        paths: Vec<String>,
        message: String,
    ) -> Result<Value, ErrorView> {
        if paths.is_empty() {
            return Err(ErrorView {
                code: "usage".into(),
                message: "pick the files to commit".into(),
                details: Value::Null,
            });
        }
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            commit::commit(
                &ctx,
                commit::CommitArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    message: Some(message),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// One page of `lode log` in `workspace`: up to `max` revisions older than `before`
    /// (newest first when `before` is none), of the branch or, with `path`, of one file
    /// followed through renames.
    pub async fn log(
        &self,
        workspace: String,
        path: Option<String>,
        before: Option<u64>,
        max: u32,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            history::log(
                &ctx,
                history::LogArgs {
                    follow: path.is_some(),
                    path,
                    before,
                    max: max.clamp(1, 1000),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// One revision with its changed files (`lode log -v` of that revision alone).
    pub async fn revision(&self, workspace: String, rev: u64) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        let v = run_op(move || async move {
            history::log(
                &ctx,
                history::LogArgs {
                    before: Some(rev.saturating_add(1)),
                    max: 1,
                    verbose: true,
                    ..Default::default()
                },
            )
            .await
        })
        .await?;
        match v["revisions"].get(0) {
            Some(r) if r["rev"].as_u64() == Some(rev) => Ok(r.clone()),
            _ => Err(ErrorView {
                code: "not_found".into(),
                message: format!("r{rev} is not in this branch's history"),
                details: Value::Null,
            }),
        }
    }

    /// `lode blame path[@rev]` in `workspace`.
    pub async fn blame(
        &self,
        workspace: String,
        path: String,
        rev: Option<u64>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        let spec = match rev {
            Some(r) => format!("{path}@{r}"),
            None => path,
        };
        run_op(move || async move {
            history::blame(
                &ctx,
                history::BlameArgs {
                    path: spec,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// `lode diff` of one working file (a depot path) against its base: the file's entry of
    /// `lode diff --json`, or null when the file has no change.
    pub async fn diff(&self, workspace: String, path: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        let v = run_op(move || async move {
            diff::diff(
                &ctx,
                diff::DiffArgs {
                    paths: vec![PathBuf::from(path)],
                    ..Default::default()
                },
            )
            .await
        })
        .await?;
        Ok(v["files"].get(0).cloned().unwrap_or(Value::Null))
    }

    /// The diff of `path` as revision `rev` changed it: against the parent revision, or
    /// against the source of a copy or move, as an entry of `lode diff --json`.
    pub async fn diff_revision(
        &self,
        workspace: String,
        rev: u64,
        path: String,
    ) -> Result<Value, ErrorView> {
        let (old, new) = self.revision_sides(workspace.clone(), rev, &path).await?;
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            diff::diff_versions(
                &ctx,
                diff::VersionsArgs {
                    old,
                    new,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// The two versions (`path@rev`) that revision `rev` changed `path` between: the parent
    /// revision's, or the source of a copy or move, and its own. None on a side that has no
    /// file (an add, or a delete).
    async fn revision_sides(
        &self,
        workspace: String,
        rev: u64,
        path: &str,
    ) -> Result<(Option<String>, Option<String>), ErrorView> {
        let r = self.revision(workspace, rev).await?;
        let not_found = || ErrorView {
            code: "not_found".into(),
            message: format!("r{rev} did not change {path}"),
            details: Value::Null,
        };
        let change = r["changes"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["path"].as_str() == Some(path)))
            .ok_or_else(not_found)?;
        let parent = r["parent"].as_u64();
        let copy = change["copy_from"].as_object().and_then(|c| {
            Some(format!(
                "{}@{}",
                c.get("path")?.as_str()?,
                c.get("rev")?.as_u64()?
            ))
        });
        let at = |rev: u64| format!("{path}@{rev}");
        Ok(match change["action"].as_str() {
            Some("add") => (copy, Some(at(rev))),
            Some("delete") => (parent.map(at), None),
            _ => (copy.or(parent.map(at)), Some(at(rev))),
        })
    }

    /// One side of an image diff: a working file (`rev` None) against its base, or a file as
    /// revision `rev` changed it. None when that side has no file; no bytes when it is over
    /// [`IMAGE_LIMIT`].
    pub async fn image_side(
        &self,
        workspace: String,
        path: String,
        rev: Option<u64>,
        side: Side,
    ) -> Result<Option<diff::Content>, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        match rev {
            None => {
                run_op(move || async move {
                    match side {
                        Side::Old => diff::base_content(&ctx, &path, IMAGE_LIMIT).await,
                        Side::New => diff::working_content(&ctx, &path, IMAGE_LIMIT),
                    }
                })
                .await
            }
            Some(rev) => {
                let (old, new) = self.revision_sides(workspace, rev, &path).await?;
                let Some(spec) = (match side {
                    Side::Old => old,
                    Side::New => new,
                }) else {
                    return Ok(None);
                };
                run_op(move || async move {
                    diff::stored_content(&ctx, &history::Source::default(), &spec, IMAGE_LIMIT)
                        .await
                        .map(Some)
                })
                .await
            }
        }
    }

    /// `lode commit -c NAME`: exactly the files of one changelist (`default` for files in
    /// none), with the changelist's description when `message` is None.
    pub async fn commit_changelist(
        &self,
        workspace: String,
        name: String,
        message: Option<String>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            commit::commit(
                &ctx,
                commit::CommitArgs {
                    changelist: Some(name),
                    message: message.filter(|m| !m.trim().is_empty()),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// `lode changelist list|new|move|delete` in `workspace`.
    async fn changelist(
        &self,
        workspace: String,
        command: changelist::ChangelistCommand,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            changelist::changelist(&ctx, changelist::ChangelistArgs { command }).await
        })
        .await
    }

    /// The workspace's changelists with their changed files, `default` first.
    pub async fn changelists(&self, workspace: String) -> Result<Value, ErrorView> {
        self.changelist(workspace, changelist::ChangelistCommand::List)
            .await
    }

    /// Creates a changelist.
    pub async fn changelist_new(
        &self,
        workspace: String,
        name: String,
        description: String,
    ) -> Result<Value, ErrorView> {
        self.changelist(
            workspace,
            changelist::ChangelistCommand::New { name, description },
        )
        .await
    }

    /// Puts files (depot paths) into a changelist; `default` takes them out of any.
    pub async fn changelist_move(
        &self,
        workspace: String,
        name: String,
        paths: Vec<String>,
    ) -> Result<Value, ErrorView> {
        let paths = paths.into_iter().map(PathBuf::from).collect();
        self.changelist(
            workspace,
            changelist::ChangelistCommand::Move { name, paths },
        )
        .await
    }

    /// Deletes a changelist; its files return to `default`.
    pub async fn changelist_delete(
        &self,
        workspace: String,
        name: String,
    ) -> Result<Value, ErrorView> {
        self.changelist(workspace, changelist::ChangelistCommand::Delete { name })
            .await
    }

    /// `lode lock` of `paths` (depot paths) in `workspace`, all or none.
    pub async fn lock(
        &self,
        workspace: String,
        paths: Vec<String>,
        comment: Option<String>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            locks::lock(
                &ctx,
                locks::LockArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    comment: comment.filter(|c| !c.trim().is_empty()),
                },
            )
            .await
        })
        .await
    }

    /// `lode unlock` of `paths`; `force` breaks other people's locks (depot admin).
    pub async fn unlock(
        &self,
        workspace: String,
        paths: Vec<String>,
        force: bool,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            locks::unlock(
                &ctx,
                locks::UnlockArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    force,
                },
            )
            .await
        })
        .await
    }

    /// `lode locks` of the workspace's branch; `mine` keeps only this workspace's.
    pub async fn locks(&self, workspace: String, mine: bool) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(
            move || async move { locks::locks(&ctx, locks::LocksArgs { path: None, mine }).await },
        )
        .await
    }

    /// `lode shelve`: the changes to `paths` (depot paths; all when empty), or to the files of
    /// `changelist`, onto a new shelf, or onto shelf `update` when given. `keep` leaves them in
    /// the workspace.
    pub async fn shelve(
        &self,
        workspace: String,
        request: ShelveRequest,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            shelve::shelve(
                &ctx,
                shelve::ShelveArgs {
                    paths: request.paths.into_iter().map(PathBuf::from).collect(),
                    message: request.message.filter(|m| !m.trim().is_empty()),
                    changelist: request.changelist,
                    keep: request.keep,
                    update: request.update,
                },
            )
            .await
        })
        .await
    }

    /// `lode shelves` of the depot: `mine` keeps the signed-in user's; `state` is `open`
    /// (the default), `submitted` or `deleted`.
    pub async fn shelves(
        &self,
        workspace: String,
        mine: bool,
        state: Option<String>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            shelve::shelves(
                &ctx,
                shelve::ShelvesArgs {
                    mine,
                    state,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// One shelf with its changes and each file's diff against the shelf's base.
    pub async fn shelf(&self, workspace: String, id: u64) -> Result<Value, ErrorView> {
        self.shelf_command(workspace, shelve::ShelfCommand::Diff { id })
            .await
    }

    /// Submits version `version` of shelf `id`; a shelf changed since is refused (`conflict`).
    pub async fn shelf_submit(
        &self,
        workspace: String,
        id: u64,
        version: u64,
        message: Option<String>,
    ) -> Result<Value, ErrorView> {
        let spec = format!("{id}@{version}");
        self.shelf_command(workspace, shelve::ShelfCommand::Submit { spec, message })
            .await
    }

    /// Deletes shelf `id`.
    pub async fn shelf_delete(&self, workspace: String, id: u64) -> Result<Value, ErrorView> {
        self.shelf_command(workspace, shelve::ShelfCommand::Delete { id })
            .await
    }

    async fn shelf_command(
        &self,
        workspace: String,
        command: shelve::ShelfCommand,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { shelve::shelf(&ctx, shelve::ShelfArgs { command }).await })
            .await
    }

    /// `lode unshelve`: applies shelf `id` to the workspace, with each file's outcome.
    pub async fn unshelve(&self, workspace: String, id: u64) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { shelve::unshelve(&ctx, shelve::UnshelveArgs { id }).await })
            .await
    }

    /// `lode checks` of several targets (`rN` for a revision, `ID@VERSION` for a shelf), for
    /// the rows on screen: `{"available": bool, "checks": {target: [check, ...]}}`. A server
    /// without the checks module gives `available: false` rather than an error, and a target
    /// the server does not know is left out.
    pub async fn checks(
        &self,
        workspace: String,
        targets: Vec<String>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            let mut found = serde_json::Map::new();
            for target in targets {
                let args = checks::ChecksArgs {
                    target: target.clone(),
                    depot: None,
                };
                match checks::checks(&ctx, args).await {
                    Ok(mut v) => {
                        found.insert(target, v["checks"].take());
                    }
                    Err(e) if e.code == "module_disabled" => {
                        return Ok(json!({ "available": false, "checks": {} }));
                    }
                    Err(e) if e.code == "not_found" => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(json!({ "available": true, "checks": found }))
        })
        .await
    }

    /// The depot's branches (`all` adds archived ones), with the workspace's as `current`.
    pub async fn branches(&self, workspace: String, all: bool) -> Result<Value, ErrorView> {
        self.branch_command(workspace, branch::BranchCmd::List { all })
            .await
    }

    /// Creates a branch from `from` (`BRANCH` or `BRANCH@REV`; default: the workspace's branch
    /// at its revision), and switches the workspace to it when `switch`.
    pub async fn branch_create(
        &self,
        workspace: String,
        name: String,
        from: Option<String>,
        switch: bool,
    ) -> Result<Value, ErrorView> {
        let from = from.filter(|f| !f.trim().is_empty());
        self.branch_command(workspace, branch::BranchCmd::Create { name, from, switch })
            .await
    }

    /// Archives a branch: hidden from lists and closed to commits (depot admin).
    pub async fn branch_archive(
        &self,
        workspace: String,
        name: String,
    ) -> Result<Value, ErrorView> {
        self.branch_command(workspace, branch::BranchCmd::Archive { name })
            .await
    }

    async fn branch_command(
        &self,
        workspace: String,
        command: branch::BranchCmd,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { branch::branch(&ctx, branch::BranchArgs { command }).await })
            .await
    }

    /// `lode switch`: moves the workspace to `branch` (its head, or `rev`), carrying local
    /// changes over.
    pub async fn switch(
        &self,
        workspace: String,
        branch: String,
        rev: Option<u64>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(
            move || async move { branch::switch(&ctx, branch::SwitchArgs { branch, rev }).await },
        )
        .await
    }

    /// `lode update`: to the branch head, or `rev`; also finishes an update or switch that
    /// stopped.
    pub async fn update(&self, workspace: String, rev: Option<u64>) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { update::update(&ctx, update::UpdateArgs { rev }).await }).await
    }

    /// `lode commit` of every tracked change, as a pending merge must be committed.
    pub async fn commit_all(&self, workspace: String, message: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            commit::commit(
                &ctx,
                commit::CommitArgs {
                    message: Some(message),
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// `lode merge`: everything of `source` since the merge base, or its revision `rev` only.
    pub async fn merge(
        &self,
        workspace: String,
        source: String,
        rev: Option<u64>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { merge::merge(&ctx, merge::MergeArgs { source, rev }).await })
            .await
    }

    /// `lode revert --merge`: the next commit no longer records the pending merge; files stay.
    pub async fn abandon_merge(&self, workspace: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            revert::revert(
                &ctx,
                revert::RevertArgs {
                    merge: true,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// The base, mine, theirs and current content of a conflicted file (depot path).
    pub async fn conflict(&self, workspace: String, path: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        let sides =
            run_op(
                move || async move { conflict::conflict_sides(&ctx, &path, CONFLICT_LIMIT).await },
            )
            .await?;
        Ok(conflict_json(sides))
    }

    /// `lode resolve --mine|--theirs|--merged` of `paths` (depot paths).
    pub async fn resolve(
        &self,
        workspace: String,
        paths: Vec<String>,
        using: Using,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move {
            update::resolve(
                &ctx,
                update::ResolveArgs {
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                    mine: using == Using::Mine,
                    theirs: using == Using::Theirs,
                    merged: using == Using::Merged,
                },
            )
            .await
        })
        .await
    }

    /// Saves `content` as the resolved file `path` and marks it resolved.
    pub async fn resolve_with(
        &self,
        workspace: String,
        path: String,
        content: String,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { conflict::resolve_with(&ctx, &path, content.as_bytes()).await })
            .await
    }

    /// The workspace view: its patterns, what it holds on the server, and what is on disk.
    pub async fn view(&self, workspace: String) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { view::view_summary(&ctx).await }).await
    }

    /// One folder of the depot (`path` a depot path; the root when None) at the workspace's
    /// revision, whether or not it is in the view.
    pub async fn view_tree(
        &self,
        workspace: String,
        path: Option<String>,
    ) -> Result<Value, ErrorView> {
        let ctx = self.ctx(Path::new(&workspace), None)?;
        // Depot paths are relative to the workspace root, where the context starts.
        let path = Some(path.filter(|p| !p.is_empty()).unwrap_or_else(|| ".".into()));
        run_op(move || async move {
            history::ls(
                &ctx,
                history::LsArgs {
                    path,
                    ..Default::default()
                },
            )
            .await
        })
        .await
    }

    /// Replaces the view's patterns and syncs the files on disk; with `dry_run`, only says what
    /// would be fetched and removed. Files with local changes stop it unless `force`.
    pub async fn view_apply(
        &self,
        workspace: String,
        include: Vec<String>,
        exclude: Vec<String>,
        force: bool,
        dry_run: bool,
    ) -> Result<Value, ErrorView> {
        let clean = |v: Vec<String>| -> Vec<String> {
            v.into_iter()
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect()
        };
        let (include, exclude) = (clean(include), clean(exclude));
        let ctx = self.ctx(Path::new(&workspace), None)?;
        run_op(move || async move { view::set_view(&ctx, include, exclude, force, dry_run).await })
            .await
    }

    /// Watches `workspace` for changes until [`Desk::unwatch`]; a second call is a no-op.
    pub fn watch(&self, workspace: String) -> Result<(), ErrorView> {
        let mut watches = self.watches.lock().unwrap_or_else(|p| p.into_inner());
        if watches.contains_key(&workspace) {
            return Ok(());
        }
        let hook = Arc::clone(&self.on_change);
        let root = workspace.clone();
        let w = Watcher::new(Path::new(&workspace), QUIET, move || hook(root.clone())).map_err(
            |e| ErrorView {
                code: "watch_failed".into(),
                message: format!("cannot watch {workspace} for changes: {e}"),
                details: Value::Null,
            },
        )?;
        watches.insert(workspace, w);
        Ok(())
    }

    /// Stops watching `workspace`.
    pub fn unwatch(&self, workspace: &str) {
        let mut watches = self.watches.lock().unwrap_or_else(|p| p.into_inner());
        watches.remove(workspace);
    }

    /// The remembered workspaces, newest first.
    pub fn recent(&self) -> Vec<RecentWorkspace> {
        std::fs::read(&self.recent_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_recent(&self, list: &[RecentWorkspace]) -> Result<(), ErrorView> {
        let io = |e: std::io::Error| ErrorView::from(OpError::from(e));
        if let Some(dir) = self.recent_file.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = serde_json::to_vec_pretty(list).map_err(|e| ErrorView {
            code: "internal".into(),
            message: e.to_string(),
            details: Value::Null,
        })?;
        let tmp = self.recent_file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, &self.recent_file).map_err(io)
    }

    /// Puts the workspace described by `info` first in the recent list.
    fn remember(&self, info: &Value) -> Result<(), ErrorView> {
        let text = |k: &str| info[k].as_str().unwrap_or_default().to_owned();
        let entry = RecentWorkspace {
            path: text("root"),
            depot: text("depot"),
            server: text("server"),
            branch: text("branch"),
        };
        let _guard = self.recent_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut list = self.recent();
        list.retain(|r| r.path != entry.path);
        list.insert(0, entry);
        list.truncate(RECENT_MAX);
        self.save_recent(&list)
    }

    /// Removes `path` from the recent list (the folder itself is left alone).
    pub fn forget(&self, path: &str) -> Result<(), ErrorView> {
        let _guard = self.recent_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut list = self.recent();
        list.retain(|r| r.path != path);
        self.save_recent(&list)
    }
}

/// Whether `url` may be handed to the system browser: `http` or `https`, with no spaces or
/// control characters that a launcher could read as more than one argument.
pub fn is_web_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty())
        && url.len() <= 2048
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// Opens a web link (a check's details) in the system browser.
pub fn open_link(url: &str) -> Result<(), ErrorView> {
    if !is_web_link(url) {
        return Err(ErrorView {
            code: "usage".into(),
            message: format!("`{url}` is not a web link"),
            details: Value::Null,
        });
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    // Reaped on a thread of its own, so a launcher that exits leaves nothing behind.
    cmd.spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|e| ErrorView {
            code: "failed".into(),
            message: format!("could not open the browser: {e}"),
            details: Value::Null,
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{encode_side, is_web_link};
    use lode_ops::cmd::diff::Content;

    #[test]
    fn image_sides_are_tagged_with_their_size() {
        assert_eq!(encode_side(None), [0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let big = encode_side(Some(Content {
            size: 70_000_000,
            bytes: None,
        }));
        assert_eq!(big[0], 1);
        assert_eq!(
            u64::from_le_bytes(big[1..9].try_into().unwrap()),
            70_000_000
        );
        assert_eq!(big.len(), 9);
        let small = encode_side(Some(Content {
            size: 3,
            bytes: Some(vec![7, 8, 9]),
        }));
        assert_eq!(small, [2, 3, 0, 0, 0, 0, 0, 0, 0, 7, 8, 9]);
    }

    #[test]
    fn only_plain_web_links_are_opened() {
        assert!(is_web_link("https://ci.example.com/build/42"));
        assert!(is_web_link("HTTP://ci.example.com/?a=1&b=2"));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
            "https://a b",
            "https://a\"b",
            "https://a\nb",
            "calc.exe",
        ] {
            assert!(!is_web_link(bad), "{bad}");
        }
    }
}
