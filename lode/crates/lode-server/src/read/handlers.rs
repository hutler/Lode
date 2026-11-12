//! Read endpoints.

use axum::extract::State;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use lode_core::EntryKind;
use lode_proto::consts::{paths, HEADER_ACCESS_EPOCH, NDJSON};
use lode_proto::read::{RevisionList, RevisionListQuery, RevisionView, TreeListing};

use super::{
    delta_manifest, full_manifest, list_revisions, list_tree, open_file, readable_rev,
    revision_detail, View, Visibility,
};
use crate::auth::{Caller, SCOPE_READ};
use crate::blobs::reader_stream;
use crate::depots::resolve;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiPath, ApiQuery};
use crate::state::AppState;

/// Header naming the blob id of a file.
pub const HEADER_ID: &str = "x-lode-id";
/// Header naming a file's kind (`file` or `symlink`).
pub const HEADER_KIND: &str = "x-lode-kind";
/// Header saying whether a file is executable (`true` or `false`).
pub const HEADER_EXECUTABLE: &str = "x-lode-executable";

/// Read routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::REVISIONS, get(revisions))
        .route(paths::REVISION, get(revision))
        .route(paths::MANIFEST, get(manifest))
        .route(paths::TREE, get(tree))
        .route(paths::FILES, get(files))
}

fn view(q: &ApiQuery) -> ApiResult<View> {
    View::new(q.all("include"), q.all("exclude"))
}

fn required_rev(q: &ApiQuery) -> ApiResult<u64> {
    q.u64("rev")?
        .ok_or_else(|| ApiError::invalid_request("`rev` is required"))
}

async fn revisions(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<RevisionList>> {
    caller.require_scope(SCOPE_READ)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let query = RevisionListQuery {
        branch: q.get("branch").filter(|b| !b.is_empty()).map(str::to_owned),
        path: q.path("path")?,
        before: q.u64("before")?,
        after: q.u64("after")?,
        own: q.flag("own")?,
        follow: q.flag("follow")?,
        changes: q.flag("changes")?,
        limit: q
            .u64("limit")?
            .map(|l| u32::try_from(l).unwrap_or(u32::MAX)),
    };
    Ok(Json(list_revisions(&state, &caller, &d, query).await?))
}

async fn revision(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, n)): ApiPath<(String, u64)>,
) -> ApiResult<Json<RevisionView>> {
    caller.require_scope(SCOPE_READ)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    Ok(Json(revision_detail(&state, &caller, &d, n).await?))
}

async fn manifest(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Response> {
    caller.require_scope(SCOPE_READ)?;
    // Read the epoch before the trees, so a concurrent ACL change makes the client resync.
    let epoch = state.permissions().access_epoch();
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let view = view(&q)?;
    let target = readable_rev(&state, &caller, d.id, required_rev(&q)?).await?;
    let new_vis = Visibility::new(target.access, view.clone());
    let body = match q.u64("base")? {
        None => full_manifest(state.clone(), d.id, target.rev.root_tree, new_vis),
        Some(base) => {
            let base = readable_rev(&state, &caller, d.id, base).await?;
            let same_rules = base.branch.id == target.branch.id
                || (base.access.reads_everything() && new_vis.is_everything());
            let old_vis = Visibility::new(base.access, view);
            delta_manifest(
                state.clone(),
                d.id,
                base.rev.root_tree,
                target.rev.root_tree,
                old_vis,
                new_vis,
                same_rules,
            )
        }
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(NDJSON));
    let name =
        HeaderName::from_bytes(HEADER_ACCESS_EPOCH.as_bytes()).map_err(|_| ApiError::internal())?;
    headers.insert(name, HeaderValue::from(epoch));
    Ok((StatusCode::OK, headers, body).into_response())
}

async fn tree(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<TreeListing>> {
    caller.require_scope(SCOPE_READ)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let view = view(&q)?;
    let rev = required_rev(&q)?;
    let path = q.path("path")?;
    let target = readable_rev(&state, &caller, d.id, rev).await?;
    let vis = Visibility::new(target.access, view);
    let entries = list_tree(&state, d.id, target.rev.root_tree, path.as_ref(), &vis).await?;
    Ok(Json(TreeListing { rev, path, entries }))
}

async fn files(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Response> {
    caller.require_scope(SCOPE_READ)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let rev = required_rev(&q)?;
    let path = q
        .path("path")?
        .ok_or_else(|| ApiError::invalid_request("`path` is required"))?;
    let target = readable_rev(&state, &caller, d.id, rev).await?;
    let vis = Visibility::new(target.access, View::default());
    let file = open_file(&state, d.id, target.rev.root_tree, &path, &vis).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(file.len));
    let id = HeaderValue::from_str(&file.entry.id.to_string()).map_err(|_| ApiError::internal())?;
    headers.insert(HeaderName::from_static(HEADER_ID), id);
    headers.insert(
        HeaderName::from_static(HEADER_KIND),
        HeaderValue::from_static(match file.entry.kind {
            EntryKind::Symlink => "symlink",
            _ => "file",
        }),
    );
    headers.insert(
        HeaderName::from_static(HEADER_EXECUTABLE),
        HeaderValue::from_static(if file.entry.executable() {
            "true"
        } else {
            "false"
        }),
    );
    Ok((
        StatusCode::OK,
        headers,
        axum::body::Body::from_stream(reader_stream(file.reader)),
    )
        .into_response())
}
