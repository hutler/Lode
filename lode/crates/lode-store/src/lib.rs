//! Content-addressed object storage for the Lode server (ARCHITECTURE §3.3–3.4).
//!
//! Objects are immutable files (or S3 objects) named by their id. Content is streamed in and out with fixed
//! buffers, verified against its id on write, and never overwritten.

#![forbid(unsafe_code)]

mod chunked;
mod encode;
mod error;
mod fs;
pub mod s3;
mod zstd_reader;

use std::pin::Pin;
use std::time::SystemTime;

use futures::stream::BoxStream;
use lode_core::{Codec, DepotId, ObjectId, ObjectKind};
use tokio::io::AsyncRead;

pub use chunked::{ChunkListOutcome, ChunkedStore};
pub use error::{Result, StoreError};
pub use fs::{FsBlobStore, FsBlobStoreConfig};
pub use s3::{S3BlobStore, S3Config};

/// Result of a `put`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// The object was written by this call.
    Created,
    /// The object already existed; nothing was written.
    AlreadyPresent,
}

/// Options for a `put`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PutOptions {
    /// The raw length the caller announced, such as a `Content-Length`. The put fails at
    /// once with `too_large` if it exceeds the store's cap, and fails if the stream turns
    /// out longer or shorter.
    pub expected_len: Option<u64>,
}

/// A boxed byte stream.
pub type ByteStream = Pin<Box<dyn AsyncRead + Send>>;

/// An object being read: its kind, decoded length and a stream of decoded bytes. The stream
/// never yields more than `raw_len` bytes; if the stored payload decodes to more or fewer,
/// reading fails with an `InvalidData` I/O error that wraps a [`StoreError::Corrupt`].
pub struct ObjectReader {
    /// Object kind from the header.
    pub kind: ObjectKind,
    /// Decoded length from the header.
    pub raw_len: u64,
    /// Decoded content.
    pub reader: ByteStream,
}

/// The stored form of an object, for serving `Accept-Encoding: zstd` without recompressing.
pub struct StoredReader {
    /// Object kind from the header.
    pub kind: ObjectKind,
    /// Codec of the payload.
    pub codec: Codec,
    /// Decoded length from the header.
    pub raw_len: u64,
    /// Length of the payload as stored.
    pub stored_len: u64,
    /// The payload exactly as stored, without the header.
    pub reader: ByteStream,
}

/// Object storage, namespaced per depot.
///
/// The content stream is passed as `&mut dyn AsyncRead` rather than a generic so the trait
/// stays usable as `dyn BlobStore`.
#[async_trait::async_trait]
pub trait BlobStore: Send + Sync {
    /// True if the object exists.
    async fn contains(&self, depot: DepotId, id: &ObjectId) -> Result<bool>;

    /// Streams raw (uncompressed) content in, verifies that it hashes to `expect` for its
    /// kind, and stores it.
    async fn put(
        &self,
        depot: DepotId,
        kind: ObjectKind,
        expect: &ObjectId,
        raw: &mut (dyn AsyncRead + Send + Unpin),
        opts: PutOptions,
    ) -> Result<PutOutcome>;

    /// Opens an object for reading decoded content.
    async fn get(&self, depot: DepotId, id: &ObjectId) -> Result<ObjectReader>;

    /// Opens an object's payload exactly as stored.
    async fn get_stored(&self, depot: DepotId, id: &ObjectId) -> Result<StoredReader>;

    /// Removes an object. Only garbage collection calls this.
    async fn delete(&self, depot: DepotId, id: &ObjectId) -> Result<()>;

    /// Every object of the depot with its modification time. Files that are not objects
    /// are skipped.
    fn list(&self, depot: DepotId) -> BoxStream<'_, Result<(ObjectId, SystemTime)>>;

    /// Removes every object of the depot, for a depot purged for good. Running it again
    /// after an interruption finishes the job. The default deletes object by object.
    async fn delete_depot(&self, depot: DepotId) -> Result<()> {
        use futures::StreamExt;
        let mut ids = Vec::new();
        let mut objects = self.list(depot);
        while let Some(item) = objects.next().await {
            ids.push(item?.0);
        }
        drop(objects);
        for id in ids {
            match self.delete(depot, &id).await {
                Ok(()) | Err(StoreError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
