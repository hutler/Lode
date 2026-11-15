//! Blob transfer: `objects/missing`, upload and download (ARCHITECTURE §6).

mod io;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_TYPE, ETAG, RANGE, VARY,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use lode_core::{ChunkList, Codec, ErrorCode, ObjectId, ObjectKind, CHUNK_THRESHOLD};
use lode_proto::consts::{paths, ENCODING_ZSTD, MAX_MISSING_QUERY};
use lode_proto::depot::{MissingObjectsRequest, MissingObjectsResponse, PutBlobResponse};
use lode_store::{ChunkListOutcome, PutOptions, PutOutcome, StoreError};

pub(crate) use self::io::reader_stream;
use self::io::{BodyReader, ZstdDecodeReader};
use crate::auth::{Caller, SCOPE_READ, SCOPE_WRITE};
use crate::depots::resolve;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath};
use crate::state::AppState;

/// Scope a module adds for shelving; uploading is part of shelving.
const SCOPE_SHELVE: &str = "shelve";

/// Blob routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::OBJECTS_MISSING, post(missing))
        .route(paths::BLOB, put(upload).get(download))
        .route(
            paths::BLOB_CHUNKS,
            put(upload_chunks)
                .get(download_chunks)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_CHUNK_LIST_BODY)),
        )
        .route(paths::OBLITERATED_IDS, axum::routing::get(obliterated_ids))
}

/// `GET /depots/{d}/obliterated-ids?after=`: what caching proxies must stop serving (T154).
/// Any reader of the depot may ask; only ids are given, never paths or reasons.
async fn obliterated_ids(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: crate::extract::ApiQuery,
) -> ApiResult<Json<lode_proto::depot::ObliteratedIds>> {
    caller.require_scope(SCOPE_READ)?;
    let after = q
        .u64("after")?
        .map_or(i64::MIN, |a| i64::try_from(a).unwrap_or(i64::MAX));
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let now_ms = state.clock().now_ms();
    let rows = state
        .meta()
        .read(move |tx| lode_meta::Reads::obliterations(tx, d.id))
        .await?;
    let mut ids: Vec<ObjectId> = rows
        .into_iter()
        .filter(|r| r.at >= after)
        .map(|r| r.id)
        .collect();
    ids.sort();
    ids.dedup();
    Ok(Json(lode_proto::depot::ObliteratedIds { ids, now_ms }))
}

fn parse_id(s: &str) -> ApiResult<ObjectId> {
    s.parse()
        .map_err(|_| ApiError::invalid_request(format!("`{s}` is not an object id")))
}

/// Whether an existing object is file content, whole or chunked. Tree objects are never
/// offered for download or acknowledged as present.
async fn is_blob(state: &AppState, depot: lode_core::DepotId, id: &ObjectId) -> ApiResult<bool> {
    match state.blobs().get_stored(depot, id).await {
        Ok(s) => Ok(matches!(s.kind, ObjectKind::Blob | ObjectKind::ChunkList)),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

async fn missing(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<MissingObjectsRequest>,
) -> ApiResult<Json<MissingObjectsResponse>> {
    caller.require_any_scope(&[SCOPE_WRITE, SCOPE_SHELVE])?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    if req.ids.len() > MAX_MISSING_QUERY {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!("at most {MAX_MISSING_QUERY} ids per call"),
        ));
    }
    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in req.ids {
        if !seen.insert(id) {
            continue;
        }
        if !state.blobs().contains(d.id, &id).await? || !is_blob(&state, d.id, &id).await? {
            missing.push(id);
        }
    }
    Ok(Json(MissingObjectsResponse { missing }))
}

fn header_has_token(headers: &HeaderMap, name: axum::http::HeaderName, token: &str) -> bool {
    headers.get_all(name).iter().any(|v| {
        v.to_str().ok().is_some_and(|s| {
            s.split(',').any(|part| {
                let mut it = part.split(';');
                let coding = it.next().unwrap_or("").trim();
                let q_zero = it.any(|p| {
                    let p = p.trim();
                    p == "q=0" || p == "q=0.0" || p == "q=0.00" || p == "q=0.000"
                });
                coding.eq_ignore_ascii_case(token) && !q_zero
            })
        })
    })
}

async fn upload(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    request: Request,
) -> ApiResult<(StatusCode, Json<PutBlobResponse>)> {
    caller.require_any_scope(&[SCOPE_WRITE, SCOPE_SHELVE])?;
    let id = parse_id(&id)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    if state.is_obliterated(d.id, &id).await? {
        return Err(ApiError::new(
            ErrorCode::Obliterated,
            format!("content {id} was obliterated from this depot and cannot be stored again"),
        ));
    }
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let encoding = headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());
    let content_length = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let body = BodyReader::new(body);
    let outcome = match encoding.as_deref() {
        None | Some("identity") => {
            let mut raw = body;
            state
                .blobs()
                .put(
                    d.id,
                    ObjectKind::Blob,
                    &id,
                    &mut raw,
                    PutOptions {
                        expected_len: content_length,
                    },
                )
                .await?
        }
        Some(ENCODING_ZSTD) => {
            let mut raw = ZstdDecodeReader::new(body).map_err(|_| ApiError::internal())?;
            state
                .blobs()
                .put(d.id, ObjectKind::Blob, &id, &mut raw, PutOptions::default())
                .await?
        }
        Some(other) => {
            return Err(ApiError::invalid_request(format!(
                "unsupported Content-Encoding `{other}`; use zstd or none"
            )))
        }
    };
    let created = outcome == PutOutcome::Created;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(PutBlobResponse { id, created })))
}

