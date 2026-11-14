//! Chunked files on top of any [`BlobStore`] (ARCHITECTURE §3.5).
//!
//! [`ChunkedStore`] wraps a store so that every reader of file content sees a chunked file
//! as its whole content: `get` of a chunk list object streams its chunks one after the
//! other. Writers of chunk lists go through [`ChunkedStore::put_chunk_list`], which checks
//! that the chunks exist and that together they hash to the file's id.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use lode_core::{ChunkList, DepotId, ObjectId, ObjectKind};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use crate::{BlobStore, ObjectReader, PutOptions, PutOutcome, Result, StoreError, StoredReader};

/// Largest chunk list payload read into memory: about 1.8 million chunks, so files of
/// several terabytes.
const MAX_LIST_BYTES: u64 = 64 * 1024 * 1024;

/// Outcome of [`ChunkedStore::put_chunk_list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkListOutcome {
    /// Stored now (`Created`) or already there, whole or chunked (`AlreadyPresent`).
    Stored(PutOutcome),
    /// These chunks are not stored; nothing was written.
    Missing(Vec<ObjectId>),
}

/// A [`BlobStore`] that assembles chunked files on read.
#[derive(Clone)]
pub struct ChunkedStore {
    inner: Arc<dyn BlobStore>,
    max_file_size: u64,
}

impl ChunkedStore {
    /// Wraps `inner`. A chunk list whose file would exceed `max_file_size` is refused with
    /// `too_large`.
    pub fn new(inner: Arc<dyn BlobStore>, max_file_size: u64) -> Self {
        Self {
            inner,
            max_file_size,
        }
    }

    /// The wrapped store, which shows chunk lists as they are stored.
    pub fn inner(&self) -> &Arc<dyn BlobStore> {
        &self.inner
    }

    /// The chunk list stored under `id`, or `None` when the object is stored whole (or is a
    /// tree). `NotFound` when there is no object.
    pub async fn chunk_list(&self, depot: DepotId, id: &ObjectId) -> Result<Option<ChunkList>> {
        let obj = self.inner.get(depot, id).await?;
        if obj.kind != ObjectKind::ChunkList {
            return Ok(None);
        }
        read_list(id, obj).await.map(Some)
    }

    /// Stores `list` as the chunked form of the file `id`. Every chunk must be stored as a
    /// blob, and the chunks together must hash to `id`.
    pub async fn put_chunk_list(
        &self,
        depot: DepotId,
        id: &ObjectId,
        list: &ChunkList,
    ) -> Result<ChunkListOutcome> {
        if self.inner.contains(depot, id).await? {
            return Ok(ChunkListOutcome::Stored(PutOutcome::AlreadyPresent));
        }
        if list.total_size() > self.max_file_size {
            return Err(StoreError::TooLarge {
                limit: self.max_file_size,
            });
        }
        let mut missing = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for c in &list.chunks {
            if !seen.insert(c.id) {
                continue;
            }
            match self.inner.get_stored(depot, &c.id).await {
                Ok(s) if s.kind == ObjectKind::Blob && s.raw_len == u64::from(c.size) => {}
                Ok(s) if s.kind == ObjectKind::Blob => {
                    return Err(StoreError::LengthMismatch {
                        expected: u64::from(c.size),
                        actual: s.raw_len,
                    })
                }
                Ok(_) | Err(StoreError::NotFound(_)) => missing.push(c.id),
                Err(e) => return Err(e),
            }
        }
        if !missing.is_empty() {
            return Ok(ChunkListOutcome::Missing(missing));
        }
        // Never trust the client: the chunks must hash to the file id.
        let mut reader = ChunkReader::new(self.inner.clone(), depot, list.clone(), 0);
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let actual = ObjectId::from_bytes(*hasher.finalize().as_bytes());
        if actual != *id {
            return Err(StoreError::HashMismatch {
                expected: *id,
                actual,
            });
        }
        let bytes = list.encode();
        let len = bytes.len() as u64;
        let outcome = self
            .inner
            .put(
                depot,
                ObjectKind::ChunkList,
                id,
                &mut &bytes[..],
                PutOptions {
                    expected_len: Some(len),
                },
            )
            .await?;
        Ok(ChunkListOutcome::Stored(outcome))
    }

    /// Opens a file's content at byte `offset`. For a chunked file only the chunks from the
    /// offset on are read; `raw_len` is the length of the whole file.
    pub async fn get_from(
        &self,
        depot: DepotId,
        id: &ObjectId,
        offset: u64,
    ) -> Result<ObjectReader> {
        let obj = self.inner.get(depot, id).await?;
        if obj.kind == ObjectKind::ChunkList {
            let list = read_list(id, obj).await?;
            let raw_len = list.total_size();
            return Ok(ObjectReader {
                kind: ObjectKind::Blob,
                raw_len,
                reader: Box::pin(ChunkReader::new(
                    self.inner.clone(),
                    depot,
                    list,
                    offset.min(raw_len),
                )),
            });
        }
        let mut obj = obj;
        if offset > 0 {
            let skip = offset.min(obj.raw_len);
            tokio::io::copy(&mut (&mut obj.reader).take(skip), &mut tokio::io::sink()).await?;
        }
        Ok(obj)
    }
}

