//! Lode workspace: local state in `.lode/` and change detection (ARCHITECTURE §10).
//!
//! - [`Workspace`] creates, finds and opens a workspace and hands out its parts.
//! - [`WorkspaceDb`] is `.lode/workspace.db`: config, file rows, conflicts, changelists.
//! - [`WorkspaceLock`] keeps a second mutating `lode` process out ("workspace is in use").
//! - [`scan()`] reports the status of every path, honouring `.lodeignore` and the view.
//! - [`BlobCache`] holds base content for diff and merge, size-capped with LRU eviction.
//!
//! Nothing here talks to a server.

#![forbid(unsafe_code)]

pub mod cache;
pub mod classify;
pub mod db;
mod error;
mod ignore_rules;
pub mod layout;
mod lock;
pub mod paths;
pub mod scan;
mod view;

use std::path::{Path, PathBuf};

pub use cache::{BlobCache, DEFAULT_CACHE_CAP, DEFAULT_SHARED_CACHE_CAP};
pub use classify::FileClass;
pub use db::{ChangelistRow, ConflictRow, DbTx, FileRow, FileState, WorkspaceDb};
pub use error::{Result, WorkspaceError};
pub use layout::Layout;
pub use lock::WorkspaceLock;
pub use scan::{scan, DiskInfo, InvalidName, ScanEntry, ScanOptions, ScanReport, Status};
pub use view::{View, ViewSpec};

use crate::db::keys;
use crate::error::IoContext;

/// What a new workspace is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInit {
    /// Server URL.
    pub server_url: String,
    /// Depot name.
    pub depot: String,
    /// Branch to follow.
    pub branch: String,
    /// Initial view.
    pub view: ViewSpec,
}

/// An open workspace.
#[derive(Debug)]
pub struct Workspace {
    layout: Layout,
    db: WorkspaceDb,
}

impl Workspace {
    /// Creates a workspace at `root` (which may exist and hold files) and records `init`.
    /// Refuses if `root` already is one.
    pub fn create(root: &Path, init: &WorkspaceInit) -> Result<Self> {
        std::fs::create_dir_all(root).at(root)?;
        let root = paths::canonical(root).at(root)?;
        let layout = Layout::new(&root);
        if layout.db().exists() {
            return Err(WorkspaceError::AlreadyAWorkspace(root));
        }
        layout.create_dirs()?;
        let db = WorkspaceDb::open(&layout.db())?;
        let mut ws = Self { layout, db };
        ws.initialize(init)?;
        Ok(ws)
    }

    /// Writes the configuration of a new workspace: what [`Workspace::create`] does after
    /// creating the database. A checkout stopped between the two finishes with this.
    pub fn initialize(&mut self, init: &WorkspaceInit) -> Result<()> {
        let view = serde_json::to_string(&init.view)
            .map_err(|e| WorkspaceError::Corrupt(e.to_string()))?;
        self.db.write(|tx| {
            tx.set_config(keys::SERVER_URL, &init.server_url)?;
            tx.set_config(keys::DEPOT, &init.depot)?;
            tx.set_config(keys::BRANCH, &init.branch)?;
            tx.set_config(keys::WORKSPACE_ID, &uuid::Uuid::new_v4().to_string())?;
            tx.set_config(keys::VIEW, &view)
        })
    }

    /// Opens the workspace whose root is `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let layout = Layout::new(root);
        if !layout.db().is_file() {
            return Err(WorkspaceError::NotAWorkspace(root.to_path_buf()));
        }
        // Old crashed downloads are not resumed from `tmp/`; they are only space.
        let db = WorkspaceDb::open(&layout.db())?;
        Ok(Self { layout, db })
    }

    /// Opens the workspace containing `start` (a directory), searching upwards.
    pub fn discover(start: &Path) -> Result<Self> {
        let start_abs = paths::canonical(start).at(start)?;
        let root = layout::discover(&start_abs)
            .ok_or_else(|| WorkspaceError::NotAWorkspace(start.to_path_buf()))?;
        Self::open(&root)
    }

    /// Takes the workspace lock; fails at once if another process holds it.
    pub fn lock(&self) -> Result<WorkspaceLock> {
        WorkspaceLock::acquire(&self.layout.lock())
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        self.layout.root()
    }

    /// Paths inside `.lode/`.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The database.
    pub fn db(&self) -> &WorkspaceDb {
        &self.db
    }

    /// The database, for writing.
    pub fn db_mut(&mut self) -> &mut WorkspaceDb {
        &mut self.db
    }

    /// The stored view.
    pub fn view_spec(&self) -> Result<ViewSpec> {
        match self.db.config(keys::VIEW)? {
            None => Ok(ViewSpec::default()),
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| WorkspaceError::Corrupt(format!("view: {e}"))),
        }
    }

    /// Replaces the stored view.
    pub fn set_view_spec(&mut self, view: &ViewSpec) -> Result<()> {
        let json =
            serde_json::to_string(view).map_err(|e| WorkspaceError::Corrupt(e.to_string()))?;
        self.db.write(|tx| tx.set_config(keys::VIEW, &json))
    }

    /// The local blob cache, capped at `cap` bytes.
    pub fn cache(&self, cap: u64) -> BlobCache {
        BlobCache::new(self.layout.cache(), cap)
    }

    /// Scans the workspace with its stored view.
    pub fn scan(&mut self, opts: &ScanOptions) -> Result<ScanReport> {
        let view = self.view_spec()?.compile()?;
        let root: PathBuf = self.layout.root().to_path_buf();
        scan::scan(&root, &mut self.db, &view, opts)
    }

    /// The local path of a depot path.
    pub fn local_path(&self, path: &lode_core::DepotPath) -> PathBuf {
        paths::to_local(self.root(), path)
    }
}
