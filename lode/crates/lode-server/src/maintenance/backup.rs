//! Online backup of a whole data directory (ARCHITECTURE §11).
//!
//! The database is copied first with SQLite's online backup, then the object store. Objects
//! are immutable and a revision's objects are stored before the revision is committed, so
//! every object the copied database references is already on disk when the copy of
//! `depots/` starts. Revisions are never removed, so garbage collection cannot delete those
//! objects either.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::CONFIG_FILE_NAME;
use crate::server::META_DB_FILE;
use crate::state::AppState;

/// What a backup copied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    /// Object files copied.
    pub objects: u64,
    /// Bytes of object files copied.
    pub bytes: u64,
}

/// Why a backup failed.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The destination exists and is not empty.
    #[error("{0} exists and is not empty")]
    NotEmpty(PathBuf),
    /// Copying the database failed.
    #[error("database backup failed: {0}")]
    Meta(#[from] lode_meta::MetaError),
    /// Copying files failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        source: io::Error,
    },
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> BackupError {
    let context = context.into();
    move |source| BackupError::Io { context, source }
}

/// Copies the data directory of `state` into `dest`, which must be missing or empty. The
/// result is a data directory `loded` can open.
pub async fn backup(state: &AppState, dest: &Path) -> Result<BackupReport, BackupError> {
    if dest.exists() {
        let mut rd =
            std::fs::read_dir(dest).map_err(io_err(format!("cannot read {}", dest.display())))?;
        if rd.next().is_some() {
            return Err(BackupError::NotEmpty(dest.to_owned()));
        }
    }
    std::fs::create_dir_all(dest).map_err(io_err(format!("cannot create {}", dest.display())))?;
    state.meta().backup_to(dest.join(META_DB_FILE)).await?;

    let src_root = state.config().data_dir.clone();
    let dest_root = dest.to_owned();
    tokio::task::spawn_blocking(move || copy_store(&src_root, &dest_root))
        .await
        .map_err(|e| BackupError::Io {
            context: "copy task failed".into(),
            source: io::Error::other(e),
        })?
}

fn copy_store(src_root: &Path, dest_root: &Path) -> Result<BackupReport, BackupError> {
    let mut report = BackupReport::default();
    let config = src_root.join(CONFIG_FILE_NAME);
    if config.is_file() {
        std::fs::copy(&config, dest_root.join(CONFIG_FILE_NAME))
            .map_err(io_err("cannot copy the configuration"))?;
    }
    let depots = src_root.join("depots");
    std::fs::create_dir_all(dest_root.join("depots")).map_err(io_err("cannot create depots/"))?;
    let Ok(rd) = std::fs::read_dir(&depots) else {
        return Ok(report);
    };
    for depot in rd.flatten() {
        let objects = depot.path().join("objects");
        let target = dest_root
            .join("depots")
            .join(depot.file_name())
            .join("objects");
        std::fs::create_dir_all(&target).map_err(io_err("cannot create a depot directory"))?;
        copy_tree(&objects, &target, &mut report)?;
    }
    Ok(report)
}

/// Copies a directory tree of object files. Temporary upload files live outside `objects/`
/// and are not copied.
fn copy_tree(src: &Path, dest: &Path, report: &mut BackupReport) -> Result<(), BackupError> {
    let rd = match std::fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(format!("cannot read {}", src.display()))(e)),
    };
    for entry in rd {
        let entry = entry.map_err(io_err(format!("cannot read {}", src.display())))?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        let ft = entry.file_type().map_err(io_err("cannot stat"))?;
        if ft.is_dir() {
            std::fs::create_dir_all(&target).map_err(io_err("cannot create a directory"))?;
            copy_tree(&path, &target, report)?;
        } else if ft.is_file() {
            match std::fs::copy(&path, &target) {
                Ok(n) => {
                    report.objects += 1;
                    report.bytes += n;
                }
                // Deleted by garbage collection after the listing: it was unreferenced.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(format!("cannot copy {}", path.display()))(e)),
            }
        }
    }
    Ok(())
}
