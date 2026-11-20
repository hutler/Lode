//! The proxy's disk cache: whole blobs, chunks and chunk lists, per depot, evicted by last
//! use down to a size cap.
//!
//! Entries live at `<cache_dir>/objects/<depot>/<ab>/<id>` (chunk lists with `.chunks`),
//! where `<depot>` is the depot name in hex. A file is written under `tmp/` and renamed into
//! place, so a crash leaves at most a stray temporary file, removed at the next start. The
//! index is rebuilt from the files at start, ordered by modification time, which is touched
//! on every use.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use lode_core::ObjectId;

/// What a cache entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Content: a whole file or one chunk. Checked against its id.
    Content,
    /// The chunk list of a chunked file.
    ChunkList,
}

/// One cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    /// Depot name as the request gave it.
    pub depot: String,
    /// Object id.
    pub id: ObjectId,
    /// What it holds.
    pub kind: Kind,
}

impl Key {
    /// Content `id` of `depot`.
    pub fn content(depot: &str, id: ObjectId) -> Self {
        Self {
            depot: depot.to_owned(),
            id,
            kind: Kind::Content,
        }
    }

    /// The chunk list of `id` in `depot`.
    pub fn list(depot: &str, id: ObjectId) -> Self {
        Self {
            depot: depot.to_owned(),
            id,
            kind: Kind::ChunkList,
        }
    }
}

struct Entry {
    size: u64,
    last_use: u64,
}

struct Index {
    entries: HashMap<Key, Entry>,
    total: u64,
    tick: u64,
}

/// Size and count of the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheSize {
    /// Bytes held.
    pub bytes: u64,
    /// Entries held.
    pub entries: u64,
}

/// The disk cache.
pub struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
    index: Mutex<Index>,
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// Hashes the file at `path` with BLAKE3.
pub fn file_id(path: &Path) -> io::Result<ObjectId> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(ObjectId::from_bytes(*h.finalize().as_bytes()))
}

impl DiskCache {
    /// Opens the cache in `root`, creating it, removing leftover temporary files and
    /// indexing what is there; then evicts down to `max_bytes`.
    pub fn open(root: &Path, max_bytes: u64) -> io::Result<Self> {
        let tmp = root.join("tmp");
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;
        let objects = root.join("objects");
        std::fs::create_dir_all(&objects)?;
        let mut found: Vec<(SystemTime, Key, u64)> = Vec::new();
        for depot_dir in std::fs::read_dir(&objects)? {
            let depot_dir = depot_dir?;
            let Some(depot) = depot_dir.file_name().to_str().and_then(unhex) else {
                continue;
            };
            for fan in std::fs::read_dir(depot_dir.path())? {
                let fan = fan?;
                if !fan.file_type()?.is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(fan.path())? {
                    let f = f?;
                    let name = f.file_name();
                    let Some(name) = name.to_str() else { continue };
                    let (id, kind) = match name.strip_suffix(".chunks") {
                        Some(id) => (id, Kind::ChunkList),
                        None => (name, Kind::Content),
                    };
                    let Ok(id) = id.parse::<ObjectId>() else {
                        continue;
                    };
                    let meta = f.metadata()?;
                    let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push((
                        used,
                        Key {
                            depot: depot.clone(),
                            id,
                            kind,
                        },
                        meta.len(),
                    ));
                }
            }
        }
        found.sort_by_key(|(t, _, _)| *t);
        let mut index = Index {
            entries: HashMap::new(),
            total: 0,
            tick: 0,
        };
        for (_, key, size) in found {
            index.tick += 1;
            index.total += size;
            index.entries.insert(
                key,
                Entry {
                    size,
                    last_use: index.tick,
                },
            );
        }
        let cache = Self {
            root: root.to_owned(),
            max_bytes,
            index: Mutex::new(index),
        };
        cache.evict();
        Ok(cache)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Where the entry `key` is kept.
    pub fn path(&self, key: &Key) -> PathBuf {
        let id = key.id.to_string();
        let name = match key.kind {
            Kind::Content => id.clone(),
            Kind::ChunkList => format!("{id}.chunks"),
        };
        self.root
            .join("objects")
            .join(hex(&key.depot))
            .join(&id[..2])
            .join(name)
    }

    /// A fresh temporary path for a file about to be added.
    pub fn tmp_path(&self) -> PathBuf {
        let n: u64 = rand::random();
        self.root.join("tmp").join(format!("{n:016x}"))
    }

    /// The path of `key` if the cache holds it, marking it used.
    pub fn lookup(&self, key: &Key) -> Option<PathBuf> {
        {
            let mut g = self.lock();
            let idx = &mut *g;
            idx.tick += 1;
            let tick = idx.tick;
            idx.entries.get_mut(key)?.last_use = tick;
        }
        let path = self.path(key);
        // The modification time orders the index after a restart.
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(path)
    }

    /// Moves the file at `tmp` into the cache as `key`, then evicts down to the cap.
    pub fn insert(&self, key: Key, tmp: &Path) -> io::Result<PathBuf> {
        let size = std::fs::metadata(tmp)?.len();
        let path = self.path(&key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::rename(tmp, &path)?;
        {
            let mut g = self.lock();
            let idx = &mut *g;
            idx.tick += 1;
            let tick = idx.tick;
            if let Some(old) = idx.entries.insert(
                key,
                Entry {
                    size,
                    last_use: tick,
                },
            ) {
                idx.total -= old.size;
            }
            idx.total += size;
        }
        self.evict();
        Ok(path)
    }

    /// Drops `key`, file and all.
    pub fn remove(&self, key: &Key) {
        let had = {
            let mut g = self.lock();
            match g.entries.remove(key) {
                Some(e) => {
                    g.total -= e.size;
                    true
                }
                None => false,
            }
        };
        if had {
            if let Err(e) = std::fs::remove_file(self.path(key)) {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, "cannot remove a cache entry");
                }
            }
        }
    }

