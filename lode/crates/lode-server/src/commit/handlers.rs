//! `POST /depots/{d}/branches/{b}/commits`.

use axum::extract::State;
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use lode_proto::commit::{CommitRequest, CommitResponse};
use lode_proto::consts::paths;

use super::engine::{commit, CommitOptions};
use crate::auth::{Caller, SCOPE_WRITE};
use crate::depots::resolve;
use crate::error::ApiResult;
use crate::extract::{ApiJson, ApiPath};
use crate::state::AppState;

/// Commit routes.
pub fn routes() -> Router<AppState> {
    Router::new().route(paths::COMMITS, post(create))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    ApiPath((depot, branch)): ApiPath<(String, String)>,
    ApiJson(req): ApiJson<CommitRequest>,
) -> ApiResult<(StatusCode, Json<CommitResponse>)> {
    caller.require_scope(SCOPE_WRITE)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let opts = CommitOptions {
        client: headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        ..CommitOptions::default()
    };
    let resp = commit(&state, &caller, &d, &branch, req, opts).await?;
    let status = if resp.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(resp)))
}
