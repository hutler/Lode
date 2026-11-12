//! The `.lode/` directory (ARCHITECTURE §10.1) and finding it.

use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result};

/// Name of the state directory at the workspace root.
pub const DOT_LODE: &str = ".lode";
/// The workspace database.
pub const DB_FILE: &str = "workspace.db";
/// The lock file of mutating commands.
pub const LOCK_FILE: &str = "lock";
/// Blob cache for diff and merge bases.
pub const CACHE_DIR: &str = "cache";
/// Downloads in progress; on the same file system as the files, so a rename is atomic.
pub const TMP_DIR: &str = "tmp";
/// Name of ignore files (gitignore syntax), at the root or in any directory.
pub const IGNORE_FILE: &str = ".lodeignore";

/// Paths inside one workspace's `.lode/`.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout of the workspace at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.lode/`.
    pub fn dot_lode(&self) -> PathBuf {
        self.root.join(DOT_LODE)
    }

    /// `.lode/workspace.db`.
    pub fn db(&self) -> PathBuf {
        self.dot_lode().join(DB_FILE)
    }

    /// `.lode/lock`.
    pub fn lock(&self) -> PathBuf {
        self.dot_lode().join(LOCK_FILE)
    }

    /// `.lode/cache/`.
    pub fn cache(&self) -> PathBuf {
        self.dot_lode().join(CACHE_DIR)
    }

    /// `.lode/tmp/`.
    pub fn tmp(&self) -> PathBuf {
        self.dot_lode().join(TMP_DIR)
    }

    /// Creates the directories.
    pub fn create_dirs(&self) -> Result<()> {
        for d in [self.dot_lode(), self.cache(), self.tmp()] {
            std::fs::create_dir_all(&d).at(&d)?;
        }
        Ok(())
    }
}

/// The workspace root containing `start`: the nearest directory at or above it with a
/// `.lode/workspace.db`.
pub fn discover(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(DOT_LODE).join(DB_FILE).is_file())
        .map(Path::to_path_buf)
}
