//! Filesystem implementation of [`BlobStore`].
//!
//! Layout: `<root>/depots/<depot-id>/objects/<h0h1>/<h2h3>/<64-hex>`, with uploads in
//! progress under `<root>/depots/<depot-id>/tmp/`.
//!
//! Objects are encoded into a temp file by [`crate::encode`] (compression heuristic there).
//!
//! **No-replace publish.** A finished temp file is published with a hard link, which fails
//! if the target exists on every supported platform, then the temp name is removed. On a
//! filesystem without hard links the store falls back to "rename if absent", which is not
//! atomic against a concurrent writer of the same object; both writers hold identical
//! verified content, so the only effect is a lost `AlreadyPresent` answer.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures::stream::{self, BoxStream, StreamExt};
use lode_core::{Codec, DepotId, ObjectHeader, ObjectId, ObjectKind};
use tokio::fs::{self as tfs, File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::encode::{encode, Limits, CHUNK};
use crate::zstd_reader::ZstdReader;
use crate::{BlobStore, ObjectReader, PutOptions, PutOutcome, Result, StoreError, StoredReader};

const STALE_TMP: Duration = Duration::from_secs(24 * 60 * 60);

/// Settings for [`FsBlobStore`].
#[derive(Debug, Clone)]
pub struct FsBlobStoreConfig {
    /// Largest raw object accepted. ARCHITECTURE §6 default: 64 GiB.
    pub max_object_size: u64,
}

impl Default for FsBlobStoreConfig {
    fn default() -> Self {
        Self {
            max_object_size: 64 * 1024 * 1024 * 1024,
        }
    }
}

/// Blob store on a local filesystem.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
    config: FsBlobStoreConfig,
}

impl FsBlobStore {
    /// Opens (creating if needed) a store under `root`, and removes temp files older than
    /// 24 hours left by interrupted uploads.
    pub async fn open(root: impl Into<PathBuf>, config: FsBlobStoreConfig) -> Result<Self> {
        let store = Self {
            root: root.into(),
            config,
        };
        tfs::create_dir_all(store.root.join("depots")).await?;
        let depots = store.root.join("depots");
        tokio::task::spawn_blocking(move || remove_stale_tmp(&depots, STALE_TMP))
            .await
            .map_err(io::Error::other)??;
        Ok(store)
    }

    fn depot_dir(&self, depot: DepotId) -> PathBuf {
        self.root.join("depots").join(depot.0.to_string())
    }

    /// The file that holds an object.
    pub fn object_path(&self, depot: DepotId, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.depot_dir(depot)
            .join("objects")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex)
    }

    async fn open_object(
        &self,
        depot: DepotId,
        id: &ObjectId,
    ) -> Result<(File, ObjectHeader, u64)> {
        let path = self.object_path(depot, id);
        let mut file = match File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StoreError::NotFound(*id)),
            Err(e) => return Err(e.into()),
        };
        let file_len = file.metadata().await?.len();
        let mut head = [0u8; ObjectHeader::LEN];
        if let Err(e) = file.read_exact(&mut head).await {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Err(corrupt(id, "file is shorter than the header"));
            }
            return Err(e.into());
        }
        let header = ObjectHeader::decode(&head).map_err(|e| corrupt(id, &e.to_string()))?;
        let stored_len = file_len - ObjectHeader::LEN as u64;
        if header.codec == Codec::None && stored_len != header.raw_len {
            return Err(corrupt(id, "payload length differs from the header"));
        }
        Ok((file, header, stored_len))
    }
}

fn corrupt(id: &ObjectId, reason: &str) -> StoreError {
    StoreError::Corrupt {
        id: *id,
        reason: reason.to_owned(),
    }
}

/// Deletes the temp file on drop unless disarmed, so every failure path cleans up.
pub(crate) struct TempGuard {
    pub(crate) path: Option<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[async_trait::async_trait]
impl BlobStore for FsBlobStore {
    async fn contains(&self, depot: DepotId, id: &ObjectId) -> Result<bool> {
        Ok(tfs::try_exists(self.object_path(depot, id)).await?)
    }

    async fn put(
        &self,
        depot: DepotId,
        kind: ObjectKind,
        expect: &ObjectId,
        raw: &mut (dyn AsyncRead + Send + Unpin),
        opts: PutOptions,
    ) -> Result<PutOutcome> {
        let limits = Limits {
            cap: self.config.max_object_size,
            expected: opts.expected_len,
        };
        limits.precheck()?;
        let tmp_dir = self.depot_dir(depot).join("tmp");
        tfs::create_dir_all(&tmp_dir).await?;
        let tmp_path = tmp_dir.join(uuid::Uuid::new_v4().simple().to_string());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await?;
        let mut guard = TempGuard {
            path: Some(tmp_path.clone()),
        };
        encode(&mut file, kind, expect, raw, &limits).await?;
        drop(file);

        let target = self.object_path(depot, expect);
        let outcome = tokio::task::spawn_blocking(move || publish(&tmp_path, &target))
            .await
            .map_err(io::Error::other)??;
        guard.path = None;
        Ok(outcome)
    }