async fn download(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    caller.require_scope(SCOPE_READ)?;
    let id = parse_id(&id)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    if state.is_obliterated(d.id, &id).await? {
        return Err(crate::maintenance::gone(&id));
    }
    let not_found = || ApiError::not_found(format!("no blob {id}"));
    let stored = match state.blobs().get_stored(d.id, &id).await {
        Ok(s) => s,
        Err(StoreError::NotFound(_)) => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    if !matches!(stored.kind, ObjectKind::Blob | ObjectKind::ChunkList) {
        return Err(not_found());
    }
    let mut resp_headers = HeaderMap::new();
    let etag = HeaderValue::from_str(&format!("\"{id}\"")).map_err(|_| ApiError::internal())?;
    resp_headers.insert(ETAG, etag);
    // Content is addressed by its hash and never changes; it is private to authorized callers.
    resp_headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    resp_headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    resp_headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    let range = headers
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let wants_zstd = header_has_token(&headers, ACCEPT_ENCODING, ENCODING_ZSTD);
    if range.is_none()
        && wants_zstd
        && stored.kind == ObjectKind::Blob
        && stored.codec == Codec::Zstd
    {
        resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static(ENCODING_ZSTD));
        resp_headers.insert(CONTENT_LENGTH, HeaderValue::from(stored.stored_len));
        let body = Body::from_stream(reader_stream(stored.reader));
        return Ok((StatusCode::OK, resp_headers, body).into_response());
    }
    drop(stored);
    let total = match state.chunked().get_from(d.id, &id, 0).await {
        Ok(o) => o.raw_len,
        Err(StoreError::NotFound(_)) => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    let (status, start, len) = match range.as_deref().map(|r| parse_range(r, total)) {
        None => (StatusCode::OK, 0, total),
        Some(Some((start, end))) => {
            let cr = format!("bytes {start}-{end}/{total}");
            resp_headers.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&cr).map_err(|_| ApiError::internal())?,
            );
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        Some(None) => {
            let cr = format!("bytes */{total}");
            resp_headers.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&cr).map_err(|_| ApiError::internal())?,
            );
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, resp_headers).into_response());
        }
    };
    let obj = match state.chunked().get_from(d.id, &id, start).await {
        Ok(o) => o,
        Err(StoreError::NotFound(_)) => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    resp_headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    let reader: lode_store::ByteStream = Box::pin(tokio::io::AsyncReadExt::take(obj.reader, len));
    let body = Body::from_stream(reader_stream(reader));
    Ok((status, resp_headers, body).into_response())
}

/// Parses a single `bytes=` range against a file of `total` bytes into an inclusive
/// `(start, end)`. `None` when it cannot be satisfied; several ranges are not supported and
/// count as unsatisfiable.
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

/// Largest chunk list body accepted: enough for files of about a terabyte.
const MAX_CHUNK_LIST_BODY: usize = 40 * 1024 * 1024;

async fn upload_chunks(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<PutBlobResponse>)> {
    caller.require_any_scope(&[SCOPE_WRITE, SCOPE_SHELVE])?;
    let id = parse_id(&id)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    if state.is_obliterated(d.id, &id).await? {
        return Err(ApiError::new(
            ErrorCode::Obliterated,
            format!("content {id} was obliterated from this depot and cannot be stored again"),
        ));
    }
    let list = ChunkList::decode(&body)
        .map_err(|e| ApiError::invalid_request(format!("not a valid chunk list: {e}")))?;
    if list.total_size() < CHUNK_THRESHOLD {
        return Err(ApiError::invalid_request(format!(
            "files under {CHUNK_THRESHOLD} bytes are sent whole"
        )));
    }
    // A commit's object checks hold the fence shared; so does this, so a GC batch cannot
    // delete chunks between the check here and the list being stored.
    let _fence = state.gc_fence().read().await;
    let outcome = match state.chunked().put_chunk_list(d.id, &id, &list).await? {
        ChunkListOutcome::Stored(o) => o,
        ChunkListOutcome::Missing(mut missing) => {
            missing.sort();
            return Err(ApiError::new(
                ErrorCode::MissingObjects,
                format!("{} chunk(s) must be uploaded first", missing.len()),
            )
            .with_details(serde_json::json!({ "ids": missing })));
        }
    };
    let created = outcome == PutOutcome::Created;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(PutBlobResponse { id, created })))
}

async fn download_chunks(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
) -> ApiResult<Response> {
    caller.require_scope(SCOPE_READ)?;
    let id = parse_id(&id)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    if state.is_obliterated(d.id, &id).await? {
        return Err(crate::maintenance::gone(&id));
    }
    let list = match state.blobs().get_stored(d.id, &id).await {
        Ok(s) if s.kind == ObjectKind::ChunkList => {
            drop(s);
            state.chunked().chunk_list(d.id, &id).await?
        }
        Ok(s) if s.kind == ObjectKind::Blob => {
            return Err(ApiError::not_found(format!("blob {id} is stored whole"))
                .with_details(serde_json::json!({ "reason": "not_chunked" })))
        }
        Ok(_) | Err(StoreError::NotFound(_)) => {
            return Err(ApiError::not_found(format!("no blob {id}")))
        }
        Err(e) => return Err(e.into()),
    };
    let Some(list) = list else {
        return Err(ApiError::not_found(format!("no blob {id}")));
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok((StatusCode::OK, headers, list.encode()).into_response())
}
