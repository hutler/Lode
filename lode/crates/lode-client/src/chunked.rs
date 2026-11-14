//! Chunked transfer of large files (ARCHITECTURE §3.5, §10.6).
//!
//! A file at or above the server's chunking threshold is cut with the shared chunker. On
//! upload only the chunks the server lacks are sent, then the chunk list; on download only
//! the chunks not found locally are fetched. Either resumes after an interruption: the
//! server keeps the chunks already uploaded, and downloaded chunks wait in a directory the
//! caller names until the file is put together.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use lode_core::{ChunkList, ChunkingHasher, ErrorCode, ObjectId, CHUNK_THRESHOLD};
use lode_proto::auth::ChunkingInfo;
use lode_proto::depot::PutBlobResponse;
use reqwest::Method;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::{Error, Result};
use crate::{decode, Client, Retry};

/// Chunks sent or fetched at once for one file.
const PARALLEL_CHUNKS: usize = 4;

/// Bytes moved over the network and bytes that did not need to be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Content bytes sent or received.
    pub network: u64,
    /// Content bytes the other side or a local copy already had.
    pub reused: u64,
}

impl std::ops::AddAssign for Transfer {
    fn add_assign(&mut self, o: Self) {
        self.network += o.network;
        self.reused += o.reused;
    }
}

/// Somewhere downloaded chunks can be kept and found again, such as the shared download
/// cache. Errors are reported by the caller as warnings, never as failures.
pub trait ChunkCache: Send + Sync {
    /// Copies the chunk `id` to `dest` if the cache has it (checked against its id).
    fn copy_out(&self, id: &ObjectId, dest: &Path) -> std::io::Result<bool>;
    /// Keeps a copy of the chunk at `src`.
    fn store(&self, id: &ObjectId, src: &Path) -> std::io::Result<()>;
}

/// Where [`Client::download_file`] may find chunks before fetching them.
#[derive(Default)]
pub struct DownloadOpts {
    /// Directory for chunks while a file is fetched, kept across runs so a rerun resumes.
    /// Without one, a temporary directory next to the destination is used and removed.
    pub chunk_dir: Option<PathBuf>,
    /// Local files that may hold some of the chunks, such as the version of the file the
    /// workspace has now. They are cut with the shared chunker; whatever they hold that
    /// matches a chunk id is used.
    pub sources: Vec<PathBuf>,
    /// A cache of chunks shared by workspaces.
    pub cache: Option<Arc<dyn ChunkCache>>,
    /// Called for warnings about the cache.
    pub warn: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

/// Test hook (T151): with `LODE_TEST_EXIT_AFTER_CHUNKS=n` the process exits with code 86
/// after `n` chunks were transferred, as if killed.
fn chunk_done(count: &std::sync::atomic::AtomicU64) {
    let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if std::env::var("LODE_TEST_EXIT_AFTER_CHUNKS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|limit| n >= limit)
    {
        std::process::exit(86);
    }
}

/// Cuts the file at `path` with the shared chunker on a blocking thread.
async fn cut_file(path: &Path) -> Result<lode_core::Chunked> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<lode_core::Chunked> {
        let mut f = std::fs::File::open(path)?;
        let mut h = ChunkingHasher::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(h.finish())
    })
    .await
    .map_err(|e| Error::Io(std::io::Error::other(e)))?
    .map_err(Error::Io)
}

fn offsets(list: &ChunkList) -> Vec<u64> {
    let mut at = 0;
    list.chunks
        .iter()
        .map(|c| {
            let o = at;
            at += u64::from(c.size);
            o
        })
        .collect()
}

impl Client {
    /// The server's chunking parameters, asked once per client. `None` when the server does
    /// not chunk or cuts files differently from this build, and then files go whole.
    pub async fn chunking(&self) -> Option<ChunkingInfo> {
        // Test hook (T151): behave like a client of a server before 0.3.
        if std::env::var_os("LODE_TEST_NO_CHUNKING").is_some() {
            return None;
        }
        if let Some(c) = self.chunking.get() {
            return c.clone();
        }
        let c = match self.info().await {
            Ok(info) => info.chunking.filter(ChunkingInfo::is_current),
            Err(e) => {
                tracing::debug!(error = %e, "no server info; sending files whole");
                return None;
            }
        };
        let _ = self.chunking.set(c.clone());
        c
    }

