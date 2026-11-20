//! Blob upload and download, streamed and verified, and file content at a revision.

use std::path::Path;

use lode_core::{DepotPath, EntryKind, ObjectId};
use lode_proto::consts::{ENCODING_ZSTD, HEADER_EXECUTABLE, HEADER_ID, HEADER_KIND, MAX_BLOB_SIZE};
use lode_proto::depot::PutBlobResponse;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH};
use reqwest::{Body, Method, Response};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{Error, Result};
use crate::io::{reader_stream, StreamReader, ZstdDecodeReader, ZstdEncodeReader};
use crate::{decode, Client, Retry};

const COPY_BUF: usize = 64 * 1024;

type BoxRead = Box<dyn AsyncRead + Send + Unpin>;

/// What [`Client::fetch_content`] got.
#[derive(Debug, Clone)]
pub enum FetchedContent {
    /// The content, written to the destination; its length.
    Whole(u64),
    /// The server keeps the file in chunks: its chunk list (nothing was written).
    Chunked(lode_core::ChunkList),
}

impl Client {
    /// `PUT /depots/{d}/blobs/{id}` with content read from `reader`, zstd-compressed on the
    /// way unless compression is off. The server checks the content against `id`.
    ///
    /// Sent once: a reader cannot be rewound. [`Client::put_blob_file`] retries.
    pub async fn put_blob<R>(
        &self,
        depot: &str,
        id: &ObjectId,
        reader: R,
    ) -> Result<PutBlobResponse>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let url = self.url(&["depots", depot, "blobs", &id.to_string()])?;
        let (body, zstd) = self.upload_body(Box::new(reader))?;
        let mut cell = Some(body);
        let resp = self
            .send(Retry::No, || {
                let body = cell
                    .take()
                    .ok_or_else(|| Error::Protocol("upload body already sent".into()))?;
                let mut b = self.request(Method::PUT, url.clone()).body(body);
                if zstd {
                    b = b.header(CONTENT_ENCODING, ENCODING_ZSTD);
                }
                Ok(b)
            })
            .await?;
        decode(resp).await
    }

    /// Uploads the file at `path` as blob `id`, reopening it for each attempt.
    pub async fn put_blob_file(
        &self,
        depot: &str,
        id: &ObjectId,
        path: &Path,
    ) -> Result<PutBlobResponse> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let file = tokio::fs::File::open(path).await?;
            match self.put_blob(depot, id, file).await {
                Err(e) if e.is_transient() && attempt < self.retry.max_attempts => {
                    tokio::time::sleep(self.retry.delay(attempt, None)).await;
                }
                other => return other,
            }
        }
    }

    fn upload_body(&self, reader: BoxRead) -> Result<(Body, bool)> {
        Ok(match self.upload_level {
            Some(level) => {
                let enc = ZstdEncodeReader::new(reader, level)?;
                (Body::wrap_stream(reader_stream(enc)), true)
            }
            None => (Body::wrap_stream(reader_stream(reader)), false),
        })
    }

    /// `GET /depots/{d}/blobs/{id}` into `out`, decompressing if the server sent zstd and
    /// checking the BLAKE3 id as the bytes go by. Returns the content length.
    ///
    /// On a mismatch `out` has received the bad bytes and the call fails with
    /// [`Error::HashMismatch`]; [`Client::download_blob`] is the variant that leaves nothing
    /// behind.
    pub async fn get_blob_to<W>(&self, depot: &str, id: &ObjectId, out: &mut W) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        self.get_blob_once(depot, id, out, Retry::Yes, MAX_BLOB_SIZE)
            .await
    }

    /// [`Client::get_blob_to`] that fails as soon as more than `max_len` bytes arrive (after
    /// decompression), so a hostile server cannot make the caller hold or write more than it
    /// expects.
    pub async fn get_blob_to_max<W>(
        &self,
        depot: &str,
        id: &ObjectId,
        out: &mut W,
        max_len: u64,
    ) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        self.get_blob_once(depot, id, out, Retry::Yes, max_len)
            .await
    }

    async fn get_blob_once<W>(
        &self,
        depot: &str,
        id: &ObjectId,
        out: &mut W,
        retry: Retry,
        max_len: u64,
    ) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        self.get_blob_via(depot, id, out, retry, max_len).await.0
    }

    /// [`Client::get_blob_once`] that also tells whether the proxy answered.
    async fn get_blob_via<W>(
        &self,
        depot: &str,
        id: &ObjectId,
        out: &mut W,
        retry: Retry,
        max_len: u64,
    ) -> (Result<u64>, bool)
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        let id_text = id.to_string();
        let (resp, via_proxy) = match self
            .send_content(retry, &["depots", depot, "blobs", &id_text], |b| {
                b.header(ACCEPT_ENCODING, ENCODING_ZSTD)
            })
            .await
        {
            Ok(r) => r,
            Err(e) => return (Err(e), false),
        };
        let result = match body_reader(resp) {
            Ok(reader) => copy_verified(reader, out, id, max_len).await,
            Err(e) => Err(e),
        };
        (result, via_proxy)
    }

    /// Downloads blob `id` to `dest`: written to a temporary file next to it, verified, then
    /// renamed into place. On any failure nothing is left at `dest` (an existing file there
    /// is untouched) and the temporary file is removed. Retries transient failures with a
    /// fresh temporary file.
    pub async fn download_blob(&self, depot: &str, id: &ObjectId, dest: &Path) -> Result<u64> {
        self.download_blob_max(depot, id, dest, MAX_BLOB_SIZE).await
    }

    /// [`Client::download_blob`] that fails once more than `max_len` bytes arrive, such as
    /// the size the manifest gave.
    pub async fn download_blob_max(
        &self,
        depot: &str,
        id: &ObjectId,
        dest: &Path,
        max_len: u64,
    ) -> Result<u64> {
        let dir = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.download_once(depot, id, &dir, dest, max_len).await {
                // The proxy broke off or sent bad content: once more from the server.
                Err((e, true)) if crate::proxy::is_proxy_failure(&e) => {
                    self.proxy_failed(&e);
                    attempt -= 1;
                }
                Err((e, _)) if e.is_transient() && attempt < self.retry.max_attempts => {
                    tokio::time::sleep(self.retry.delay(attempt, None)).await;
                }
                Ok(n) => return Ok(n),
                Err((e, _)) => return Err(e),
            }
        }
    }

    /// One download attempt; the error comes with whether the proxy answered.
    async fn download_once(
        &self,
        depot: &str,
        id: &ObjectId,
        dir: &Path,
        dest: &Path,
        max_len: u64,
    ) -> std::result::Result<u64, (Error, bool)> {
        let tmp = tempfile::Builder::new()
            .prefix(".lode-download-")
            .tempfile_in(dir)
            .map_err(|e| (Error::Io(e), false))?;
        let file = tmp
            .as_file()
            .try_clone()
            .map_err(|e| (Error::Io(e), false))?;
        let mut file = tokio::fs::File::from_std(file);
        let (len, via_proxy) = self
            .get_blob_via(depot, id, &mut file, Retry::No, max_len)
            .await;
        let len = len.map_err(|e| (e, via_proxy))?;
        let local = |e: std::io::Error| (Error::Io(e), false);
        file.flush().await.map_err(local)?;
        file.sync_all().await.map_err(local)?;
        tmp.persist(dest).map_err(|e| (Error::Io(e.error), false))?;
        Ok(len)
    }

    /// Fetches content `id` for a cache (the caching proxy, T154): a file the server keeps
    /// in chunks comes back as its chunk list, anything else is written to `dest` (checked
    /// against its id, nothing left behind on failure). Large raw answers are first asked for
    /// a chunk list, so a chunked file is never fetched whole.
    pub async fn fetch_content(
        &self,
        depot: &str,
        id: &ObjectId,
        dest: &Path,
    ) -> Result<FetchedContent> {
        let url = self.url(&["depots", depot, "blobs", &id.to_string()])?;
        let resp = self
            .send(Retry::Yes, || {
                Ok(self
                    .request(Method::GET, url.clone())
                    .header(ACCEPT_ENCODING, ENCODING_ZSTD))
            })
            .await?;
        let raw_len = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let encoded = resp.headers().contains_key(CONTENT_ENCODING);
        if !encoded && raw_len.is_some_and(|n| n >= lode_core::CHUNK_THRESHOLD) {
            drop(resp);
            if let Some(list) = self.chunk_list(depot, id).await? {
                return Ok(FetchedContent::Chunked(list));
            }
            let n = self.download_blob(depot, id, dest).await?;
            return Ok(FetchedContent::Whole(n));
        }
        let dir = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        let tmp = tempfile::Builder::new()
            .prefix(".lode-fetch-")
            .tempfile_in(&dir)?;
        let mut file = tokio::fs::File::from_std(tmp.as_file().try_clone()?);
        let n = copy_verified(body_reader(resp)?, &mut file, id, MAX_BLOB_SIZE).await?;
        file.flush().await?;
        drop(file);
        tmp.persist(dest).map_err(|e| Error::Io(e.error))?;
        Ok(FetchedContent::Whole(n))
    }

    /// `GET /depots/{d}/obliterated-ids?after=`: ids obliterated from the depot (T154).
    pub async fn obliterated_ids(
        &self,
        depot: &str,
        after: Option<i64>,
    ) -> Result<lode_proto::depot::ObliteratedIds> {
        let mut url = self.url(&["depots", depot, "obliterated-ids"])?;
        if let Some(a) = after {
            url.query_pairs_mut()
                .append_pair("after", &a.max(0).to_string());
        }
        let resp = self
            .send(Retry::Yes, || Ok(self.request(Method::GET, url.clone())))
            .await?;
        decode(resp).await
    }

    /// `GET /depots/{d}/files?rev=&path=`: a file's content at a revision, with its id, kind
    /// and executable bit.
    pub async fn file(&self, depot: &str, rev: u64, path: &DepotPath) -> Result<FileContent> {
        let mut url = self.url(&["depots", depot, "files"])?;
        url.query_pairs_mut()
            .append_pair("rev", &rev.to_string())
            .append_pair("path", path.as_str());
        let resp = self
            .send(Retry::Yes, || Ok(self.request(Method::GET, url.clone())))
            .await?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let id = header(HEADER_ID)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Protocol(format!("no valid {HEADER_ID} header")))?;
        let kind = match header(HEADER_KIND).as_deref() {
            Some("file") => EntryKind::File,
            Some("symlink") => EntryKind::Symlink,
            other => {
                return Err(Error::Protocol(format!(
                    "unexpected {HEADER_KIND} {other:?}"
                )))
            }
        };
        let executable = header(HEADER_EXECUTABLE).as_deref() == Some("true");
        let size = header(CONTENT_LENGTH.as_str()).and_then(|s| s.parse().ok());
        Ok(FileContent {
            id,
            kind,
            executable,
            size,
            resp,
        })
    }
}

