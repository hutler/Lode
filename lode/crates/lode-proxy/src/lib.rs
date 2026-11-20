//! The Lode caching proxy (T154, ARCHITECTURE §10.8).
//!
//! A small server in a remote office that answers content downloads
//! (`GET /api/v1/depots/{d}/blobs/{id}` and `…/chunks`) from a disk cache, fetching each
//! object from the main server ("upstream") once and checking its hash before keeping it.
//! Everything else is refused with a hint to use the main server.
//!
//! Access stays with the upstream: the proxy keeps no tokens. For each caller (keyed by a
//! hash of the bearer token) and depot it asks the upstream once a minute whether the caller
//! may read the depot, using the caller's own token; the same call brings the depot's newly
//! obliterated ids, whose cached copies are then deleted.

#![forbid(unsafe_code)]

pub mod cache;
pub mod config;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{
    ACCEPT_RANGES, AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bytes::Bytes;
use futures::StreamExt;
use lode_client::{Client, FetchedContent};
use lode_core::{ChunkList, ErrorCode, ObjectId};
use lode_proto::auth::{Info, ProxyInfo};
use lode_proto::consts::paths;
use lode_server::{ApiError, BoundServer, SharedClock, SystemClock};
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::cache::{file_id, DiskCache, Key};
pub use crate::config::{ProxyConfig, EXAMPLE_CONFIG};

/// How long an access answer and a depot's obliterated ids are kept, in milliseconds.
pub const REFRESH_MS: i64 = 60_000;

/// How far back the next obliterated-ids query reaches past the previous answer, so an
/// obliteration recorded just before that answer is not missed.
const OBLITERATED_MARGIN_MS: i64 = 5 * 60_000;

/// Bytes read from the cache per body frame.
const FRAME: usize = 256 * 1024;

/// `User-Agent` product of the proxy's requests to the upstream.
const PRODUCT: &str = concat!("loded-proxy/", env!("CARGO_PKG_VERSION"));

/// Why the proxy cannot start.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// The configuration is unusable.
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    /// The cache directory cannot be used.
    #[error("cache directory {path}: {source}")]
    Cache {
        /// The directory.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// The upstream URL is unusable.
    #[error("upstream: {0}")]
    Upstream(#[from] lode_client::Error),
    /// The listener could not be set up.
    #[error(transparent)]
    Server(#[from] lode_server::ServerError),
}

/// Counters for the `/admin/` page.
#[derive(Debug, Default)]
struct Metrics {
    hits: AtomicU64,
    misses: AtomicU64,
    bytes_saved: AtomicU64,
    bytes_fetched: AtomicU64,
    upstream_errors: AtomicU64,
}

/// A snapshot of the proxy's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MetricsView {
    /// Bytes in the cache.
    pub cache_bytes: u64,
    /// Entries in the cache.
    pub cache_entries: u64,
    /// The cache's cap in bytes.
    pub cache_max_bytes: u64,
    /// Objects served from the cache.
    pub hits: u64,
    /// Objects fetched from the upstream.
    pub misses: u64,
    /// Bytes served from the cache, which did not cross the long link.
    pub bytes_saved: u64,
    /// Bytes fetched from the upstream.
    pub bytes_fetched: u64,
    /// Failed requests to the upstream.
    pub upstream_errors: u64,
}

/// An access answer for one caller and depot.
#[derive(Clone)]
struct Access {
    at_ms: i64,
    refused: Option<(u16, String, String)>,
}

/// What the proxy knows of a depot's obliterated content.
#[derive(Default)]
struct Obliterated {
    refreshed_ms: Option<i64>,
    after_ms: Option<i64>,
    ids: HashSet<ObjectId>,
}

/// One object's fetch lock.
type FlightLock = Arc<tokio::sync::Mutex<()>>;

struct Inner {
    upstream: Client,
    upstream_url: String,
    cache: DiskCache,
    clock: SharedClock,
    access: Mutex<HashMap<([u8; 32], String), Access>>,
    obliterated: Mutex<HashMap<String, Obliterated>>,
    flights: Mutex<HashMap<(String, ObjectId), FlightLock>>,
    metrics: Metrics,
}

/// The proxy's shared state.
#[derive(Clone)]
pub struct ProxyState(Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The bearer token of a request, if any.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_owned())
}

fn parse_id(s: &str) -> Result<ObjectId, ApiError> {
    s.parse()
        .map_err(|_| ApiError::invalid_request(format!("`{s}` is not an object id")))
}

/// The upstream's rejection, passed on as it came.
fn passed_on(e: &lode_client::ApiError) -> ApiError {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY);
    ApiError::custom(&e.code, status, e.message.clone()).with_details(e.details.clone())
}