    /// Whether a file of `size` bytes goes in chunks to this server.
    pub async fn chunks_file(&self, size: u64) -> bool {
        size >= CHUNK_THRESHOLD && self.chunking().await.is_some()
    }

    /// `GET /depots/{d}/blobs/{id}/chunks`: the chunk list of a chunked file, or `None` when
    /// the server keeps the file whole.
    pub async fn chunk_list(&self, depot: &str, id: &ObjectId) -> Result<Option<ChunkList>> {
        let url = self.url(&["depots", depot, "blobs", &id.to_string(), "chunks"])?;
        let resp = match self
            .send(Retry::Yes, || Ok(self.request(Method::GET, url.clone())))
            .await
        {
            Ok(r) => r,
            Err(Error::Api(a))
                if a.known_code() == Some(ErrorCode::NotFound)
                    && a.details.get("reason").and_then(|r| r.as_str()) == Some("not_chunked") =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };
        let bytes = resp.bytes().await.map_err(Error::Transport)?;
        let list = ChunkList::decode(&bytes)
            .map_err(|e| Error::Protocol(format!("the server sent a bad chunk list: {e}")))?;
        Ok(Some(list))
    }

    /// `PUT /depots/{d}/blobs/{id}/chunks`: stores the file `id` as `list`. Every chunk must
    /// be uploaded first.
    pub async fn put_chunk_list(
        &self,
        depot: &str,
        id: &ObjectId,
        list: &ChunkList,
    ) -> Result<PutBlobResponse> {
        let url = self.url(&["depots", depot, "blobs", &id.to_string(), "chunks"])?;
        let body = list.encode();
        let resp = self
            .send(Retry::Yes, || {
                Ok(self.request(Method::PUT, url.clone()).body(body.clone()))
            })
            .await?;
        decode(resp).await
    }

