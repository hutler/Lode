//! Copies of backups in an S3-compatible bucket (`[backup.copy_to]`), and restoring from them.
//!
//! Layout under the bucket's prefix:
//!
//! - `objects/<depot-id>/<h0h1>/<h2h3>/<64-hex>`: stored objects, shared by every copy. An
//!   object never changes once written, so each copy uploads only objects the bucket does not
//!   have yet.
//! - `<backup-name>/meta.db` and `<backup-name>/loded.toml`: the database and configuration.
//!   A Postgres-backed server has `DATABASE-BACKUP.txt` instead of `meta.db` (back the
//!   database up with `pg_dump`).
//! - `<backup-name>/objects.zst`: the objects this copy needs, one `<depot-id>/<hex>` per line,
//!   compressed with zstd.
//! - `<backup-name>/done.json`: written last; a copy without it is incomplete and ignored.
//!
//! After a copy, complete copies beyond `keep` are removed, oldest first, together with
//! incomplete ones older than the newest complete copy, and then every object no kept copy
//! lists. That is also how obliterated content and erased depots leave the bucket.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use lode_store::S3BlobStore;
use serde::{Deserialize, Serialize};

use super::backup::DATABASE_NOTE_FILE;
use super::schedule::BACKUP_PREFIX;
use crate::config::{BackupCopy, CONFIG_FILE_NAME};
use crate::server::{DEPOTS_DIR, META_DB_FILE};

const OBJECTS: &str = "objects/";
const LIST_FILE: &str = "objects.zst";
const DONE_FILE: &str = "done.json";

/// Why copying or restoring failed.
#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    /// The bucket settings are unusable, such as missing credentials.
    #[error("[backup.copy_to]: {0}")]
    Config(String),
    /// The bucket answered with an error or could not be reached.
    #[error("{0}")]
    Store(#[from] lode_store::StoreError),
    /// A local file could not be read or written.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        source: io::Error,
    },
    /// There is nothing to restore.
    #[error("{0}")]
    NotFound(String),
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> CopyError {
    let context = context.into();
    move |source| CopyError::Io { context, source }
}

/// What a copy sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyReport {
    /// The copy's name, the same as the local backup's.
    pub name: String,
    /// Objects the copy needs.
    pub objects: u64,
    /// Objects uploaded now; the rest were in the bucket already.
    pub uploaded: u64,
    /// Bytes uploaded, objects and database together.
    pub bytes: u64,
    /// Old copies removed.
    #[serde(default)]
    pub removed: Vec<String>,
    /// Objects removed because no kept copy needs them.
    #[serde(default)]
    pub objects_removed: u64,
}

/// `done.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Done {
    server_version: String,
    objects: u64,
}

/// A complete copy in the bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteBackup {
    /// Its name, `lode-backup-<UTC time>`.
    pub name: String,
}

/// Opens the bucket of `[backup.copy_to]`. `data_dir` holds the temporary upload folder.
pub async fn open_bucket(copy: &BackupCopy, data_dir: &Path) -> Result<S3BlobStore, CopyError> {
    let env = |k: &str| std::env::var(k).ok();
    let cfg = copy
        .s3
        .store_config(data_dir, u64::MAX, &env)
        .map_err(CopyError::Config)?;
    Ok(S3BlobStore::open(cfg).await?)
}

