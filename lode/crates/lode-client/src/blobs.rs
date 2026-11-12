//! Blob upload and download, streamed and verified, and file content at a revision.

use std::path::Path;

use lode_core::{DepotPath, EntryKind, ObjectId};
use lode_proto::consts::{ENCODING_ZSTD, HEADER_EXECUTABLE, HEADER_ID, HEADER_KIND};
use lode_proto::depot::PutBlobResponse;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH};
use reqwest::{Body, Method, Response};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{Error, Result};
use crate::io::{reader_stream, StreamReader, ZstdDecodeReader, ZstdEncodeReader};
use crate::{decode, Client, Retry};

const COPY_BUF: usize = 64 * 1024;

type BoxRead = Box<dyn AsyncRead + Send + Unpin>;

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
        self.get_blob_once(depot, id, out, Retry::Yes).await
    }

    async fn get_blob_once<W>(
        &self,
        depot: &str,
        id: &ObjectId,
        out: &mut W,
        retry: Retry,
    ) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        let url = self.url(&["depots", depot, "blobs", &id.to_string()])?;
        let resp = self
            .send(retry, || {
                Ok(self
                    .request(Method::GET, url.clone())
                    .header(ACCEPT_ENCODING, ENCODING_ZSTD))
            })
            .await?;
        let reader = body_reader(resp)?;
        copy_verified(reader, out, id).await
    }

    /// Downloads blob `id` to `dest`: written to a temporary file next to it, verified, then
    /// renamed into place. On any failure nothing is left at `dest` (an existing file there
    /// is untouched) and the temporary file is removed. Retries transient failures with a
    /// fresh temporary file.
    pub async fn download_blob(&self, depot: &str, id: &ObjectId, dest: &Path) -> Result<u64> {
        let dir = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.download_once(depot, id, &dir, dest).await {
                Err(e) if e.is_transient() && attempt < self.retry.max_attempts => {
                    tokio::time::sleep(self.retry.delay(attempt, None)).await;
                }
                other => return other,
            }
        }
    }

    async fn download_once(
        &self,
        depot: &str,
        id: &ObjectId,
        dir: &Path,
        dest: &Path,
    ) -> Result<u64> {
        let tmp = tempfile::Builder::new()
            .prefix(".lode-download-")
            .tempfile_in(dir)?;
        let len = {
            let mut file = tokio::fs::File::from_std(tmp.as_file().try_clone()?);
            let len = self.get_blob_once(depot, id, &mut file, Retry::No).await?;
            file.flush().await?;
            file.sync_all().await?;
            len
        };
        tmp.persist(dest).map_err(|e| Error::Io(e.error))?;
        Ok(len)
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
        copy_verified(reader, out, &id).await
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

async fn copy_verified<W>(mut reader: BoxRead, out: &mut W, expected: &ObjectId) -> Result<u64>
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
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await?;
        total += n as u64;
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
