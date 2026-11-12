//! Content-addressed blob cache with a size cap and least-recently-used eviction, for diff
//! and merge bases (ARCHITECTURE §10.1). Also usable as the shared download cache: entries
//! are written with temp-plus-rename and checked against their id when read, so concurrent
//! processes and a damaged file are both safe.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lode_core::ObjectId;

/// Default size cap: 1 GiB.
pub const DEFAULT_CACHE_CAP: u64 = 1024 * 1024 * 1024;

/// Temporary files older than this are leftovers of a killed process.
const STALE_TMP: Duration = Duration::from_secs(3600);

const TMP_PREFIX: &str = ".tmp-";

/// A blob cache directory.
#[derive(Debug, Clone)]
pub struct BlobCache {
    dir: PathBuf,
    cap: u64,
}

/// What [`BlobCache::evict`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Bytes held before.
    pub bytes_before: u64,
    /// Entries removed.
    pub removed: usize,
    /// Bytes held after.
    pub bytes_after: u64,
}

impl BlobCache {
    /// A cache in `dir` holding at most `cap` bytes after each insert.
    pub fn new(dir: impl Into<PathBuf>, cap: u64) -> Self {
        Self {
            dir: dir.into(),
            cap,
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.dir.join(&hex[..2]).join(hex)
    }

    /// The cached content of `id`, checked against the id. A damaged entry is removed and
    /// reported as absent. A hit counts as a use for eviction.
    pub fn get(&self, id: &ObjectId) -> io::Result<Option<PathBuf>> {
        let path = self.entry_path(id);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if lode_core::blob_id(&file)? != *id {
            drop(file);
            tracing::warn!(%id, "removing a damaged cache entry");
            remove_if_exists(&path)?;
            return Ok(None);
        }
        // Eviction goes by modification time; refresh it so recently used entries stay.
        let _ = File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        Ok(Some(path))
    }

    /// Whether `id` is cached, without reading it.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.entry_path(id).is_file()
    }

    /// Copies the file at `src` into the cache as `id`, refusing content that does not hash
    /// to `id`. Evicts afterwards to stay under the cap.
    pub fn insert_file(&self, id: &ObjectId, src: &Path) -> io::Result<PathBuf> {
        self.insert_reader(id, File::open(src)?)
    }

    /// Stores content read from `reader` as `id`; see [`BlobCache::insert_file`].
    pub fn insert_reader(&self, id: &ObjectId, mut reader: impl Read) -> io::Result<PathBuf> {
        let dest = self.entry_path(id);
        if dest.is_file() {
            return Ok(dest);
        }
        let parent = dest
            .parent()
            .ok_or_else(|| io::Error::other("cache entry has no parent"))?;
        fs::create_dir_all(parent)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(TMP_PREFIX)
            .tempfile_in(parent)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n])?;
        }
        let actual = ObjectId::from_bytes(*hasher.finalize().as_bytes());
        if actual != *id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("content hashes to {actual}, not {id}"),
            ));
        }
        tmp.as_file().sync_all()?;
        // Another process may have stored the same entry meanwhile; either copy is correct.
        match tmp.persist(&dest) {
            Ok(_) => {}
            Err(e) if dest.is_file() => drop(e),
            Err(e) => return Err(e.error),
        }
        self.evict()?;
        Ok(dest)
    }

    /// Removes least recently used entries until the cache holds at most the cap, and
    /// temporary files left by killed processes.
    pub fn evict(&self) -> io::Result<EvictReport> {
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let now = SystemTime::now();
        let shards = match fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(EvictReport::default()),
            Err(e) => return Err(e),
        };
        for shard in shards {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let meta = match entry.metadata() {
                    Ok(m) if m.is_file() => m,
                    _ => continue,
                };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
                    if now.duration_since(mtime).unwrap_or_default() > STALE_TMP {
                        remove_if_exists(&entry.path())?;
                    }
                    continue;
                }
                entries.push((mtime, meta.len(), entry.path()));
            }
        }
        let before: u64 = entries.iter().map(|e| e.1).sum();
        let mut report = EvictReport {
            bytes_before: before,
            removed: 0,
            bytes_after: before,
        };
        if before <= self.cap {
            return Ok(report);
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        for (_, len, path) in entries {
            if report.bytes_after <= self.cap {
                break;
            }
            remove_if_exists(&path)?;
            report.bytes_after -= len;
            report.removed += 1;
        }
        Ok(report)
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn id(b: &[u8]) -> ObjectId {
        lode_core::blob_id(b).unwrap()
    }

    fn age(path: &Path, secs: u64) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn insert_get_verify_and_evict() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().join("cache"), 25);
        let a = vec![b'a'; 10];
        let b = vec![b'b'; 10];
        let c = vec![b'c'; 10];
        let pa = cache.insert_reader(&id(&a), &a[..]).unwrap();
        let pb = cache.insert_reader(&id(&b), &b[..]).unwrap();
        age(&pa, 300);
        age(&pb, 200);
        // Using `a` makes `b` the least recently used.
        assert_eq!(cache.get(&id(&a)).unwrap(), Some(pa.clone()));
        cache.insert_reader(&id(&c), &c[..]).unwrap();
        assert!(cache.contains(&id(&a)));
        assert!(!cache.contains(&id(&b)));
        assert!(cache.contains(&id(&c)));

        // Wrong content is refused.
        assert!(cache.insert_reader(&id(b"x"), &b"y"[..]).is_err());
        assert!(!cache.contains(&id(b"x")));

        // A damaged entry is dropped on read.
        fs::write(&pa, b"tampered!!").unwrap();
        assert_eq!(cache.get(&id(&a)).unwrap(), None);
        assert!(!pa.exists());
        assert_eq!(cache.get(&id(b"never")).unwrap(), None);
    }
}