impl ProxyState {
    /// Opens the cache and prepares the upstream client.
    pub fn new(config: &ProxyConfig, clock: SharedClock) -> Result<Self, ProxyError> {
        let cache =
            DiskCache::open(&config.cache_dir, config.cache_max_bytes).map_err(|source| {
                ProxyError::Cache {
                    path: config.cache_dir.clone(),
                    source,
                }
            })?;
        let upstream = Client::builder(config.upstream.as_str())
            .allow_insecure_http(config.allow_http_upstream)
            .product(PRODUCT)
            .build()?;
        Ok(Self(Arc::new(Inner {
            upstream_url: upstream.server_url().to_string(),
            upstream,
            cache,
            clock,
            access: Mutex::default(),
            obliterated: Mutex::default(),
            flights: Mutex::default(),
            metrics: Metrics::default(),
        })))
    }

    /// The counters.
    pub fn metrics(&self) -> MetricsView {
        let m = &self.0.metrics;
        let size = self.0.cache.size();
        MetricsView {
            cache_bytes: size.bytes,
            cache_entries: size.entries,
            cache_max_bytes: self.0.cache.max_bytes(),
            hits: m.hits.load(Ordering::Relaxed),
            misses: m.misses.load(Ordering::Relaxed),
            bytes_saved: m.bytes_saved.load(Ordering::Relaxed),
            bytes_fetched: m.bytes_fetched.load(Ordering::Relaxed),
            upstream_errors: m.upstream_errors.load(Ordering::Relaxed),
        }
    }

    /// The cache, for tests and tools.
    pub fn cache(&self) -> &DiskCache {
        &self.0.cache
    }

    fn upstream_failed(&self, e: &lode_client::Error) -> ApiError {
        self.0
            .metrics
            .upstream_errors
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(error = %e, "upstream request failed");
        match e {
            lode_client::Error::Api(a) => passed_on(a),
            _ => ApiError::custom(
                "unavailable",
                StatusCode::BAD_GATEWAY,
                "the caching proxy cannot reach the main server",
            ),
        }
    }

    fn client_for(&self, token: Option<&str>) -> Client {
        match token {
            Some(t) => self.0.upstream.with_token(t),
            None => self.0.upstream.clone(),
        }
    }

    /// Checks that the caller may read `depot`, asking the upstream when the last answer
    /// for this caller or the depot's obliterated ids are older than [`REFRESH_MS`].
    async fn authorize(&self, token: Option<&str>, depot: &str) -> Result<(), ApiError> {
        let now = self.0.clock.now_ms();
        let who: [u8; 32] = sha2::Sha256::digest(token.unwrap_or("").as_bytes()).into();
        let key = (who, depot.to_owned());
        let cached = lock(&self.0.access).get(&key).cloned();
        let (depot_fresh, after) = {
            let o = lock(&self.0.obliterated);
            match o.get(depot) {
                Some(d) => (
                    d.refreshed_ms.is_some_and(|r| now - r < REFRESH_MS),
                    d.after_ms,
                ),
                None => (false, None),
            }
        };
        if let Some(a) = &cached {
            if now - a.at_ms < REFRESH_MS && (depot_fresh || a.refused.is_some()) {
                return match &a.refused {
                    None => Ok(()),
                    Some((status, code, message)) => Err(ApiError::custom(
                        code,
                        StatusCode::from_u16(*status).unwrap_or(StatusCode::FORBIDDEN),
                        message.clone(),
                    )),
                };
            }
        }
        match self.client_for(token).obliterated_ids(depot, after).await {
            Ok(list) => {
                let gone: Vec<ObjectId> = {
                    let mut o = lock(&self.0.obliterated);
                    let d = o.entry(depot.to_owned()).or_default();
                    d.refreshed_ms = Some(now);
                    d.after_ms = Some(list.now_ms - OBLITERATED_MARGIN_MS);
                    list.ids
                        .into_iter()
                        .filter(|id| d.ids.insert(*id))
                        .collect()
                };
                for id in gone {
                    self.0.cache.remove(&Key::content(depot, id));
                    self.0.cache.remove(&Key::list(depot, id));
                }
                lock(&self.0.access).insert(
                    key,
                    Access {
                        at_ms: now,
                        refused: None,
                    },
                );
                Ok(())
            }
            Err(lode_client::Error::Api(a)) if a.status < 500 => {
                lock(&self.0.access).insert(
                    key,
                    Access {
                        at_ms: now,
                        refused: Some((a.status, a.code.clone(), a.message.clone())),
                    },
                );
                Err(passed_on(&a))
            }
            Err(e) => Err(self.upstream_failed(&e)),
        }
    }

