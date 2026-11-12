//! Workspace errors.

use std::io;
use std::path::PathBuf;

/// Everything workspace operations fail with.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// A file system operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The workspace database failed.
    #[error("workspace database: {0}")]
    Db(#[from] rusqlite::Error),
    /// The database was written by a newer `lode`.
    #[error("workspace database has schema version {found}; this lode supports up to {supported}; upgrade lode")]
    SchemaTooNew {
        /// Version in the file.
        found: u32,
        /// Newest version this build knows.
        supported: u32,
    },
    /// Another `lode` process is changing this workspace.
    #[error("workspace is in use by another lode process; wait for it to finish")]
    InUse,
    /// No `.lode` directory here or above.
    #[error("not in a lode workspace (no .lode directory in {0} or above it)")]
    NotAWorkspace(PathBuf),
    /// `create` found a workspace already there.
    #[error("{0} is already a lode workspace")]
    AlreadyAWorkspace(PathBuf),
    /// A local path is outside the workspace root.
    #[error("{0} is outside the workspace")]
    OutsideWorkspace(PathBuf),
    /// A local file name is not a valid depot path (SPEC §5.7).
    #[error("{local}: not a valid depot path: {reason}")]
    InvalidName {
        /// The local path.
        local: PathBuf,
        /// Which rule it breaks.
        reason: String,
    },
    /// A view or ignore pattern does not compile.
    #[error(transparent)]
    Glob(#[from] lode_core::GlobError),
    /// A stored value is not what the schema allows.
    #[error("workspace database holds a bad value: {0}")]
    Corrupt(String),
}

impl WorkspaceError {
    /// An I/O error about `path`.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Shorthand for workspace results.
pub type Result<T, E = WorkspaceError> = std::result::Result<T, E>;

/// Adds the path to an I/O result.
pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| WorkspaceError::io(path, e))
    }
}
