//! What the app does, without Tauri: each method runs one `lode-ops` operation and returns
//! its JSON document. The Tauri commands in `lib.rs` are thin wrappers around these, so the
//! tests drive this type directly against a test server.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lode_ops::cmd::{checkout, info, login, status};
use lode_ops::context::{Ctx, Globals};
use lode_ops::error::{OpError, OpResult};
use lode_ops::output::Ui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
pub async fn run_op<F, Fut>(f: F) -> Result<Value, ErrorView>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = OpResult<Value>>,
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
        }
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
