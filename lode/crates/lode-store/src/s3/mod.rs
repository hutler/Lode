//! S3-compatible implementation of [`BlobStore`] (T123): AWS S3, Backblaze B2, MinIO and
//! others that speak the S3 API with Signature Version 4.
//!
//! Objects keep the stored format of ARCHITECTURE §3.3 under
//! `<prefix>/<depot-id>/<h0h1>/<h2h3>/<64-hex>`. A put is encoded into a local temp file first
//! (hashed, checked, compressed) and then sent with one `PUT` carrying `If-None-Match: *`, so
//! an object is never replaced: `412 Precondition Failed` means it was already there. Reads
//! stream the response body. `list` pages through ListObjectsV2. Transport errors and 5xx
//! answers are tried again up to three times.
//!
//! Single `PUT`s are limited to 5 GiB by S3, so this store refuses larger objects (multipart
//! uploads are a later task).

#[cfg(feature = "test-util")]
pub mod fake;
pub mod sigv4;
mod xml;

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::stream::{self, BoxStream, Stream, StreamExt};
use lode_core::{Codec, DepotId, ObjectHeader, ObjectId, ObjectKind};
use reqwest::{Method, StatusCode};
use tokio::fs::{self as tfs, File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use self::sigv4::{Credentials, EMPTY_SHA256, UNSIGNED_PAYLOAD};
use crate::encode::{encode, Limits, CHUNK};
use crate::fs::{remove_stale_tmp_dir, TempGuard};
use crate::zstd_reader::ZstdReader;
use crate::{BlobStore, ObjectReader, PutOptions, PutOutcome, Result, StoreError, StoredReader};

/// The largest object one S3 `PUT` takes (5 GiB), less the object header.
pub const MAX_SINGLE_PUT: u64 = 5 * 1024 * 1024 * 1024 - ObjectHeader::LEN as u64;
const ATTEMPTS: u32 = 3;
const TIMEOUT_IDLE: Duration = Duration::from_secs(300);
const STALE_TMP: Duration = Duration::from_secs(24 * 60 * 60);

/// Settings for [`S3BlobStore`].
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Bucket name.
    pub bucket: String,
    /// Key prefix, without leading or trailing `/`; may be empty.
    pub prefix: String,
    /// Region, such as `us-east-1` (MinIO accepts any).
    pub region: String,
    /// Endpoint for S3-compatible services, such as `https://s3.us-west-004.backblazeb2.com`
    /// or `http://127.0.0.1:9000`. `None` is AWS: `https://s3.<region>.amazonaws.com`.
    pub endpoint: Option<String>,
    /// Bucket in the path (`endpoint/bucket/key`) instead of the host name
    /// (`bucket.endpoint/key`). MinIO usually needs it.
    pub path_style: bool,
    /// Credentials.
    pub credentials: Credentials,
    /// Largest raw object accepted; capped at [`MAX_SINGLE_PUT`].
    pub max_object_size: u64,
    /// Local directory for uploads being prepared.
    pub tmp_dir: PathBuf,
}

/// Blob store in an S3 bucket.
#[derive(Debug, Clone)]
pub struct S3BlobStore {
    cfg: S3Config,
    http: reqwest::Client,
    /// `scheme://host[:port]`.
    origin: String,
    /// The `Host` header, as signed.
    host: String,
}

/// A request body.
enum Body<'a> {
    Empty,
    File(&'a Path, u64),
}

fn other(message: String) -> StoreError {
    StoreError::Io(io::Error::other(message))
}