async fn read_list(id: &ObjectId, mut obj: ObjectReader) -> Result<ChunkList> {
    if obj.raw_len > MAX_LIST_BYTES {
        return Err(StoreError::Corrupt {
            id: *id,
            reason: "chunk list is too long".into(),
        });
    }
    let mut bytes = Vec::with_capacity(obj.raw_len as usize);
    obj.reader.read_to_end(&mut bytes).await?;
    ChunkList::decode(&bytes).map_err(|e| StoreError::Corrupt {
        id: *id,
        reason: e.to_string(),
    })
}

#[async_trait::async_trait]
impl BlobStore for ChunkedStore {
    async fn contains(&self, depot: DepotId, id: &ObjectId) -> Result<bool> {
        self.inner.contains(depot, id).await
    }

    async fn put(
        &self,
        depot: DepotId,
        kind: ObjectKind,
        expect: &ObjectId,
        raw: &mut (dyn AsyncRead + Send + Unpin),
        opts: PutOptions,
    ) -> Result<PutOutcome> {
        if kind == ObjectKind::ChunkList {
            // Chunk lists are checked against their chunks; only put_chunk_list does that.
            return Err(StoreError::UnsupportedKind(kind));
        }
        self.inner.put(depot, kind, expect, raw, opts).await
    }

    async fn get(&self, depot: DepotId, id: &ObjectId) -> Result<ObjectReader> {
        self.get_from(depot, id, 0).await
    }

    /// The stored form, unchanged: a chunked file shows as its chunk list.
    async fn get_stored(&self, depot: DepotId, id: &ObjectId) -> Result<StoredReader> {
        self.inner.get_stored(depot, id).await
    }

    async fn delete(&self, depot: DepotId, id: &ObjectId) -> Result<()> {
        self.inner.delete(depot, id).await
    }

    fn list(&self, depot: DepotId) -> BoxStream<'_, Result<(ObjectId, SystemTime)>> {
        self.inner.list(depot)
    }
}

/// Streams a chunked file's chunks in order, starting at an offset.
struct ChunkReader {
    store: Arc<dyn BlobStore>,
    depot: DepotId,
    list: ChunkList,
    /// Next chunk to open.
    next: usize,
    /// Bytes to skip at the start of the next chunk opened.
    skip: u64,
    state: State,
}

enum State {
    Idle,
    Opening(BoxFuture<'static, Result<ObjectReader>>, u32),
    Reading(Pin<Box<dyn AsyncRead + Send>>, u64),
    Done,
}

impl ChunkReader {
    fn new(store: Arc<dyn BlobStore>, depot: DepotId, list: ChunkList, offset: u64) -> Self {
        let mut next = 0;
        let mut skip = offset;
        while next < list.chunks.len() && skip >= u64::from(list.chunks[next].size) {
            skip -= u64::from(list.chunks[next].size);
            next += 1;
        }
        Self {
            store,
            depot,
            list,
            next,
            skip,
            state: State::Idle,
        }
    }
}

fn corrupt_io(id: ObjectId, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        StoreError::Corrupt {
            id,
            reason: reason.to_owned(),
        },
    )
}