/// A file's content at a revision, not yet read.
#[derive(Debug)]
pub struct FileContent {
    /// Blob id of the content.
    pub id: ObjectId,
    /// `File` or `Symlink` (the content of a symlink is its target).
    pub kind: EntryKind,
    /// Executable bit.
    pub executable: bool,
    /// Length, when the server sent one.
    pub size: Option<u64>,
    resp: Response,
}

impl FileContent {
    /// Copies the content to `out`, checking it against [`FileContent::id`]. Returns the
    /// length.
    pub async fn write_to<W>(self, out: &mut W) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        let id = self.id;
        let reader = body_reader(self.resp)?;
        copy_verified(reader, out, &id, self.size.unwrap_or(MAX_BLOB_SIZE)).await
    }
}

/// The body as raw content: decoded when the server sent zstd.
fn body_reader(resp: Response) -> Result<BoxRead> {
    let zstd = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().eq_ignore_ascii_case(ENCODING_ZSTD));
    let raw = StreamReader::new(resp.bytes_stream());
    Ok(match zstd {
        None => Box::new(raw),
        Some(true) => Box::new(ZstdDecodeReader::new(raw)?),
        Some(false) => {
            return Err(Error::Protocol(
                "the server sent a content encoding that was not asked for".into(),
            ))
        }
    })
}

async fn copy_verified<W>(
    mut reader: BoxRead,
    out: &mut W,
    expected: &ObjectId,
    max_len: u64,
) -> Result<u64>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_len {
            return Err(Error::Protocol(format!(
                "blob {expected} is larger than the {max_len} bytes expected"
            )));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await?;
    }
    out.flush().await?;
    let actual = ObjectId::from_bytes(*hasher.finalize().as_bytes());
    if actual != *expected {
        return Err(Error::HashMismatch {
            expected: *expected,
            actual,
        });
    }
    Ok(total)
}