impl S3BlobStore {
    /// Sets up the store and removes temp files older than 24 hours from `tmp_dir`. Nothing
    /// is sent to S3 until the first request; [`S3BlobStore::check`] tries the bucket.
    pub async fn open(cfg: S3Config) -> Result<Self> {
        if cfg.bucket.is_empty() {
            return Err(other("S3 storage needs a bucket name".into()));
        }
        let endpoint = match &cfg.endpoint {
            Some(e) => e.trim_end_matches('/').to_owned(),
            None => format!("https://s3.{}.amazonaws.com", cfg.region),
        };
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|e| other(format!("S3 endpoint {endpoint} is not a URL: {e}")))?;
        let scheme = url.scheme();
        let Some(host) = url.host_str() else {
            return Err(other(format!("S3 endpoint {endpoint} has no host")));
        };
        if !matches!(scheme, "http" | "https") || url.path() != "/" || url.query().is_some() {
            return Err(other(format!(
                "S3 endpoint {endpoint} must be http(s)://host[:port] with nothing after it"
            )));
        }
        let host_port = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_owned(),
        };
        let host = if cfg.path_style {
            host_port
        } else {
            format!("{}.{host_port}", cfg.bucket)
        };
        let http = reqwest::Client::builder()
            .read_timeout(TIMEOUT_IDLE)
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("lode-store/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| other(format!("cannot set up the S3 client: {e}")))?;
        tfs::create_dir_all(&cfg.tmp_dir).await?;
        let tmp = cfg.tmp_dir.clone();
        tokio::task::spawn_blocking(move || remove_stale_tmp_dir(&tmp, STALE_TMP))
            .await
            .map_err(io::Error::other)??;
        Ok(Self {
            origin: format!("{scheme}://{host}"),
            host,
            cfg,
            http,
        })
    }

    fn cap(&self) -> u64 {
        self.cfg.max_object_size.min(MAX_SINGLE_PUT)
    }

    /// The key of an object.
    pub fn key(&self, depot: DepotId, id: &ObjectId) -> String {
        let hex = id.to_hex();
        format!(
            "{}{}/{}/{}/{hex}",
            self.depot_prefix_base(),
            depot.0,
            &hex[0..2],
            &hex[2..4]
        )
    }

    fn depot_prefix_base(&self) -> String {
        if self.cfg.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", self.cfg.prefix)
        }
    }

    fn depot_prefix(&self, depot: DepotId) -> String {
        format!("{}{}/", self.depot_prefix_base(), depot.0)
    }

    /// The encoded request path for `key` (or the bucket itself).
    fn path(&self, key: Option<&str>) -> String {
        let key = key.map(|k| sigv4::uri_encode(k, true));
        match (self.cfg.path_style, key) {
            (true, Some(k)) => format!("/{}/{k}", sigv4::uri_encode(&self.cfg.bucket, false)),
            (true, None) => format!("/{}", sigv4::uri_encode(&self.cfg.bucket, false)),
            (false, Some(k)) => format!("/{k}"),
            (false, None) => "/".to_owned(),
        }
    }

    /// Sends one signed request, trying again on transport errors and 5xx answers.
    async fn send(
        &self,
        method: Method,
        key: Option<&str>,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        body: Body<'_>,
    ) -> Result<reqwest::Response> {
        let path = self.path(key);
        let qs = sigv4::canonical_query(query);
        let url = if qs.is_empty() {
            format!("{}{path}", self.origin)
        } else {
            format!("{}{path}?{qs}", self.origin)
        };
        let payload_hash = match body {
            Body::Empty => EMPTY_SHA256,
            Body::File(..) => UNSIGNED_PAYLOAD,
        };
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 * 4u64.pow(attempt - 1))).await;
            }
            let amz_date = sigv4::amz_date(SystemTime::now());
            let mut headers: Vec<(&str, &str)> = vec![
                ("host", &self.host),
                ("x-amz-content-sha256", payload_hash),
                ("x-amz-date", &amz_date),
            ];
            if let Some(t) = &self.cfg.credentials.session_token {
                headers.push(("x-amz-security-token", t));
            }
            headers.extend_from_slice(extra);
            let auth = sigv4::authorization(
                &self.cfg.credentials,
                &sigv4::Request {
                    method: method.as_str(),
                    path: &path,
                    query,
                    headers: &headers,
                    payload_hash,
                    amz_date: &amz_date,
                    region: &self.cfg.region,
                    service: "s3",
                },
            );
            let mut req = self.http.request(method.clone(), &url);
            for (k, v) in &headers {
                req = req.header(*k, *v);
            }
            req = req.header(reqwest::header::AUTHORIZATION, auth);
            req = match body {
                Body::Empty => req,
                Body::File(p, len) => req
                    .header(reqwest::header::CONTENT_LENGTH, len)
                    .body(file_body(File::open(p).await?)),
            };
            match req.send().await {
                Ok(resp) if resp.status().is_server_error() => {
                    last = Some(s3_error(resp, method.as_str(), key).await);
                }
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    last = Some(other(format!(
                        "cannot reach S3 for {method} {}: {}",
                        key.unwrap_or("the bucket"),
                        e.without_url()
                    )));
                }
            }
        }
        Err(last.unwrap_or_else(|| other("S3 request failed".into())))
    }

    /// Checks that the bucket can be reached with these credentials.
    pub async fn check(&self) -> Result<()> {
        let prefix = self.depot_prefix_base();
        let resp = self
            .send(
                Method::GET,
                None,
                &[("list-type", "2"), ("max-keys", "1"), ("prefix", &prefix)],
                &[],
                Body::Empty,
            )
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(s3_error(resp, "GET", None).await)
        }
    }

    /// Creates the bucket if it does not exist. For tests and a first MinIO setup.
    pub async fn ensure_bucket(&self) -> Result<()> {
        let resp = self.send(Method::HEAD, None, &[], &[], Body::Empty).await?;
        if resp.status().is_success() {
            return Ok(());
        }
        let resp = self.send(Method::PUT, None, &[], &[], Body::Empty).await?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            StatusCode::CONFLICT => Ok(()), // created meanwhile
            _ => Err(s3_error(resp, "PUT", None).await),
        }
    }

    /// Uploads a file that already holds an object in the stored format, such as one of a
    /// filesystem store being migrated. The header is checked; the content is not re-hashed.
    pub async fn put_stored_file(
        &self,
        depot: DepotId,
        id: &ObjectId,
        file: &Path,
    ) -> Result<PutOutcome> {
        let mut f = File::open(file).await?;
        let len = f.metadata().await?.len();
        let mut head = [0u8; ObjectHeader::LEN];
        f.read_exact(&mut head)
            .await
            .map_err(|_| corrupt(id, "file is shorter than the header"))?;
        ObjectHeader::decode(&head).map_err(|e| corrupt(id, &e.to_string()))?;
        drop(f);
        self.upload(depot, id, file, len).await
    }

    async fn upload(
        &self,
        depot: DepotId,
        id: &ObjectId,
        file: &Path,
        len: u64,
    ) -> Result<PutOutcome> {
        let key = self.key(depot, id);
        for _ in 0..ATTEMPTS {
            let resp = self
                .send(
                    Method::PUT,
                    Some(&key),
                    &[],
                    &[("if-none-match", "*")],
                    Body::File(file, len),
                )
                .await?;
            match resp.status() {
                s if s.is_success() => return Ok(PutOutcome::Created),
                StatusCode::PRECONDITION_FAILED => return Ok(PutOutcome::AlreadyPresent),
                // A concurrent conditional write of the same key; the next try sees it.
                StatusCode::CONFLICT => continue,
                _ => return Err(s3_error(resp, "PUT", Some(&key)).await),
            }
        }
        Ok(PutOutcome::AlreadyPresent)
    }

    /// Opens an object: its header, stored length and the rest of the body.
    async fn open_object(
        &self,
        depot: DepotId,
        id: &ObjectId,
    ) -> Result<(BodyReader, ObjectHeader, u64)> {
        let key = self.key(depot, id);
        let resp = self
            .send(Method::GET, Some(&key), &[], &[], Body::Empty)
            .await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(StoreError::NotFound(*id)),
            _ => return Err(s3_error(resp, "GET", Some(&key)).await),
        }
        let total = resp
            .content_length()
            .ok_or_else(|| other(format!("S3 sent {key} without a length")))?;
        let mut body = BodyReader::new(resp.bytes_stream().boxed());
        let mut head = [0u8; ObjectHeader::LEN];
        if let Err(e) = body.read_exact(&mut head).await {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Err(corrupt(id, "object is shorter than the header"));
            }
            return Err(e.into());
        }
        let header = ObjectHeader::decode(&head).map_err(|e| corrupt(id, &e.to_string()))?;
        let stored_len = total.saturating_sub(ObjectHeader::LEN as u64);
        if header.codec == Codec::None && stored_len != header.raw_len {
            return Err(corrupt(id, "payload length differs from the header"));
        }
        Ok((body, header, stored_len))
    }

    async fn list_page(
        &self,
        depot: DepotId,
        token: Option<String>,
    ) -> Result<(Vec<Result<(ObjectId, SystemTime)>>, Option<String>)> {
        let prefix = self.depot_prefix(depot);
        let (objects, next) = self.list_raw(&prefix, token).await?;
        let mut items = Vec::with_capacity(objects.len());
        for (key, modified) in objects {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let Some(id) = parse_object_key(rest) else {
                continue; // not an object, such as a stray file
            };
            let when = time::OffsetDateTime::parse(
                &modified,
                &time::format_description::well_known::Rfc3339,
            )
            .map(SystemTime::from)
            .map_err(|_| other(format!("S3 listing: bad LastModified {modified:?}")));
            items.push(when.map(|t| (id, t)));
        }
        Ok((items, next))
    }
}

