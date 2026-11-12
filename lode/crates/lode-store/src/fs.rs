//! Filesystem implementation of [`BlobStore`].
//!
//! Layout: `<root>/depots/<depot-id>/objects/<h0h1>/<h2h3>/<64-hex>`, with uploads in
//! progress under `<root>/depots/<depot-id>/tmp/`.
//!
//! **Compression heuristic.** zstd level 3 is used unless it saves less than 3 %. Content up
//! to 1 MiB is compressed in one go and the decision uses its exact size. For longer content
//! the decision is made once, on the first 1 MiB, and then kept for the rest of the stream,
//! so large incompressible files (video, archives, already compressed textures) are never
//! compressed twice. The first MiB is therefore held in memory; everything else streams
//! through a 64 KiB buffer.
//!
//! **No-replace publish.** A finished temp file is published with a hard link, which fails
//! if the target exists on every supported platform, then the temp name is removed. On a
//! filesystem without hard links the store falls back to "rename if absent", which is not
//! atomic against a concurrent writer of the same object; both writers hold identical
//! verified content, so the only effect is a lost `AlreadyPresent` answer.

use std::io::{self, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures::stream::{self, BoxStream, StreamExt};
use lode_core::{Codec, DepotId, ObjectHeader, ObjectId, ObjectKind, TREE_ID_CONTEXT};
use tokio::fs::{self as tfs, File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::zstd_reader::ZstdReader;
use crate::{BlobStore, ObjectReader, PutOptions, PutOutcome, Result, StoreError, StoredReader};

const CHUNK: usize = 64 * 1024;
const PROBE: usize = 1024 * 1024;
const ZSTD_LEVEL: i32 = 3;
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

    fn object_path(&self, depot: DepotId, id: &ObjectId) -> PathBuf {
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

enum Hasher {
    Blob(blake3::Hasher),
    Tree(blake3::Hasher),
}

impl Hasher {
    fn new(kind: ObjectKind) -> Result<Self> {
        match kind {
            ObjectKind::Blob => Ok(Hasher::Blob(blake3::Hasher::new())),
            ObjectKind::Tree => Ok(Hasher::Tree(blake3::Hasher::new_derive_key(
                TREE_ID_CONTEXT,
            ))),
            ObjectKind::ChunkList => Err(StoreError::UnsupportedKind(kind)),
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self {
            Hasher::Blob(h) | Hasher::Tree(h) => {
                h.update(b);
            }
        }
    }

    fn finish(&self) -> ObjectId {
        match self {
            Hasher::Blob(h) | Hasher::Tree(h) => ObjectId::from_bytes(*h.finalize().as_bytes()),
        }
    }
}

/// Deletes the temp file on drop unless disarmed, so every failure path cleans up.
struct TempGuard {
    path: Option<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

struct Limits {
    cap: u64,
    expected: Option<u64>,
}

impl Limits {
    fn check(&self, total: u64) -> Result<()> {
        if total > self.cap {
            return Err(StoreError::TooLarge { limit: self.cap });
        }
        if let Some(exp) = self.expected {
            if total > exp {
                return Err(StoreError::LengthMismatch {
                    expected: exp,
                    actual: total,
                });
            }
        }
        Ok(())
    }
}

async fn read_some(r: &mut (dyn AsyncRead + Send + Unpin), buf: &mut [u8]) -> Result<usize> {
    loop {
        match r.read(buf).await {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
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
        let mut hasher = Hasher::new(kind)?;
        let limits = Limits {
            cap: self.config.max_object_size,
            expected: opts.expected_len,
        };
        if let Some(exp) = opts.expected_len {
            if exp > limits.cap {
                return Err(StoreError::TooLarge { limit: limits.cap });
            }
        }

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
        // Placeholder; the real header is written once the codec and length are known.
        file.write_all(&[0u8; ObjectHeader::LEN]).await?;

        // Probe: read up to 1 MiB to choose the codec.
        let mut probe =
            Vec::with_capacity(PROBE.min(opts.expected_len.unwrap_or(PROBE as u64) as usize));
        let mut chunk = vec![0u8; CHUNK];
        let mut total: u64 = 0;
        let mut eof = false;
        while probe.len() < PROBE {
            let want = (PROBE - probe.len()).min(CHUNK);
            let n = read_some(raw, &mut chunk[..want]).await?;
            if n == 0 {
                eof = true;
                break;
            }
            total += n as u64;
            limits.check(total)?;
            hasher.update(&chunk[..n]);
            probe.extend_from_slice(&chunk[..n]);
        }
        let compressed_probe = zstd::bulk::compress(&probe, ZSTD_LEVEL)?;
        // Use zstd only if it saves at least 3 %.
        let use_zstd = (compressed_probe.len() as u128) * 100 <= (probe.len() as u128) * 97;
        let codec = if use_zstd { Codec::Zstd } else { Codec::None };

        if eof {
            let payload = if use_zstd { &compressed_probe } else { &probe };
            file.write_all(payload).await?;
        } else if !use_zstd {
            file.write_all(&probe).await?;
            loop {
                let n = read_some(raw, &mut chunk).await?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                limits.check(total)?;
                hasher.update(&chunk[..n]);
                file.write_all(&chunk[..n]).await?;
            }
        } else {
            drop(compressed_probe);
            let mut enc = zstd::stream::write::Encoder::new(Vec::with_capacity(CHUNK), ZSTD_LEVEL)?;
            enc.write_all(&probe)?;
            drop(probe);
            file.write_all(enc.get_ref()).await?;
            enc.get_mut().clear();
            loop {
                let n = read_some(raw, &mut chunk).await?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                limits.check(total)?;
                hasher.update(&chunk[..n]);
                enc.write_all(&chunk[..n])?;
                if !enc.get_ref().is_empty() {
                    file.write_all(enc.get_ref()).await?;
                    enc.get_mut().clear();
                }
            }
            let rest = enc.finish()?;
            file.write_all(&rest).await?;
        }

        if let Some(exp) = opts.expected_len {
            if total != exp {
                return Err(StoreError::LengthMismatch {
                    expected: exp,
                    actual: total,
                });
            }
        }
        let actual = hasher.finish();
        if actual != *expect {
            return Err(StoreError::HashMismatch {
                expected: *expect,
                actual,
            });
        }

        let header = ObjectHeader {
            kind,
            codec,
            raw_len: total,
        };
        file.seek(SeekFrom::Start(0)).await?;
        file.write_all(&header.encode()).await?;
        file.flush().await?;
        file.sync_all().await?;
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

fn remove_stale_tmp(depots: &Path, max_age: Duration) -> io::Result<()> {
    let now = SystemTime::now();
    for depot in std::fs::read_dir(depots)?.filter_map(|e| e.ok()) {
        let tmp = depot.path().join("tmp");
        let Ok(rd) = std::fs::read_dir(&tmp) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
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
    }
    Ok(())
}