    /// Removes least recently used entries until the cache fits its cap.
    fn evict(&self) {
        let victims: Vec<Key> = {
            let mut g = self.lock();
            if g.total <= self.max_bytes {
                return;
            }
            let mut order: Vec<(u64, Key, u64)> = g
                .entries
                .iter()
                .map(|(k, e)| (e.last_use, k.clone(), e.size))
                .collect();
            order.sort_by_key(|(t, _, _)| *t);
            let mut out = Vec::new();
            for (_, k, size) in order {
                if g.total <= self.max_bytes {
                    break;
                }
                g.entries.remove(&k);
                g.total -= size;
                out.push(k);
            }
            out
        };
        for k in victims {
            if let Err(e) = std::fs::remove_file(self.path(&k)) {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, "cannot evict a cache entry");
                }
            }
        }
    }

    /// Bytes and entries held.
    pub fn size(&self) -> CacheSize {
        let g = self.lock();
        CacheSize {
            bytes: g.total,
            entries: g.entries.len() as u64,
        }
    }

    /// The cap in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn add(cache: &DiskCache, depot: &str, data: &[u8]) -> Key {
        let id = lode_core::blob_id(data).unwrap();
        let tmp = cache.tmp_path();
        std::fs::write(&tmp, data).unwrap();
        let key = Key::content(depot, id);
        cache.insert(key.clone(), &tmp).unwrap();
        key
    }

    #[test]
    fn evicts_least_recently_used_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::open(dir.path(), 25).unwrap();
        let a = add(&cache, "game", &[1u8; 10]);
        let b = add(&cache, "game", &[2u8; 10]);
        assert!(cache.lookup(&a).is_some());
        let c = add(&cache, "game/x", &[3u8; 10]);
        assert!(cache.lookup(&b).is_none(), "b was least recently used");
        assert!(cache.lookup(&a).is_some() && cache.lookup(&c).is_some());
        assert_eq!(cache.size().bytes, 20);
        drop(cache);
        std::fs::write(dir.path().join("tmp").join("junk"), b"x").unwrap();
        let cache = DiskCache::open(dir.path(), 25).unwrap();
        assert_eq!(
            cache.size(),
            CacheSize {
                bytes: 20,
                entries: 2
            }
        );
        assert!(!dir.path().join("tmp").join("junk").exists());
        assert_eq!(file_id(&cache.lookup(&c).unwrap()).unwrap(), c.id);
        cache.remove(&a);
        assert_eq!(cache.size().entries, 1);
    }
}