/// Plain files under the store's prefix, for uses other than object storage, such as
/// copies of backups. `rel` is a key relative to the prefix.
impl S3BlobStore {
    fn full_key(&self, rel: &str) -> String {
        format!("{}{rel}", self.depot_prefix_base())
    }

    /// Uploads `file` as `rel`. With `only_new` an existing key is left alone and the
    /// answer is `false`; otherwise it is replaced. Files over 5 GiB are refused.
    pub async fn put_file(&self, rel: &str, file: &Path, only_new: bool) -> Result<bool> {
        let len = tfs::metadata(file).await?.len();
        if len > MAX_SINGLE_PUT {
            return Err(other(format!(
                "{} is larger than the 5 GiB one S3 upload takes",
                file.display()
            )));
        }
        let key = self.full_key(rel);
        let extra: &[(&str, &str)] = if only_new {
            &[("if-none-match", "*")]
        } else {
            &[]
        };
        for _ in 0..ATTEMPTS {
            let resp = self
                .send(Method::PUT, Some(&key), &[], extra, Body::File(file, len))
                .await?;
            match resp.status() {
                s if s.is_success() => return Ok(true),
                StatusCode::PRECONDITION_FAILED if only_new => return Ok(false),
                StatusCode::CONFLICT => continue,
                _ => return Err(s3_error(resp, "PUT", Some(&key)).await),
            }
        }
        Ok(false)
    }