    /// Uploads `len` bytes of the file at `path` from `offset` as blob `id`, reopening the
    /// file for each attempt.
    async fn put_blob_range(
        &self,
        depot: &str,
        id: &ObjectId,
        path: &Path,
        offset: u64,
        len: u64,
    ) -> Result<PutBlobResponse> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut file = tokio::fs::File::open(path).await?;
            file.seek(SeekFrom::Start(offset)).await?;
            match self.put_blob(depot, id, file.take(len)).await {
                Err(e) if e.is_transient() && attempt < self.retry.max_attempts => {
                    tokio::time::sleep(self.retry.delay(attempt, None)).await;
                }
                other => return other,
            }
        }
    }

    /// Uploads the file at `path` as blob `id`: in chunks when it is large and the server
    /// chunks, sending only the chunks the server lacks; otherwise whole. A file whose
    /// content no longer hashes to `id` fails with [`Error::HashMismatch`] (or the server's
    /// `hash_mismatch`).
    pub async fn upload_file(
        &self,
        depot: &str,
        id: &ObjectId,
        path: &Path,
        size: u64,
    ) -> Result<Transfer> {
        if !self.chunks_file(size).await {
            self.put_blob_file(depot, id, path).await?;
            return Ok(Transfer {
                network: size,
                reused: 0,
            });
        }
        let cut = cut_file(path).await?;
        if cut.id != *id {
            return Err(Error::HashMismatch {
                expected: *id,
                actual: cut.id,
            });
        }
        let Some(list) = cut.chunks else {
            self.put_blob_file(depot, id, path).await?;
            return Ok(Transfer {
                network: cut.size,
                reused: 0,
            });
        };
        let at = offsets(&list);
        let mut sent = 0u64;
        let count = std::sync::atomic::AtomicU64::new(0);
        // A garbage collection may remove an unused chunk between the check and the list;
        // the second round sends it again.
        for round in 0..2 {
            let ids: Vec<ObjectId> = {
                let mut seen = HashSet::new();
                list.chunks
                    .iter()
                    .filter(|c| seen.insert(c.id))
                    .map(|c| c.id)
                    .collect()
            };
            let lacking: HashSet<ObjectId> = self
                .missing_objects(depot, &ids)
                .await?
                .into_iter()
                .collect();
            let mut todo = Vec::new();
            let mut queued = HashSet::new();
            for (i, c) in list.chunks.iter().enumerate() {
                if lacking.contains(&c.id) && queued.insert(c.id) {
                    todo.push((c.id, at[i], u64::from(c.size)));
                }
            }
            let mut uploads = futures::stream::iter(todo.into_iter().map(|(cid, off, len)| {
                let count = &count;
                async move {
                    self.put_blob_range(depot, &cid, path, off, len).await?;
                    chunk_done(count);
                    Ok::<u64, Error>(len)
                }
            }))
            .buffer_unordered(PARALLEL_CHUNKS);
            while let Some(r) = uploads.next().await {
                sent += r?;
            }
            drop(uploads);
            match self.put_chunk_list(depot, id, &list).await {
                Ok(_) => break,
                Err(Error::Api(a))
                    if round == 0 && a.known_code() == Some(ErrorCode::MissingObjects) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Transfer {
            network: sent,
            reused: cut.size.saturating_sub(sent),
        })
    }

    /// Downloads blob `id` of `size` bytes to `dest` (written beside it, checked, then
    /// renamed into place). A large chunked file is put together from chunks found in
    /// `opts` and only the others are fetched; anything else is downloaded whole.
    pub async fn download_file(
        &self,
        depot: &str,
        id: &ObjectId,
        dest: &Path,
        size: u64,
        opts: &DownloadOpts,
    ) -> Result<Transfer> {
        let whole = |n: u64| Transfer {
            network: n,
            reused: 0,
        };
        if !self.chunks_file(size).await {
            return self
                .download_blob_max(depot, id, dest, size)
                .await
                .map(whole);
        }
        let Some(list) = self.chunk_list(depot, id).await? else {
            return self
                .download_blob_max(depot, id, dest, size)
                .await
                .map(whole);
        };
        if list.total_size() != size {
            return Err(Error::Protocol(format!(
                "the chunks of {id} add up to {} bytes, not {size}",
                list.total_size()
            )));
        }
        let dir_parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let scratch;
        let chunk_dir = match &opts.chunk_dir {
            Some(d) => d.clone(),
            None => {
                scratch = tempfile::Builder::new()
                    .prefix(".lode-chunks-")
                    .tempdir_in(&dir_parent)?;
                scratch.path().to_path_buf()
            }
        };
        tokio::fs::create_dir_all(&chunk_dir).await?;
        let warn = |m: String| {
            if let Some(w) = &opts.warn {
                w(m);
            }
        };

        // Chunks the local sources hold, by id.
        let wanted: HashSet<ObjectId> = list.chunks.iter().map(|c| c.id).collect();
        let mut local: HashMap<ObjectId, (PathBuf, u64, u64)> = HashMap::new();
        for src in &opts.sources {
            if !src.is_file() {
                continue;
            }
            match cut_file(src).await {
                Ok(c) => {
                    if let Some(l) = c.chunks {
                        for (c, off) in l.chunks.iter().zip(offsets(&l)) {
                            if wanted.contains(&c.id) {
                                local
                                    .entry(c.id)
                                    .or_insert((src.clone(), off, u64::from(c.size)));
                            }
                        }
                    }
                }
                Err(e) => warn(format!("{}: {e}", src.display())),
            }
        }

        let chunk_path = |cid: &ObjectId| chunk_dir.join(cid.to_hex());
        let mut transfer = Transfer::default();
        let mut fetch = Vec::new();
        let mut seen = HashSet::new();
        for c in &list.chunks {
            if !seen.insert(c.id) {
                continue;
            }
            let p = chunk_path(&c.id);
            // A chunk left by an interrupted run, or found in a local file or the cache.
            if verify_file(&p, &c.id).await? {
                transfer.reused += u64::from(c.size);
                continue;
            }
            if let Some((src, off, len)) = local.get(&c.id) {
                if copy_range(src, *off, *len, &p, &c.id).await? {
                    transfer.reused += u64::from(c.size);
                    continue;
                }
            }
            if let Some(cache) = &opts.cache {
                match cache.copy_out(&c.id, &p) {
                    Ok(true) => {
                        transfer.reused += u64::from(c.size);
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => warn(format!("shared cache: {e}")),
                }
            }
            fetch.push(*c);
        }
        let count = std::sync::atomic::AtomicU64::new(0);
        let mut downloads = futures::stream::iter(fetch.into_iter().map(|c| {
            let p = chunk_path(&c.id);
            let count = &count;
            async move {
                self.download_blob_max(depot, &c.id, &p, u64::from(c.size))
                    .await?;
                chunk_done(count);
                Ok::<_, Error>((c, p))
            }
        }))
        .buffer_unordered(PARALLEL_CHUNKS);
        while let Some(r) = downloads.next().await {
            let (c, p) = r?;
            transfer.network += u64::from(c.size);
            if let Some(cache) = &opts.cache {
                if let Err(e) = cache.store(&c.id, &p) {
                    warn(format!("shared cache: {e}"));
                }
            }
        }
        drop(downloads);

        // Put the file together beside its destination, checking the whole id.
        let tmp = tempfile::Builder::new()
            .prefix(".lode-download-")
            .tempfile_in(&dir_parent)?;
        {
            let mut out = tokio::fs::File::from_std(tmp.as_file().try_clone()?);
            let mut hasher = blake3::Hasher::new();
            let mut buf = vec![0u8; 256 * 1024];
            for c in &list.chunks {
                let mut f = tokio::fs::File::open(chunk_path(&c.id)).await?;
                loop {
                    let n = f.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    out.write_all(&buf[..n]).await?;
                }
            }
            out.flush().await?;
            out.sync_all().await?;
            let actual = ObjectId::from_bytes(*hasher.finalize().as_bytes());
            if actual != *id {
                return Err(Error::HashMismatch {
                    expected: *id,
                    actual,
                });
            }
        }
        tmp.persist(dest).map_err(|e| Error::Io(e.error))?;
        for c in &list.chunks {
            let _ = tokio::fs::remove_file(chunk_path(&c.id)).await;
        }
        Ok(transfer)
    }
}

/// Whether the file at `p` exists and hashes to `id`. A damaged one is removed.
async fn verify_file(p: &Path, id: &ObjectId) -> Result<bool> {
    let p = p.to_path_buf();
    let id = *id;
    tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        let f = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if lode_core::blob_id(f)? == id {
            return Ok(true);
        }
        let _ = std::fs::remove_file(&p);
        Ok(false)
    })
    .await
    .map_err(|e| Error::Io(std::io::Error::other(e)))?
    .map_err(Error::Io)
}

/// Copies `len` bytes at `off` of `src` to `dest` if they hash to `id` (the source may have
/// changed since it was cut).
async fn copy_range(src: &Path, off: u64, len: u64, dest: &Path, id: &ObjectId) -> Result<bool> {
    let (src, dest, id) = (src.to_path_buf(), dest.to_path_buf(), *id);
    tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        let mut f = std::fs::File::open(&src)?;
        f.seek(SeekFrom::Start(off))?;
        let mut bytes = Vec::with_capacity(len as usize);
        f.take(len).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != len || lode_core::blob_id(&bytes[..])? != id {
            return Ok(false);
        }
        let tmp = dest.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &dest)?;
        Ok(true)
    })
    .await
    .map_err(|e| Error::Io(std::io::Error::other(e)))?
    .map_err(Error::Io)
}