    fn check_obliterated(&self, depot: &str, id: &ObjectId) -> Result<(), ApiError> {
        let o = lock(&self.0.obliterated);
        if o.get(depot).is_some_and(|d| d.ids.contains(id)) {
            return Err(ApiError::new(
                ErrorCode::Obliterated,
                format!("content {id} was obliterated from this depot"),
            ));
        }
        Ok(())
    }

    /// Waits until no other request fetches `(depot, id)`; the guard lets the next in.
    async fn flight(&self, depot: &str, id: ObjectId) -> Flight {
        let key = (depot.to_owned(), id);
        let m = lock(&self.0.flights)
            .entry(key.clone())
            .or_default()
            .clone();
        let guard = m.clone().lock_owned().await;
        Flight {
            state: self.clone(),
            key,
            mutex: m,
            _guard: guard,
        }
    }

    /// A cached content entry checked against its id; a damaged one is dropped.
    async fn cached_content(&self, key: &Key) -> Option<PathBuf> {
        let path = self.0.cache.lookup(key)?;
        let p = path.clone();
        let ok = tokio::task::spawn_blocking(move || file_id(&p))
            .await
            .ok()
            .and_then(Result::ok)
            == Some(key.id);
        if ok {
            return Some(path);
        }
        tracing::warn!(id = %key.id, "damaged cache entry dropped");
        self.0.cache.remove(key);
        None
    }