    /// Downloads `rel` into `dest`, replacing it. A missing key is [`StoreError::Io`] with
    /// kind `NotFound`.
    pub async fn get_file(&self, rel: &str, dest: &Path) -> Result<u64> {
        let key = self.full_key(rel);
        let resp = self
            .send(Method::GET, Some(&key), &[], &[], Body::Empty)
            .await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(StoreError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{key} is not in the bucket"),
                )))
            }
            _ => return Err(s3_error(resp, "GET", Some(&key)).await),
        }
        let mut body = BodyReader::new(resp.bytes_stream().boxed());
        let tmp = dest.with_extension("part");
        let mut out = File::create(&tmp).await?;
        let n = tokio::io::copy(&mut body, &mut out).await?;
        out.sync_all().await?;
        drop(out);
        tfs::rename(&tmp, dest).await?;
        Ok(n)
    }

    /// Every key that starts with `rel_prefix`, relative to the store's prefix, in order.
    pub async fn list_keys(&self, rel_prefix: &str) -> Result<Vec<String>> {
        let full = self.full_key(rel_prefix);
        let base = self.depot_prefix_base();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (objects, next) = self.list_raw(&full, token.take()).await?;
            out.extend(
                objects
                    .into_iter()
                    .filter_map(|(k, _)| k.strip_prefix(&base).map(str::to_owned)),
            );
            match next {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }

    /// Deletes `rel`; a missing key is not an error.
    pub async fn delete_key(&self, rel: &str) -> Result<()> {
        let key = self.full_key(rel);
        let resp = self
            .send(Method::DELETE, Some(&key), &[], &[], Body::Empty)
            .await?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(s3_error(resp, "DELETE", Some(&key)).await)
        }
    }

    /// One ListObjectsV2 page of full keys under `prefix`, with their modification times.
    async fn list_raw(
        &self,
        prefix: &str,
        token: Option<String>,
    ) -> Result<(Vec<(String, String)>, Option<String>)> {
        let mut query = vec![("list-type", "2"), ("prefix", prefix)];
        if let Some(t) = &token {
            query.push(("continuation-token", t.as_str()));
        }
        let resp = self
            .send(Method::GET, None, &query, &[], Body::Empty)
            .await?;
        if !resp.status().is_success() {
            return Err(s3_error(resp, "GET", None).await);
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| other(format!("reading an S3 listing failed: {}", e.without_url())))?;
        let page = xml::parse_list(&body).map_err(|e| other(format!("S3 listing: {e}")))?;
        let next = if page.truncated {
            match page.next_token {
                Some(t) => Some(t),
                None => return Err(other("S3 listing is truncated without a token".into())),
            }
        } else {
            None
        };
        Ok((page.objects, next))
    }
}

