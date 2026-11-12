//! Blob transfer: `objects/missing`, upload and download (ARCHITECTURE §6).

mod io;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG, VARY,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use lode_core::{Codec, ErrorCode, ObjectId, ObjectKind};
use lode_proto::consts::{paths, ENCODING_ZSTD, MAX_MISSING_QUERY};
use lode_proto::depot::{MissingObjectsRequest, MissingObjectsResponse, PutBlobResponse};
use lode_store::{PutOptions, PutOutcome, StoreError};

use self::io::{reader_stream, BodyReader, ZstdDecodeReader};
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
}

fn parse_id(s: &str) -> ApiResult<ObjectId> {
    s.parse()
        .map_err(|_| ApiError::invalid_request(format!("`{s}` is not an object id")))
}

/// Whether an existing object is a blob. Tree objects are never offered for download or
/// acknowledged as present.
async fn is_blob(state: &AppState, depot: lode_core::DepotId, id: &ObjectId) -> ApiResult<bool> {
    match state.blobs().get_stored(depot, id).await {
        Ok(s) => Ok(s.kind == ObjectKind::Blob),
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
    let not_found = || ApiError::not_found(format!("no blob {id}"));
    let stored = match state.blobs().get_stored(d.id, &id).await {
        Ok(s) => s,
        Err(StoreError::NotFound(_)) => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    if stored.kind != ObjectKind::Blob {
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
    resp_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    let wants_zstd = header_has_token(&headers, ACCEPT_ENCODING, ENCODING_ZSTD);
    let body = if wants_zstd && stored.codec == Codec::Zstd {
        resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static(ENCODING_ZSTD));
        resp_headers.insert(CONTENT_LENGTH, HeaderValue::from(stored.stored_len));
        Body::from_stream(reader_stream(stored.reader))
    } else {
        drop(stored);
        let obj = match state.blobs().get(d.id, &id).await {
            Ok(o) => o,
            Err(StoreError::NotFound(_)) => return Err(not_found()),
            Err(e) => return Err(e.into()),
        };
        resp_headers.insert(CONTENT_LENGTH, HeaderValue::from(obj.raw_len));
        Body::from_stream(reader_stream(obj.reader))
    };
    Ok((StatusCode::OK, resp_headers, body).into_response())
}