impl AsyncRead for ChunkReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            match &mut this.state {
                State::Done => return Poll::Ready(Ok(())),
                State::Idle => {
                    let Some(c) = this.list.chunks.get(this.next).copied() else {
                        this.state = State::Done;
                        continue;
                    };
                    this.next += 1;
                    let store = this.store.clone();
                    let depot = this.depot;
                    let fut = Box::pin(async move { store.get(depot, &c.id).await });
                    this.state = State::Opening(fut, c.size);
                }
                State::Opening(fut, size) => {
                    let size = *size;
                    let obj = match fut.as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(StoreError::NotFound(id))) => {
                            this.state = State::Done;
                            return Poll::Ready(Err(corrupt_io(
                                id,
                                "a chunk of this file is missing",
                            )));
                        }
                        Poll::Ready(Err(e)) => {
                            this.state = State::Done;
                            return Poll::Ready(Err(io::Error::other(e)));
                        }
                        Poll::Ready(Ok(obj)) => obj,
                    };
                    let id = this.list.chunks[this.next - 1].id;
                    if obj.kind != ObjectKind::Blob || obj.raw_len != u64::from(size) {
                        this.state = State::Done;
                        return Poll::Ready(Err(corrupt_io(
                            id,
                            "chunk differs from the chunk list",
                        )));
                    }
                    this.state = State::Reading(obj.reader, std::mem::take(&mut this.skip));
                }
                State::Reading(reader, skip) => {
                    if *skip > 0 {
                        let mut scratch = [0u8; 8192];
                        let want = (*skip).min(scratch.len() as u64) as usize;
                        let mut rb = ReadBuf::new(&mut scratch[..want]);
                        match reader.as_mut().poll_read(cx, &mut rb) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                            Poll::Ready(Ok(())) if rb.filled().is_empty() => {
                                this.state = State::Idle;
                            }
                            Poll::Ready(Ok(())) => *skip -= rb.filled().len() as u64,
                        }
                        continue;
                    }
                    let before = buf.filled().len();
                    match reader.as_mut().poll_read(cx, buf) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Ready(Ok(())) => {
                            if buf.filled().len() > before || buf.remaining() == 0 {
                                return Poll::Ready(Ok(()));
                            }
                            this.state = State::Idle;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsBlobStore, FsBlobStoreConfig};
    use lode_core::ChunkingHasher;

    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    async fn setup() -> (tempfile::TempDir, ChunkedStore) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FsBlobStore::open(dir.path(), FsBlobStoreConfig::default())
            .await
            .unwrap();
        (dir, ChunkedStore::new(Arc::new(fs), u64::MAX))
    }

    fn cut(data: &[u8]) -> (ObjectId, ChunkList) {
        let mut h = ChunkingHasher::new();
        h.update(data);
        let r = h.finish();
        (r.id, r.chunks.unwrap())
    }

    async fn put_chunks(store: &ChunkedStore, d: DepotId, data: &[u8], list: &ChunkList) {
        let mut off = 0usize;
        for c in &list.chunks {
            let bytes = &data[off..off + c.size as usize];
            off += c.size as usize;
            store
                .put(
                    d,
                    ObjectKind::Blob,
                    &c.id,
                    &mut &bytes[..],
                    PutOptions::default(),
                )
                .await
                .unwrap();
        }
    }

    async fn read_all(r: ObjectReader) -> Vec<u8> {
        let mut out = Vec::new();
        let mut reader = r.reader;
        reader.read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn stores_checks_and_assembles() {
        let (_dir, store) = setup().await;
        let d = DepotId(1);
        let data = noise(11, 20 * 1024 * 1024 + 123);
        let (id, list) = cut(&data);

        // Nothing stored yet: every chunk is missing.
        let out = store.put_chunk_list(d, &id, &list).await.unwrap();
        let ChunkListOutcome::Missing(m) = out else {
            panic!("{out:?}")
        };
        assert_eq!(m.len(), list.chunks.len());

        put_chunks(&store, d, &data, &list).await;

        // A wrong id is refused after hashing.
        let wrong = ObjectId::from_bytes([7; 32]);
        assert!(matches!(
            store.put_chunk_list(d, &wrong, &list).await,
            Err(StoreError::HashMismatch { .. })
        ));
        assert_eq!(
            store.put_chunk_list(d, &id, &list).await.unwrap(),
            ChunkListOutcome::Stored(PutOutcome::Created)
        );
        assert_eq!(
            store.put_chunk_list(d, &id, &list).await.unwrap(),
            ChunkListOutcome::Stored(PutOutcome::AlreadyPresent)
        );

        let obj = store.get(d, &id).await.unwrap();
        assert_eq!(obj.kind, ObjectKind::Blob);
        assert_eq!(obj.raw_len, data.len() as u64);
        assert_eq!(read_all(obj).await, data);

        for off in [
            0u64,
            1,
            300_000,
            list.chunks[0].size as u64,
            data.len() as u64 - 5,
        ] {
            let got = read_all(store.get_from(d, &id, off).await.unwrap()).await;
            assert_eq!(got, data[off as usize..], "offset {off}");
        }
        assert_eq!(store.chunk_list(d, &id).await.unwrap(), Some(list.clone()));
        assert_eq!(
            store.get_stored(d, &id).await.unwrap().kind,
            ObjectKind::ChunkList
        );

        // Plain puts of a chunk list are refused.
        assert!(matches!(
            store
                .put(
                    d,
                    ObjectKind::ChunkList,
                    &id,
                    &mut &b""[..],
                    PutOptions::default()
                )
                .await,
            Err(StoreError::UnsupportedKind(_))
        ));
    }

    #[tokio::test]
    async fn missing_chunk_fails_the_read() {
        let (_dir, store) = setup().await;
        let d = DepotId(1);
        let data = noise(12, 17 * 1024 * 1024);
        let (id, list) = cut(&data);
        put_chunks(&store, d, &data, &list).await;
        store.put_chunk_list(d, &id, &list).await.unwrap();
        store.delete(d, &list.chunks[2].id).await.unwrap();
        let mut r = store.get(d, &id).await.unwrap().reader;
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn too_large_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FsBlobStore::open(dir.path(), FsBlobStoreConfig::default())
            .await
            .unwrap();
        let store = ChunkedStore::new(Arc::new(fs), 1024);
        let data = noise(13, 17 * 1024 * 1024);
        let (id, list) = cut(&data);
        assert!(matches!(
            store.put_chunk_list(DepotId(1), &id, &list).await,
            Err(StoreError::TooLarge { .. })
        ));
    }
}