#[cfg(feature = "test-util")]
impl S3BlobStore {
    /// Test access: writes `bytes` at `rel` under the depot's prefix, replacing anything.
    pub async fn raw_put(&self, depot: DepotId, rel: &str, bytes: &[u8]) -> Result<()> {
        let tmp = self
            .cfg
            .tmp_dir
            .join(uuid::Uuid::new_v4().simple().to_string());
        let _guard = TempGuard {
            path: Some(tmp.clone()),
        };
        tfs::write(&tmp, bytes).await?;
        let key = format!("{}{rel}", self.depot_prefix(depot));
        let resp = self
            .send(
                Method::PUT,
                Some(&key),
                &[],
                &[],
                Body::File(&tmp, bytes.len() as u64),
            )
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(s3_error(resp, "PUT", Some(&key)).await)
        }
    }

    /// Test access: the bytes at `rel` under the depot's prefix.
    pub async fn raw_get(&self, depot: DepotId, rel: &str) -> Result<Vec<u8>> {
        let key = format!("{}{rel}", self.depot_prefix(depot));
        let resp = self
            .send(Method::GET, Some(&key), &[], &[], Body::Empty)
            .await?;
        if !resp.status().is_success() {
            return Err(s3_error(resp, "GET", Some(&key)).await);
        }
        let b = resp
            .bytes()
            .await
            .map_err(|e| other(format!("reading {key}: {}", e.without_url())))?;
        Ok(b.to_vec())
    }
}