    async fn cached_list(&self, key: &Key) -> Option<ChunkList> {
        let path = self.0.cache.lookup(key)?;
        match tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|b| ChunkList::decode(&b).ok())
        {
            Some(l) => Some(l),
            None => {
                self.0.cache.remove(key);
                None
            }
        }
    }

    fn keep_list(&self, depot: &str, id: ObjectId, list: &ChunkList) {
        let tmp = self.0.cache.tmp_path();
        let r = std::fs::write(&tmp, list.encode())
            .and_then(|()| self.0.cache.insert(Key::list(depot, id), &tmp));
        if let Err(e) = r {
            tracing::warn!(error = %e, "cannot cache a chunk list");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// The content `id`: a cached file, or a chunk list, fetching from the upstream once.
    async fn content(
        &self,
        token: Option<&str>,
        depot: &str,
        id: ObjectId,
    ) -> Result<Held, ApiError> {
        let _flight = self.flight(depot, id).await;
        let key = Key::content(depot, id);
        if let Some(p) = self.cached_content(&key).await {
            return Ok(Held::File(p, true));
        }
        if let Some(l) = self.cached_list(&Key::list(depot, id)).await {
            return Ok(Held::Chunked(l));
        }
        let tmp = self.0.cache.tmp_path();
        let fetched = self.client_for(token).fetch_content(depot, &id, &tmp).await;
        match fetched {
            Ok(FetchedContent::Whole(n)) => {
                self.0.metrics.misses.fetch_add(1, Ordering::Relaxed);
                self.0.metrics.bytes_fetched.fetch_add(n, Ordering::Relaxed);
                match self.0.cache.insert(key, &tmp) {
                    Ok(p) => Ok(Held::File(p, false)),
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        tracing::error!(error = %e, "cannot write to the cache");
                        Err(ApiError::internal())
                    }
                }
            }
            Ok(FetchedContent::Chunked(list)) => {
                self.keep_list(depot, id, &list);
                Ok(Held::Chunked(list))
            }
            Err(e) => Err(self.upstream_failed(&e)),
        }
    }

    /// The chunk list of `id`, or `None` when the upstream keeps it whole.
    async fn chunk_list(
        &self,
        token: Option<&str>,
        depot: &str,
        id: ObjectId,
    ) -> Result<Option<ChunkList>, ApiError> {
        let _flight = self.flight(depot, id).await;
        if let Some(l) = self.cached_list(&Key::list(depot, id)).await {
            return Ok(Some(l));
        }
        match self.client_for(token).chunk_list(depot, &id).await {
            Ok(Some(list)) => {
                self.keep_list(depot, id, &list);
                Ok(Some(list))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(self.upstream_failed(&e)),
        }
    }

    /// One chunk's bytes, from the cache or fetched once.
    async fn chunk(
        &self,
        token: Option<&str>,
        depot: &str,
        id: ObjectId,
        size: u32,
    ) -> Result<Bytes, ApiError> {
        let _flight = self.flight(depot, id).await;
        let key = Key::content(depot, id);
        if let Some(p) = self.cached_content(&key).await {
            if let Ok(b) = tokio::fs::read(&p).await {
                self.0.metrics.hits.fetch_add(1, Ordering::Relaxed);
                self.0
                    .metrics
                    .bytes_saved
                    .fetch_add(b.len() as u64, Ordering::Relaxed);
                return Ok(Bytes::from(b));
            }
        }
        let tmp = self.0.cache.tmp_path();
        let client = self.client_for(token);
        if let Err(e) = client
            .download_blob_max(depot, &id, &tmp, u64::from(size))
            .await
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(self.upstream_failed(&e));
        }
        let bytes = tokio::fs::read(&tmp)
            .await
            .map_err(|_| ApiError::internal())?;
        self.0.metrics.misses.fetch_add(1, Ordering::Relaxed);
        self.0
            .metrics
            .bytes_fetched
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if let Err(e) = self.0.cache.insert(key, &tmp) {
            tracing::warn!(error = %e, "cannot cache a chunk");
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(Bytes::from(bytes))
    }
}

/// Held while one request fetches an object.
struct Flight {
    state: ProxyState,
    key: (String, ObjectId),
    mutex: Arc<tokio::sync::Mutex<()>>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for Flight {
    fn drop(&mut self) {
        let mut f = lock(&self.state.0.flights);
        // The map, this flight and its guard hold the mutex; nobody else waits for it.
        if Arc::strong_count(&self.mutex) <= 3 {
            f.remove(&self.key);
        }
    }
}

/// Content the proxy can serve.
enum Held {
    /// A cached file; whether it was there before this request.
    File(PathBuf, bool),
    Chunked(ChunkList),
}

/// The proxy's routes.
pub fn router(state: ProxyState) -> Router {
    Router::new()
        .route(paths::INFO, get(info))
        .route(paths::BLOB, get(blob))
        .route(paths::BLOB_CHUNKS, get(chunks))
        .route("/admin", get(admin_page))
        .route("/admin/", get(admin_page))
        .route("/admin/metrics.json", get(admin_metrics))
        .fallback(fallback)
        .with_state(state)
}

async fn fallback() -> ApiError {
    ApiError::not_found(
        "this is a Lode caching proxy; it serves file content only. Send other requests to \
         the main server",
    )
    .with_details(serde_json::json!({ "reason": "caching_proxy" }))
}

async fn info(State(state): State<ProxyState>) -> Result<Json<Info>, ApiError> {
    let mut info = state
        .0
        .upstream
        .info()
        .await
        .map_err(|e| state.upstream_failed(&e))?;
    info.proxy = Some(ProxyInfo {
        upstream: state.0.upstream_url.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });
    Ok(Json(info))
}

async fn chunks(
    State(state): State<ProxyState>,
    UrlPath((depot, id)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    let token = bearer(&headers);
    state.authorize(token.as_deref(), &depot).await?;
    state.check_obliterated(&depot, &id)?;
    let Some(list) = state.chunk_list(token.as_deref(), &depot, id).await? else {
        return Err(ApiError::not_found(format!("blob {id} is stored whole"))
            .with_details(serde_json::json!({ "reason": "not_chunked" })));
    };
    let mut h = HeaderMap::new();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok((StatusCode::OK, h, list.encode()).into_response())
}

async fn blob(
    State(state): State<ProxyState>,
    UrlPath((depot, id)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    let token = bearer(&headers);
    state.authorize(token.as_deref(), &depot).await?;
    state.check_obliterated(&depot, &id)?;
    let held = state.content(token.as_deref(), &depot, id).await?;
    let total = match &held {
        Held::File(p, _) => tokio::fs::metadata(p)
            .await
            .map_err(|_| ApiError::internal())?
            .len(),
        Held::Chunked(l) => l.total_size(),
    };
    let mut h = HeaderMap::new();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    let range = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let (status, start, len) = match range.map(|r| parse_range(r, total)) {
        None => (StatusCode::OK, 0, total),
        Some(Some((s, e))) => {
            let cr = format!("bytes {s}-{e}/{total}");
            h.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&cr).map_err(|_| ApiError::internal())?,
            );
            (StatusCode::PARTIAL_CONTENT, s, e - s + 1)
        }
        Some(None) => {
            let cr = format!("bytes */{total}");
            h.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&cr).map_err(|_| ApiError::internal())?,
            );
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, h).into_response());
        }
    };
    h.insert(CONTENT_LENGTH, HeaderValue::from(len));
    let body = match held {
        Held::File(p, was_cached) => {
            if was_cached {
                state.0.metrics.hits.fetch_add(1, Ordering::Relaxed);
            }
            let mut f = tokio::fs::File::open(&p)
                .await
                .map_err(|_| ApiError::internal())?;
            f.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|_| ApiError::internal())?;
            file_body(state, f, len, was_cached)
        }
        Held::Chunked(list) => chunked_body(state, token, depot, id, list, start, len),
    };
    Ok((status, h, body).into_response())
}