    async fn get(&self, depot: DepotId, id: &ObjectId) -> Result<ObjectReader> {
        let (file, header, _) = self.open_object(depot, id).await?;
        let reader: crate::ByteStream = match header.codec {
            Codec::None => Box::pin(file.take(header.raw_len)),
            Codec::Zstd => Box::pin(ZstdReader::new(
                *id,
                tokio::io::BufReader::with_capacity(CHUNK, file),
                header.raw_len,
            )?),
        };
        Ok(ObjectReader {
            kind: header.kind,
            raw_len: header.raw_len,
            reader,
        })
    }

    async fn get_stored(&self, depot: DepotId, id: &ObjectId) -> Result<StoredReader> {
        let (file, header, stored_len) = self.open_object(depot, id).await?;
        Ok(StoredReader {
            kind: header.kind,
            codec: header.codec,
            raw_len: header.raw_len,
            stored_len,
            reader: Box::pin(file),
        })
    }

    async fn delete(&self, depot: DepotId, id: &ObjectId) -> Result<()> {
        match tfs::remove_file(self.object_path(depot, id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(StoreError::NotFound(*id)),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete_depot(&self, depot: DepotId) -> Result<()> {
        match tfs::remove_dir_all(self.depot_dir(depot)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, depot: DepotId) -> BoxStream<'_, Result<(ObjectId, SystemTime)>> {
        let dir = self.depot_dir(depot).join("objects");
        stream::once(async move {
            tokio::task::spawn_blocking(move || list_objects(&dir))
                .await
                .map_err(|e| StoreError::Io(io::Error::other(e)))
        })
        .flat_map(|res| match res {
            Ok(items) => stream::iter(items).boxed(),
            Err(e) => stream::iter(vec![Err(e)]).boxed(),
        })
        .boxed()
    }
}

/// Moves a finished temp file into place without ever replacing an existing object.
fn publish(tmp: &Path, target: &Path) -> Result<PutOutcome> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("object path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let outcome = match std::fs::hard_link(tmp, target) {
        Ok(()) => PutOutcome::Created,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => PutOutcome::AlreadyPresent,
        Err(e) if hard_links_unsupported(&e) => {
            if target.exists() {
                PutOutcome::AlreadyPresent
            } else {
                std::fs::rename(tmp, target)?;
                sync_dir(parent)?;
                return Ok(PutOutcome::Created);
            }
        }
        Err(e) => return Err(e.into()),
    };
    std::fs::remove_file(tmp)?;
    if outcome == PutOutcome::Created {
        sync_dir(parent)?;
    }
    Ok(outcome)
}

fn hard_links_unsupported(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied)
        // ERROR_INVALID_FUNCTION and ERROR_NOT_SUPPORTED on Windows (FAT, some network shares).
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(1) | Some(50)))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    // Windows has no directory fsync; NTFS journals the metadata change.
    Ok(())
}

fn list_objects(objects: &Path) -> Vec<Result<(ObjectId, SystemTime)>> {
    let mut out = Vec::new();
    let level = |p: &Path| -> Vec<PathBuf> {
        match std::fs::read_dir(p) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.len() == 2
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
                .map(|e| e.path())
                .collect(),
            Err(_) => Vec::new(),
        }
    };
    for d1 in level(objects) {
        for d2 in level(&d1) {
            let rd = match std::fs::read_dir(&d2) {
                Ok(rd) => rd,
                Err(e) => {
                    out.push(Err(e.into()));
                    continue;
                }
            };
            for entry in rd.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Ok(id) = ObjectId::from_hex(name) else {
                    continue;
                };
                if id.to_hex() != name {
                    continue; // upper-case or otherwise non-canonical name
                }
                let prefix_ok = d1.file_name().is_some_and(|n| n == &name[0..2])
                    && d2.file_name().is_some_and(|n| n == &name[2..4]);
                if !prefix_ok {
                    continue;
                }
                match entry.metadata().and_then(|m| m.modified()) {
                    Ok(t) if entry.file_type().is_ok_and(|t| t.is_file()) => out.push(Ok((id, t))),
                    Ok(_) => {}
                    Err(e) => out.push(Err(e.into())),
                }
            }
        }
    }
    out
}

pub(crate) fn remove_stale_tmp(depots: &Path, max_age: Duration) -> io::Result<()> {
    for depot in std::fs::read_dir(depots)?.filter_map(|e| e.ok()) {
        let _ = remove_stale_tmp_dir(&depot.path().join("tmp"), max_age);
    }
    Ok(())
}

/// Removes files older than `max_age` from one temp directory.
pub(crate) fn remove_stale_tmp_dir(tmp: &Path, max_age: Duration) -> io::Result<()> {
    let now = SystemTime::now();
    for entry in std::fs::read_dir(tmp)?.filter_map(|e| e.ok()) {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > max_age)
            .unwrap_or(false);
        if old {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(path = %entry.path().display(), error = %e, "could not remove stale temp file");
            }
        }
    }
    Ok(())
}