/// `h0h1/h2h3/<64 lower-case hex>` with matching prefixes.
fn parse_object_key(rest: &str) -> Option<ObjectId> {
    let mut parts = rest.split('/');
    let (Some(a), Some(b), Some(name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let id = ObjectId::from_hex(name).ok()?;
    (id.to_hex() == name && a == &name[0..2] && b == &name[2..4]).then_some(id)
}

fn corrupt(id: &ObjectId, reason: &str) -> StoreError {
    StoreError::Corrupt {
        id: *id,
        reason: reason.to_owned(),
    }
}

/// An S3 error answer as a store error: status and S3's error code, never a secret.
async fn s3_error(resp: reqwest::Response, method: &str, key: Option<&str>) -> StoreError {
    let status = resp.status();
    let body = resp.bytes().await.unwrap_or_default();
    let head = &body[..body.len().min(4096)];
    let code = xml::error_code(head).unwrap_or_default();
    other(format!(
        "S3 answered {status}{}{} to {method} {}",
        if code.is_empty() { "" } else { " " },
        code,
        key.unwrap_or("the bucket")
    ))
}

/// A file as a streamed request body.
fn file_body(file: File) -> reqwest::Body {
    let s = stream::unfold(Some(file), |state| async move {
        let mut f = state?;
        let mut buf = vec![0u8; CHUNK];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, io::Error>(Bytes::from(buf)), Some(f)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    reqwest::Body::wrap_stream(s)
}

/// A response body as an [`AsyncRead`].
pub(crate) struct BodyReader {
    stream: BoxStream<'static, reqwest::Result<Bytes>>,
    buf: Bytes,
}

impl BodyReader {
    fn new(stream: BoxStream<'static, reqwest::Result<Bytes>>) -> Self {
        Self {
            stream,
            buf: Bytes::new(),
        }
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        while self.buf.is_empty() {
            match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Ready(Some(Ok(b))) => self.buf = b,
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::other(e.without_url())))
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = self.buf.len().min(out.remaining());
        let chunk = self.buf.split_to(n);
        out.put_slice(&chunk);
        Poll::Ready(Ok(()))
    }
}

#[async_trait::async_trait]
impl BlobStore for S3BlobStore {
    async fn contains(&self, depot: DepotId, id: &ObjectId) -> Result<bool> {
        let key = self.key(depot, id);
        let resp = self
            .send(Method::HEAD, Some(&key), &[], &[], Body::Empty)
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(s3_error(resp, "HEAD", Some(&key)).await),
        }
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
            cap: self.cap(),
            expected: opts.expected_len,
        };
        limits.precheck()?;
        tfs::create_dir_all(&self.cfg.tmp_dir).await?;
        let tmp_path = self
            .cfg
            .tmp_dir
            .join(uuid::Uuid::new_v4().simple().to_string());
        let _guard = TempGuard {
            path: Some(tmp_path.clone()),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await?;
        encode(&mut file, kind, expect, raw, &limits).await?;
        let len = file.metadata().await?.len();
        drop(file);
        self.upload(depot, expect, &tmp_path, len).await
    }

    async fn get(&self, depot: DepotId, id: &ObjectId) -> Result<ObjectReader> {
        let (body, header, _) = self.open_object(depot, id).await?;
        let reader: crate::ByteStream = match header.codec {
            Codec::None => Box::pin(body.take(header.raw_len)),
            Codec::Zstd => Box::pin(ZstdReader::new(
                *id,
                tokio::io::BufReader::with_capacity(CHUNK, body),
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
        let (body, header, stored_len) = self.open_object(depot, id).await?;
        Ok(StoredReader {
            kind: header.kind,
            codec: header.codec,
            raw_len: header.raw_len,
            stored_len,
            reader: Box::pin(body),
        })
    }

    async fn delete(&self, depot: DepotId, id: &ObjectId) -> Result<()> {
        if !self.contains(depot, id).await? {
            return Err(StoreError::NotFound(*id));
        }
        let key = self.key(depot, id);
        let resp = self
            .send(Method::DELETE, Some(&key), &[], &[], Body::Empty)
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(s3_error(resp, "DELETE", Some(&key)).await)
        }
    }

    fn list(&self, depot: DepotId) -> BoxStream<'_, Result<(ObjectId, SystemTime)>> {
        // State: the next page's token, `Some(None)` for the first page, `None` when done.
        stream::unfold(Some(None::<String>), move |state| async move {
            let token = state?;
            match self.list_page(depot, token).await {
                Ok((items, next)) => Some((stream::iter(items), next.map(Some))),
                Err(e) => Some((stream::iter(vec![Err(e)]), None)),
            }
        })
        .flatten()
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_keys() {
        let id = ObjectId::from_bytes([0xab; 32]);
        let hex = id.to_hex();
        assert_eq!(parse_object_key(&format!("ab/ab/{hex}")), Some(id));
        assert_eq!(parse_object_key(&format!("ab/ac/{hex}")), None);
        assert_eq!(
            parse_object_key(&format!("ab/ab/{}", hex.to_uppercase())),
            None
        );
        assert_eq!(parse_object_key("ab/ab/notes.txt"), None);
        assert_eq!(parse_object_key(&format!("x/ab/ab/{hex}")), None);
    }
}
