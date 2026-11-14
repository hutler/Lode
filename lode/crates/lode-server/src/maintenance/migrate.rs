//! `loded migrate-storage --to s3` (T123): copies a filesystem store into the S3 bucket that
//! `[storage]` names, then sets `kind = "s3"` in `loded.toml`.
//!
//! Objects are immutable and named by their id, so the copy can run while the server is up
//! (`copy_only`) and be repeated: each run lists what the bucket already holds and uploads
//! only the rest. The switch itself is meant for a stopped server, after a last run has
//! copied what arrived meanwhile. Local object files are left in place.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use futures::stream::{self, StreamExt, TryStreamExt};
use lode_core::{DepotId, ObjectId};
use lode_store::{BlobStore, FsBlobStore, FsBlobStoreConfig, PutOutcome, S3BlobStore, StoreError};

use crate::config::{ServerConfig, StorageKind, CONFIG_FILE_NAME};

/// Uploads in flight at once.
const PARALLEL: usize = 8;

/// Options for [`migrate_to_s3`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MigrateOptions {
    /// Copy, but leave `kind` as it is (for a first pass while the server runs).
    pub copy_only: bool,
}

/// What a migration did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrateReport {
    /// Depots looked at.
    pub depots: u64,
    /// Objects uploaded by this run.
    pub copied: u64,
    /// Bytes uploaded by this run (stored size).
    pub bytes: u64,
    /// Objects that were already in the bucket.
    pub already_there: u64,
    /// The configuration file, if this run switched it to S3.
    pub switched: Option<PathBuf>,
}

/// Why a migration stopped.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// `[storage]` has no S3 settings.
    #[error(
        "loded.toml has no S3 settings: add bucket (and the other S3 settings) to [storage], \
         keeping kind = \"fs\", then run this again"
    )]
    NotPrepared,
    /// The server already uses S3.
    #[error("the server already keeps its content in S3 ([storage] kind = \"s3\")")]
    AlreadyS3,
    /// The S3 settings cannot be used.
    #[error("{0}")]
    Settings(String),
    /// A store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// After the copy, the bucket still lacks objects.
    #[error(
        "{missing} object(s) of depot {depot} are not in the bucket after the copy; run this again"
    )]
    Incomplete {
        /// The depot.
        depot: i64,
        /// How many are missing.
        missing: u64,
    },
    /// Reading or writing local files failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Cause.
        source: io::Error,
    },
    /// The configuration file could not be switched.
    #[error("{0}")]
    Config(String),
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> MigrateError {
    let context = context.into();
    move |source| MigrateError::Io { context, source }
}

/// Copies every object of the filesystem store into the configured bucket and, unless
/// `copy_only`, switches `config_path` to S3.
pub async fn migrate_to_s3(
    config_path: &Path,
    opts: MigrateOptions,
) -> Result<MigrateReport, MigrateError> {
    let config =
        ServerConfig::load(config_path).map_err(|e| MigrateError::Config(e.to_string()))?;
    if config.storage.kind == StorageKind::S3 {
        return Err(MigrateError::AlreadyS3);
    }
    let Some(s3) = &config.storage.s3 else {
        return Err(MigrateError::NotPrepared);
    };
    let env = |name: &str| std::env::var(name).ok();
    let s3_cfg = s3
        .store_config(&config.data_dir, config.limits.max_blob_size, &env)
        .map_err(MigrateError::Settings)?;
    let bucket = S3BlobStore::open(s3_cfg).await?;
    bucket.check().await?;
    let fs = FsBlobStore::open(
        &config.data_dir,
        FsBlobStoreConfig {
            max_object_size: config.limits.max_blob_size,
        },
    )
    .await?;

    let mut report = MigrateReport::default();
    for depot in local_depots(&config.data_dir)? {
        report.depots += 1;
        copy_depot(&fs, &bucket, depot, &mut report).await?;
    }
    if !opts.copy_only {
        switch_config(config_path)?;
        report.switched = Some(config_path.to_owned());
    }
    Ok(report)
}

/// Depot ids that have a directory under `depots/`.
fn local_depots(data_dir: &Path) -> Result<Vec<DepotId>, MigrateError> {
    let dir = data_dir.join("depots");
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(format!("cannot read {}", dir.display()))(e)),
    };
    let mut ids: Vec<DepotId> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str()?.parse::<i64>().ok())
        .map(DepotId)
        .collect();
    ids.sort_by_key(|d| d.0);
    Ok(ids)
}

async fn ids_of(store: &dyn BlobStore, depot: DepotId) -> Result<HashSet<ObjectId>, StoreError> {
    store.list(depot).map_ok(|(id, _)| id).try_collect().await
}

