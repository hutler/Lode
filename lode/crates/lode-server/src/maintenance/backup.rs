//! Online backup of a whole data directory (ARCHITECTURE §11).
//!
//! The database is copied first with SQLite's online backup, then the object store. Objects
//! are immutable and a revision's objects are stored before the revision is committed, so
//! every object the copied database references is already on disk when the copy of
//! `depots/` starts. Revisions are never removed, so garbage collection cannot delete those
//! objects either.
//!
//! With S3 storage (`[storage] kind = "s3"`) the content is not copied: it stays in the
//! bucket, which the bucket's own versioning or replication protects. The backup then holds
//! the database and the configuration.
//!
//! With the metadata in Postgres (`[database] kind = "postgres"`) the database is not copied
//! either: the backup holds the configuration, the objects, and `DATABASE-BACKUP.txt`, which
//! says to back the database up with `pg_dump` (or the provider's snapshots). Take the dump
//! before copying the objects, for the same reason as above.

use std::io::{self, Write};
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
    /// The content is in an S3 bucket and was not copied.
    pub content_in_bucket: bool,
    /// The metadata is in Postgres and was not copied; back it up with `pg_dump`.
    pub database_external: bool,
}

/// The note a backup of a Postgres-backed server holds instead of the database.
pub const DATABASE_NOTE_FILE: &str = "DATABASE-BACKUP.txt";

const DATABASE_NOTE: &str = "\
This server keeps its metadata (history, users, access rules, module data) in Postgres, so
this backup does not contain it. Back the database up with Postgres's own tools, before the
objects are copied, for example:

    pg_dump --format=custom --file=lode-meta.dump \"$(head -n1 database-url.txt)\"

and restore it with pg_restore into an empty database. loded.toml in this directory names the
database in [database].
";

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
    let database_external = state.meta().dialect() == lode_meta::Dialect::Postgres;
    if database_external {
        std::fs::write(dest.join(DATABASE_NOTE_FILE), DATABASE_NOTE)
            .map_err(io_err("cannot write the database note"))?;
    } else {
        state.meta().backup_to(dest.join(META_DB_FILE)).await?;
    }

    let src_root = state.config().data_dir.clone();
    let config = state.config().source.clone();
    let dest_root = dest.to_owned();
    let objects = state.config().storage.s3_in_use().is_none();
    let mut report = tokio::task::spawn_blocking(move || {
        copy_store(&src_root, &dest_root, objects, config.as_ref())
    })
    .await
    .map_err(|e| BackupError::Io {
        context: "copy task failed".into(),
        source: io::Error::other(e),
    })??;
    report.database_external = database_external;
    Ok(report)
}

/// A restored server must open the database and objects beside its backup configuration.
fn write_backup_config(text: &str, dest: &Path) -> Result<(), BackupError> {
    let mut table = text.parse::<toml::Table>().map_err(|_| {
        io_err("cannot parse loaded configuration")(io::ErrorKind::InvalidData.into())
    })?;
    let rewritten;
    let text = if table.get("data_dir").and_then(toml::Value::as_str) == Some(".") {
        text
    } else {
        table.insert("data_dir".into(), toml::Value::String(".".into()));
        rewritten = toml::to_string(&table).map_err(|_| {
            io_err("cannot serialize backup configuration")(io::ErrorKind::InvalidData.into())
        })?;
        &rewritten
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(dest)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(io_err("cannot write backup configuration"))
}

fn copy_store(
    src_root: &Path,
    dest_root: &Path,
    objects: bool,
    source: Option<&crate::config::ConfigSource>,
) -> Result<BackupReport, BackupError> {
    let mut report = BackupReport::default();
    let config = src_root.join(CONFIG_FILE_NAME);
    if let Some(source) = source {
        write_backup_config(&source.0, &dest_root.join(CONFIG_FILE_NAME))?;
    } else if config.is_file() {
        std::fs::copy(&config, dest_root.join(CONFIG_FILE_NAME))
            .map_err(io_err("cannot copy the configuration"))?;
    }
    if !objects {
        report.content_in_bucket = true;
        return Ok(report);
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