/// Object files of a local backup, as `(<depot-id>/<hex>, path)`, sorted.
fn local_objects(backup: &Path) -> Result<Vec<(String, PathBuf)>, CopyError> {
    let mut out = Vec::new();
    let depots = backup.join(DEPOTS_DIR);
    let rd = match std::fs::read_dir(&depots) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(io_err(format!("cannot read {}", depots.display()))(e)),
    };
    for depot in rd {
        let depot = depot.map_err(io_err(format!("cannot read {}", depots.display())))?;
        let id = depot.file_name().to_string_lossy().into_owned();
        if id.parse::<u64>().is_err() {
            continue;
        }
        walk(&depot.path().join("objects"), &id, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// Object files below `dir` (`<h0h1>/<h2h3>/<hex>`), added as `<depot>/<hex>`.
fn walk(dir: &Path, depot: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), CopyError> {
    let rd = std::fs::read_dir(dir).map_err(io_err(format!("cannot read {}", dir.display())))?;
    for entry in rd {
        let entry = entry.map_err(io_err(format!("cannot read {}", dir.display())))?;
        let ft = entry.file_type().map_err(io_err("cannot stat"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            walk(&entry.path(), depot, out)?;
        } else if ft.is_file() && name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push((format!("{depot}/{name}"), entry.path()));
        }
    }
    Ok(())
}

/// `<depot-id>/<hex>` to its key under `objects/`.
fn object_key(entry: &str) -> Option<String> {
    let (depot, hex) = entry.split_once('/')?;
    if hex.len() != 64 || depot.is_empty() {
        return None;
    }
    Some(format!(
        "{OBJECTS}{depot}/{}/{}/{hex}",
        &hex[0..2],
        &hex[2..4]
    ))
}

/// Writes the zstd-compressed object list.
fn write_list(path: &Path, entries: &[String]) -> Result<(), CopyError> {
    let file = std::fs::File::create(path).map_err(io_err("cannot write the object list"))?;
    let mut enc = zstd::Encoder::new(file, 3).map_err(io_err("cannot compress"))?;
    for e in entries {
        writeln!(enc, "{e}").map_err(io_err("cannot write the object list"))?;
    }
    enc.finish()
        .map_err(io_err("cannot write the object list"))?;
    Ok(())
}

fn read_list(path: &Path) -> Result<Vec<String>, CopyError> {
    let file = std::fs::File::open(path).map_err(io_err("cannot read an object list"))?;
    let dec = zstd::Decoder::new(file).map_err(io_err("cannot read an object list"))?;
    let mut out = Vec::new();
    for line in io::BufReader::new(dec).lines() {
        let line = line.map_err(io_err("cannot read an object list"))?;
        if !line.is_empty() {
            out.push(line);
        }
    }
    Ok(out)
}

fn is_backup_name(name: &str) -> bool {
    name.strip_prefix(BACKUP_PREFIX)
        .is_some_and(|s| s.len() == 16 && s.ends_with('Z'))
}

/// Copies in the bucket: complete ones (oldest first) and incomplete ones.
async fn remote_copies(bucket: &S3BlobStore) -> Result<(Vec<String>, Vec<String>), CopyError> {
    let keys = bucket.list_keys(BACKUP_PREFIX).await?;
    let mut all = BTreeSet::new();
    let mut done = BTreeSet::new();
    for k in &keys {
        let Some((name, file)) = k.split_once('/') else {
            continue;
        };
        if !is_backup_name(name) {
            continue;
        }
        all.insert(name.to_owned());
        if file == DONE_FILE {
            done.insert(name.to_owned());
        }
    }
    let incomplete = all.difference(&done).cloned().collect();
    Ok((done.into_iter().collect(), incomplete))
}

/// The complete copies in the bucket, oldest first.
pub async fn list_remote(bucket: &S3BlobStore) -> Result<Vec<RemoteBackup>, CopyError> {
    Ok(remote_copies(bucket)
        .await?
        .0
        .into_iter()
        .map(|name| RemoteBackup { name })
        .collect())
}

async fn delete_copy(bucket: &S3BlobStore, name: &str) -> Result<(), CopyError> {
    // done.json first, so a copy cut short while being removed is no longer complete.
    bucket.delete_key(&format!("{name}/{DONE_FILE}")).await?;
    for k in bucket.list_keys(&format!("{name}/")).await? {
        bucket.delete_key(&k).await?;
    }
    Ok(())
}

/// Sends the local backup at `backup` (a folder `lode-backup-<time>`) to the bucket, then
/// removes copies beyond `copy.keep` and objects no kept copy needs. `tmp` is a scratch
/// directory for the object list.
pub async fn copy_backup(
    copy: &BackupCopy,
    bucket: &S3BlobStore,
    backup: &Path,
    tmp: &Path,
) -> Result<CopyReport, CopyError> {
    let name = backup
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| is_backup_name(n))
        .ok_or_else(|| {
            CopyError::NotFound(format!("{} is not a scheduled backup", backup.display()))
        })?;
    let dir = backup.to_owned();
    let objects = tokio::task::spawn_blocking(move || local_objects(&dir))
        .await
        .map_err(|e| io_err("listing task failed")(io::Error::other(e)))??;
    let mut report = CopyReport {
        name: name.clone(),
        objects: objects.len() as u64,
        ..CopyReport::default()
    };

    let present: HashSet<String> = bucket.list_keys(OBJECTS).await?.into_iter().collect();
    for (entry, path) in &objects {
        let Some(key) = object_key(entry) else {
            continue;
        };
        if present.contains(&key) {
            continue;
        }
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if bucket.put_file(&key, path, true).await? {
            report.uploaded += 1;
            report.bytes += len;
        }
    }

    // A Postgres-backed server's backup holds a note instead of the database.
    for file in [META_DB_FILE, DATABASE_NOTE_FILE, CONFIG_FILE_NAME] {
        let path = backup.join(file);
        if path.is_file() {
            bucket
                .put_file(&format!("{name}/{file}"), &path, false)
                .await?;
            report.bytes += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        }
    }
    std::fs::create_dir_all(tmp).map_err(io_err("cannot create a temporary folder"))?;
    let list = tmp.join(format!("{name}.{LIST_FILE}"));
    let entries: Vec<String> = objects.into_iter().map(|(e, _)| e).collect();
    write_list(&list, &entries)?;
    let sent = bucket
        .put_file(&format!("{name}/{LIST_FILE}"), &list, false)
        .await;
    let _ = std::fs::remove_file(&list);
    sent?;
    let done = tmp.join(format!("{name}.{DONE_FILE}"));
    let body = serde_json::to_vec(&Done {
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        objects: report.objects,
    })
    .map_err(|e| io_err("cannot encode")(io::Error::other(e)))?;
    std::fs::write(&done, body).map_err(io_err("cannot write done.json"))?;
    let sent = bucket
        .put_file(&format!("{name}/{DONE_FILE}"), &done, false)
        .await;
    let _ = std::fs::remove_file(&done);
    sent?;

    prune(copy, bucket, tmp, &mut report).await?;
    Ok(report)
}

/// Removes copies beyond `keep`, incomplete copies older than the newest complete one, and
/// objects that no kept copy lists.
async fn prune(
    copy: &BackupCopy,
    bucket: &S3BlobStore,
    tmp: &Path,
    report: &mut CopyReport,
) -> Result<(), CopyError> {
    let (complete, incomplete) = remote_copies(bucket).await?;
    let extra = complete.len().saturating_sub(copy.keep.max(1));
    let (old, kept) = complete.split_at(extra);
    for name in old {
        delete_copy(bucket, name).await?;
        report.removed.push(name.clone());
    }
    if let Some(newest) = kept.last() {
        for name in incomplete.iter().filter(|n| *n < newest) {
            delete_copy(bucket, name).await?;
            report.removed.push(name.clone());
        }
    }
    // Objects every kept copy needs, and objects of a copy being written right now (one
    // newer than the newest complete copy) are left alone.
    if incomplete.iter().any(|n| kept.last().is_none_or(|k| n > k)) {
        return Ok(());
    }
    let mut needed = HashSet::new();
    for name in kept {
        let path = tmp.join(format!("{name}.{LIST_FILE}"));
        bucket
            .get_file(&format!("{name}/{LIST_FILE}"), &path)
            .await?;
        let list = read_list(&path);
        let _ = std::fs::remove_file(&path);
        needed.extend(list?.iter().filter_map(|e| object_key(e)));
    }
    for key in bucket.list_keys(OBJECTS).await? {
        if !needed.contains(&key) {
            bucket.delete_key(&key).await?;
            report.objects_removed += 1;
        }
    }
    Ok(())
}

/// What a restore wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    /// The copy restored.
    pub name: String,
    /// Objects downloaded.
    pub objects: u64,
    /// Bytes downloaded.
    pub bytes: u64,
}