async fn copy_depot(
    fs: &FsBlobStore,
    bucket: &S3BlobStore,
    depot: DepotId,
    report: &mut MigrateReport,
) -> Result<(), MigrateError> {
    let local = ids_of(fs, depot).await?;
    let there = ids_of(bucket, depot).await?;
    let mut todo: Vec<ObjectId> = local.difference(&there).copied().collect();
    todo.sort();
    report.already_there += (local.len() - todo.len()) as u64;
    let total = todo.len();
    let mut done = 0usize;
    let mut uploads = stream::iter(todo)
        .map(|id| async move {
            let path = fs.object_path(depot, &id);
            let len = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            match bucket.put_stored_file(depot, &id, &path).await {
                Ok(outcome) => Ok((outcome, len)),
                // Collected by GC since the listing: nothing to copy.
                Err(StoreError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
                    Ok((PutOutcome::AlreadyPresent, 0))
                }
                Err(e) => Err(e),
            }
        })
        .buffer_unordered(PARALLEL);
    while let Some(r) = uploads.next().await {
        let (outcome, len) = r?;
        done += 1;
        match outcome {
            PutOutcome::Created => {
                report.copied += 1;
                report.bytes += len;
            }
            PutOutcome::AlreadyPresent => report.already_there += 1,
        }
        if done.is_multiple_of(1000) {
            tracing::info!(depot = depot.0, done, total, "copying objects to S3");
        }
    }
    drop(uploads);
    // Check: every local object that still exists is now in the bucket.
    let there = ids_of(bucket, depot).await?;
    let missing = local
        .iter()
        .filter(|id| !there.contains(id) && fs.object_path(depot, id).is_file())
        .count() as u64;
    if missing > 0 {
        return Err(MigrateError::Incomplete {
            depot: depot.0,
            missing,
        });
    }
    Ok(())
}

/// Sets `kind = "s3"` in the `[storage]` table of the file, keeping everything else, and
/// keeps the old file as `loded.toml.before-s3`.
fn switch_config(path: &Path) -> Result<(), MigrateError> {
    let text =
        std::fs::read_to_string(path).map_err(io_err(format!("cannot read {}", path.display())))?;
    let new = with_s3_kind(&text).ok_or_else(|| {
        MigrateError::Config(format!(
            "cannot find the [storage] table in {}; set kind = \"s3\" there yourself",
            path.display()
        ))
    })?;
    let base = path.parent().unwrap_or(Path::new("."));
    let check =
        ServerConfig::parse(&new, base, path).map_err(|e| MigrateError::Config(e.to_string()))?;
    if check.storage.kind != StorageKind::S3 {
        return Err(MigrateError::Config(format!(
            "could not switch {} to S3; set kind = \"s3\" in [storage] yourself",
            path.display()
        )));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    let saved = path.with_file_name(format!("{name}.before-s3"));
    std::fs::copy(path, &saved).map_err(io_err(format!("cannot write {}", saved.display())))?;
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, new).map_err(io_err(format!("cannot write {}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(io_err(format!("cannot replace {}", path.display())))?;
    Ok(())
}

/// The text with `kind = "s3"` in its `[storage]` table: an existing `kind` line is
/// replaced, or one is added under the header. `None` without a `[storage]` header.
fn with_s3_kind(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_header = |l: &str| {
        let t = l.trim();
        t.starts_with('[') && !t.starts_with("[[")
    };
    let table_name = |l: &str| -> Option<String> {
        let t = l.trim();
        let t = t.split('#').next()?.trim();
        Some(t.strip_prefix('[')?.strip_suffix(']')?.trim().to_owned())
    };
    let start = lines
        .iter()
        .position(|l| is_header(l) && table_name(l).as_deref() == Some("storage"))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_header(l) || l.trim().starts_with("[["))
        .map_or(lines.len(), |i| start + 1 + i);
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let kind_line = format!("kind = \"s3\"{newline}");
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    let existing = (start + 1..end).find(|&i| {
        let t = lines[i].trim_start();
        t.strip_prefix("kind")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    });
    match existing {
        Some(i) => out[i] = kind_line,
        None => {
            if !out[start].ends_with('\n') {
                out[start].push_str(newline);
            }
            out.insert(start + 1, kind_line);
        }
    }
    Some(out.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_storage_kind_is_switched_in_place() {
        let text = "data_dir = '.'\n\n[storage]\n# notes\nkind = \"fs\"   # for now\nbucket = 'b-1'\n\n[gc]\ngrace_seconds = 1\n";
        assert_eq!(
            with_s3_kind(text).unwrap(),
            "data_dir = '.'\n\n[storage]\n# notes\nkind = \"s3\"\nbucket = 'b-1'\n\n[gc]\ngrace_seconds = 1\n"
        );
        let text = "data_dir = '.'\r\n[storage] # S3\r\nbucket = 'b-1'\r\n";
        assert_eq!(
            with_s3_kind(text).unwrap(),
            "data_dir = '.'\r\n[storage] # S3\r\nkind = \"s3\"\r\nbucket = 'b-1'\r\n"
        );
        // A `kind` in another table is not touched.
        let text = "[storage]\nbucket = 'b-1'\n[other]\nkind = 'fs'\n";
        assert_eq!(
            with_s3_kind(text).unwrap(),
            "[storage]\nkind = \"s3\"\nbucket = 'b-1'\n[other]\nkind = 'fs'\n"
        );
        assert_eq!(with_s3_kind("data_dir = '.'\n# [storage]\n"), None);
    }
}