/// `len` bytes of an open cached file.
fn file_body(state: ProxyState, file: tokio::fs::File, len: u64, saved: bool) -> Body {
    let stream = futures::stream::unfold((file, len), move |(mut f, left)| {
        let state = state.clone();
        async move {
            if left == 0 {
                return None;
            }
            let want = usize::try_from(left.min(FRAME as u64)).unwrap_or(FRAME);
            let mut buf = vec![0u8; want];
            match f.read(&mut buf).await {
                Ok(0) => Some((
                    Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
                    (f, 0),
                )),
                Ok(n) => {
                    buf.truncate(n);
                    if saved {
                        state
                            .0
                            .metrics
                            .bytes_saved
                            .fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Some((Ok(Bytes::from(buf)), (f, left - n as u64)))
                }
                Err(e) => Some((Err(e), (f, 0))),
            }
        }
    });
    Body::from_stream(stream)
}

/// Bytes `start..start+len` of a chunked file, chunk by chunk, each from the cache or
/// fetched once. A whole file is hashed on the way; if it does not match its id the chunk
/// list is dropped and the body broken off, so the client does not keep it.
fn chunked_body(
    state: ProxyState,
    token: Option<String>,
    depot: String,
    id: ObjectId,
    list: ChunkList,
    start: u64,
    len: u64,
) -> Body {
    let whole = start == 0 && len == list.total_size();
    let mut plan: Vec<(ObjectId, u32, usize, usize)> = Vec::new();
    let mut at = 0u64;
    let end = start + len;
    for c in &list.chunks {
        let (c_start, c_end) = (at, at + u64::from(c.size));
        at = c_end;
        if c_end <= start || c_start >= end {
            continue;
        }
        let from = usize::try_from(start.saturating_sub(c_start)).unwrap_or(0);
        let to = usize::try_from(end.min(c_end) - c_start).unwrap_or(0);
        plan.push((c.id, c.size, from, to));
    }
    let hasher = whole.then(blake3::Hasher::new);
    let stream =
        futures::stream::unfold((plan.into_iter(), hasher), move |(mut plan, mut hasher)| {
            let state = state.clone();
            let token = token.clone();
            let depot = depot.clone();
            async move {
                let Some((cid, size, from, to)) = plan.next() else {
                    let h = hasher.take()?;
                    if ObjectId::from_bytes(*h.finalize().as_bytes()) == id {
                        return None;
                    }
                    tracing::warn!(%id, "chunk list does not match its file; dropped");
                    state.0.cache.remove(&Key::list(&depot, id));
                    return Some((
                        Err(std::io::Error::other("content does not match its id")),
                        (plan, None),
                    ));
                };
                match state.chunk(token.as_deref(), &depot, cid, size).await {
                    Ok(b) if b.len() >= to => {
                        if let Some(h) = hasher.as_mut() {
                            h.update(&b);
                        }
                        Some((Ok(b.slice(from..to)), (plan, hasher)))
                    }
                    Ok(_) => Some((
                        Err(std::io::Error::other("chunk shorter than its list says")),
                        (Vec::new().into_iter(), None),
                    )),
                    Err(e) => Some((
                        Err(std::io::Error::other(e.to_string())),
                        (Vec::new().into_iter(), None),
                    )),
                }
            }
        });
    Body::from_stream(stream.boxed())
}

/// Parses a single `bytes=` range against `total` bytes into an inclusive `(start, end)`, as
/// the server does.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') || total == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some((total.saturating_sub(n), total - 1));
    }
    let start: u64 = a.parse().ok()?;
    if start >= total {
        return None;
    }
    let end = if b.is_empty() {
        total - 1
    } else {
        b.parse::<u64>().ok()?.min(total - 1)
    };
    (end >= start).then_some((start, end))
}

