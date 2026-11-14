//! The stored object format of ARCHITECTURE §3.3, written to a local file: the content is
//! hashed while it streams in, compressed with zstd when that saves at least 3 %, and checked
//! against the expected id and length. Shared by the filesystem and S3 stores.
//!
//! **Compression heuristic.** zstd level 3 is used unless it saves less than 3 %. Content up
//! to 1 MiB is compressed in one go and the decision uses its exact size. For longer content
//! the decision is made once, on the first 1 MiB, and then kept for the rest of the stream,
//! so large incompressible files are never compressed twice.

use std::io::{self, SeekFrom, Write};

use lode_core::{Codec, ObjectHeader, ObjectId, ObjectKind, TREE_ID_CONTEXT};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::{Result, StoreError};

pub(crate) const CHUNK: usize = 64 * 1024;
const PROBE: usize = 1024 * 1024;
const ZSTD_LEVEL: i32 = 3;

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

/// The size cap and the announced length of one put.
pub(crate) struct Limits {
    pub(crate) cap: u64,
    pub(crate) expected: Option<u64>,
}

impl Limits {
    /// Refuses an announced length over the cap before anything is read.
    pub(crate) fn precheck(&self) -> Result<()> {
        if let Some(exp) = self.expected {
            if exp > self.cap {
                return Err(StoreError::TooLarge { limit: self.cap });
            }
        }
        Ok(())
    }

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

/// Writes `raw` to `file` (new and empty) in the stored format, checks it hashes to `expect`
/// and syncs the file. Returns the header written.
pub(crate) async fn encode(
    file: &mut File,
    kind: ObjectKind,
    expect: &ObjectId,
    raw: &mut (dyn AsyncRead + Send + Unpin),
    limits: &Limits,
) -> Result<ObjectHeader> {
    let mut hasher = Hasher::new(kind)?;
    let opts_expected = limits.expected;
    // Placeholder; the real header is written once the codec and length are known.
    file.write_all(&[0u8; ObjectHeader::LEN]).await?;

    // Probe: read up to 1 MiB to choose the codec.
    let mut probe = Vec::with_capacity(PROBE.min(opts_expected.unwrap_or(PROBE as u64) as usize));
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

    if let Some(exp) = opts_expected {
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
    Ok(header)
}