/// Downloads copy `name` (the newest complete one when `None`) into `dest`, which must be
/// missing or empty. The result is a data directory `loded` can open, the same as a local
/// backup.
pub async fn restore(
    bucket: &S3BlobStore,
    name: Option<&str>,
    dest: &Path,
) -> Result<RestoreReport, CopyError> {
    if dest.exists() {
        let mut rd =
            std::fs::read_dir(dest).map_err(io_err(format!("cannot read {}", dest.display())))?;
        if rd.next().is_some() {
            return Err(CopyError::NotFound(format!(
                "{} exists and is not empty",
                dest.display()
            )));
        }
    }
    let (complete, _) = remote_copies(bucket).await?;
    let name = match name {
        Some(n) if complete.iter().any(|c| c == n) => n.to_owned(),
        Some(n) => {
            return Err(CopyError::NotFound(format!(
                "the bucket has no complete copy named {n}"
            )))
        }
        None => complete
            .last()
            .cloned()
            .ok_or_else(|| CopyError::NotFound("the bucket holds no complete copy".into()))?,
    };
    std::fs::create_dir_all(dest).map_err(io_err(format!("cannot create {}", dest.display())))?;
    let mut report = RestoreReport {
        name: name.clone(),
        ..RestoreReport::default()
    };
    // The database, or for a Postgres-backed server the note saying it is backed up there.
    let mut database = false;
    for file in [META_DB_FILE, DATABASE_NOTE_FILE, CONFIG_FILE_NAME] {
        match bucket
            .get_file(&format!("{name}/{file}"), &dest.join(file))
            .await
        {
            Ok(n) => {
                report.bytes += n;
                database |= file != CONFIG_FILE_NAME;
            }
            Err(lode_store::StoreError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    if !database {
        return Err(CopyError::NotFound(format!(
            "copy {name} holds neither {META_DB_FILE} nor {DATABASE_NOTE_FILE}"
        )));
    }
    let list_path = dest.join(LIST_FILE);
    bucket
        .get_file(&format!("{name}/{LIST_FILE}"), &list_path)
        .await?;
    let entries = read_list(&list_path);
    let _ = std::fs::remove_file(&list_path);
    std::fs::create_dir_all(dest.join(DEPOTS_DIR)).map_err(io_err("cannot create depots/"))?;
    for entry in entries? {
        let (Some(key), Some((depot, hex))) = (object_key(&entry), entry.split_once('/')) else {
            continue;
        };
        let dir = dest
            .join(DEPOTS_DIR)
            .join(depot)
            .join("objects")
            .join(&hex[0..2])
            .join(&hex[2..4]);
        std::fs::create_dir_all(&dir).map_err(io_err("cannot create a depot folder"))?;
        report.bytes += bucket.get_file(&key, &dir.join(hex)).await?;
        report.objects += 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names() {
        let hex = "ab".repeat(32);
        assert_eq!(
            object_key(&format!("3/{hex}")).unwrap_or_default(),
            format!("objects/3/ab/ab/{hex}")
        );
        assert!(object_key("3/short").is_none());
        assert!(is_backup_name("lode-backup-20261120T120000Z"));
        assert!(!is_backup_name("lode-backup-x"));
    }

    #[test]
    fn lists_round_trip() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let p = dir.path().join("l.zst");
        let entries = vec!["1/aa".to_owned(), "2/bb".to_owned()];
        write_list(&p, &entries).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_list(&p).unwrap_or_default(), entries);
    }
}