async fn admin_metrics(State(state): State<ProxyState>) -> Json<MetricsView> {
    Json(state.metrics())
}

fn human(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if b >= (1u64 << 30) as f64 {
        format!("{:.1} GiB", b / (1u64 << 30) as f64)
    } else if b >= (1u64 << 20) as f64 {
        format!("{:.1} MiB", b / (1u64 << 20) as f64)
    } else {
        format!("{bytes} bytes")
    }
}

async fn admin_page(State(state): State<ProxyState>) -> Html<String> {
    let m = state.metrics();
    let asked = m.hits + m.misses;
    let rate = if asked == 0 {
        "none yet".to_owned()
    } else {
        #[allow(clippy::cast_precision_loss)]
        let r = m.hits as f64 * 100.0 / asked as f64;
        format!("{r:.1}%")
    };
    let upstream = state
        .0
        .upstream_url
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lode caching proxy</title>\
         <meta name=\"viewport\" content=\"width=device-width\">\
         <style>body{{font-family:system-ui,sans-serif;margin:2rem;max-width:40rem}}\
         td{{padding:.25rem 1rem .25rem 0}}td:last-child{{font-variant-numeric:tabular-nums}}</style>\
         </head><body><h1>Lode caching proxy</h1><p>Upstream: {upstream}</p><table>\
         <tr><td>Cache size</td><td>{} of {}</td></tr>\
         <tr><td>Cached objects</td><td>{}</td></tr>\
         <tr><td>Hit rate</td><td>{rate}</td></tr>\
         <tr><td>Bytes saved</td><td>{}</td></tr>\
         <tr><td>Bytes fetched</td><td>{}</td></tr>\
         <tr><td>Upstream errors</td><td>{}</td></tr>\
         </table><p>Counters start at zero when the proxy starts. \
         <a href=\"/admin/metrics.json\">JSON</a></p></body></html>",
        human(m.cache_bytes),
        human(m.cache_max_bytes),
        m.cache_entries,
        human(m.bytes_saved),
        human(m.bytes_fetched),
        m.upstream_errors,
    ))
}

/// A proxy bound to its address, ready to run.
pub struct BoundProxy {
    server: BoundServer,
    state: ProxyState,
}

impl BoundProxy {
    /// Opens the cache and binds the listener.
    pub async fn bind(config: &ProxyConfig, clock: SharedClock) -> Result<Self, ProxyError> {
        let state = ProxyState::new(config, clock)?;
        let app = router(state.clone()).layer(axum::extract::DefaultBodyLimit::max(64 * 1024));
        let server = BoundServer::bind(&config.listener, app).await?;
        Ok(Self { server, state })
    }

    /// Binds with the system clock.
    pub async fn bind_default(config: &ProxyConfig) -> Result<Self, ProxyError> {
        Self::bind(config, Arc::new(SystemClock)).await
    }

    /// The bound address.
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> &'static str {
        self.server.scheme()
    }

    /// The proxy's state.
    pub fn state(&self) -> &ProxyState {
        &self.state
    }

    /// Serves until `shutdown` completes.
    pub async fn run<F>(self, shutdown: F) -> std::io::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.server.run(shutdown).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=95-", 100), Some((95, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(
            bearer(&{
                let mut h = HeaderMap::new();
                h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
                h
            }),
            Some("abc".into())
        );
    }
}
