//! Deleting depots: to the trash, out of it again, and for good.
//!
//! A deleted depot goes to the trash first. It disappears from every list and lookup, keeps
//! its name, and can be restored. After [`TRASH_DAYS`] days, or at once with `force`, it is
//! purged: its content is removed from storage, then its history and the modules' rows from
//! the database, in that order, so an interrupted purge is finished by the next attempt.
//! The depot's row stays behind as a renamed tombstone so its id is never reused.

use std::time::Duration;

use axum::extract::State;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use lode_core::{DepotId, ErrorCode};
use lode_meta::{DepotRow, Millis, Reads};
use lode_proto::admin::{DeleteDepotRequest, DeleteDepotResponse, TrashList, TrashedDepot};
use lode_proto::consts::paths;
use lode_proto::depot::DepotView;

use crate::auth::{ms_to_time, Caller, SCOPE_ADMIN};
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath};
use crate::module::Stop;
use crate::state::AppState;

/// Days a deleted depot stays in the trash before it is purged.
pub const TRASH_DAYS: i64 = 30;

const DAY_MS: Millis = 24 * 60 * 60 * 1000;

/// Trash routes. Server administrators only.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::ADMIN_DEPOT, delete(delete_handler))
        .route(paths::ADMIN_DEPOT_RESTORE, post(restore_handler))
        .route(paths::ADMIN_TRASH, get(list_handler))
}

/// When a depot deleted at `deleted_at` is purged.
pub fn purge_after(deleted_at: Millis) -> Millis {
    deleted_at.saturating_add(TRASH_DAYS * DAY_MS)
}

fn require_admin(state: &AppState, caller: &Caller) -> ApiResult<()> {
    caller.require_scope(SCOPE_ADMIN)?;
    state.permissions().require_server_admin(caller)
}

enum Found {
    Live(DepotRow),
    Trashed(DepotRow),
}

async fn find(state: &AppState, name: &str) -> ApiResult<Found> {
    let owned = name.to_owned();
    let found = state
        .meta()
        .read(move |tx| {
            Ok(match tx.depot_by_name(&owned)? {
                Some(d) => Some(Found::Live(d)),
                None => tx.trashed_depot(&owned)?.map(Found::Trashed),
            })
        })
        .await?;
    found.ok_or_else(|| ApiError::not_found(format!("no depot `{name}`")))
}

async fn delete_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(name): ApiPath<String>,
    ApiJson(req): ApiJson<DeleteDepotRequest>,
) -> ApiResult<Json<DeleteDepotResponse>> {
    require_admin(&state, &caller)?;
    if req.confirm != name {
        return Err(ApiError::invalid_request(format!(
            "to confirm, type the depot's name `{name}` exactly"
        )));
    }
    let found = find(&state, &name).await?;
    let _backup = if req.force {
        crate::maintenance::backup_lock::for_removal(&state)?
    } else {
        None
    };
    let d = match (found, req.force) {
        (Found::Live(d), false) => {
            let now = state.clock().now_ms();
            let id = d.id;
            state
                .meta()
                .write(move |tx| tx.trash_depot(id, now))
                .await?;
            tracing::info!(depot = %name, by = %caller.principal.name, "depot moved to the trash");
            return Ok(Json(DeleteDepotResponse {
                name,
                purged: false,
                purge_after: Some(ms_to_time(purge_after(now))),
            }));
        }
        (Found::Trashed(_), false) => {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!(
                    "depot `{name}` is already in the trash; delete it with force to erase it now"
                ),
            ));
        }
        (Found::Live(d), true) => {
            // Into the trash first, so it stops answering before its content goes.
            let now = state.clock().now_ms();
            let id = d.id;
            state
                .meta()
                .write(move |tx| tx.trash_depot(id, now))
                .await?;
            d
        }
        (Found::Trashed(d), true) => d,
    };
    purge_objects_and_rows(&state, d.id).await?;
    tracing::info!(depot = %name, by = %caller.principal.name, "depot erased");
    Ok(Json(DeleteDepotResponse {
        name,
        purged: true,
        purge_after: None,
    }))
}

async fn restore_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<Json<DepotView>> {
    require_admin(&state, &caller)?;
    let d = match find(&state, &name).await? {
        Found::Trashed(d) => d,
        Found::Live(_) => {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("depot `{name}` is not in the trash"),
            ))
        }
    };
    let id = d.id;
    state.meta().write(move |tx| tx.restore_depot(id)).await?;
    tracing::info!(depot = %name, by = %caller.principal.name, "depot restored from the trash");
    Ok(Json(DepotView {
        name: d.name,
        created_at: ms_to_time(d.created_at),
        revisions: None,
        last_commit_at: None,
        size_bytes: None,
    }))
}

async fn list_handler(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<TrashList>> {
    require_admin(&state, &caller)?;
    let rows = state.meta().read(|tx| tx.trashed_depots()).await?;
    let depots = rows
        .into_iter()
        .map(|d| {
            let deleted = d.deleted_at.unwrap_or(d.created_at);
            TrashedDepot {
                name: d.name,
                created_at: ms_to_time(d.created_at),
                deleted_at: ms_to_time(deleted),
                purge_after: ms_to_time(purge_after(deleted)),
            }
        })
        .collect();
    Ok(Json(TrashList { depots }))
}

/// Erases a depot that is in the trash: its stored content, then its history and every
/// module's rows. Safe to run again after an interruption.
pub async fn purge(state: &AppState, depot: DepotId) -> ApiResult<()> {
    let _backup = crate::maintenance::backup_lock::for_removal(state)?;
    purge_objects_and_rows(state, depot).await
}

async fn purge_objects_and_rows(state: &AppState, depot: DepotId) -> ApiResult<()> {
    state.blobs().delete_depot(depot).await?;
    let modules = state.modules().to_vec();
    let now = state.clock().now_ms();
    state
        .meta()
        .write(move |tx| {
            for m in &modules {
                m.purge_depot(tx, depot)?;
            }
            tx.purge_depot_rows(depot, now)
        })
        .await?;
    state.permissions().reload(state.meta()).await?;
    Ok(())
}

/// Purges the depots that have been in the trash for [`TRASH_DAYS`] days. Returns the names
/// of those purged; a depot that fails is logged and tried again next time.
pub async fn purge_due(state: &AppState) -> ApiResult<Vec<String>> {
    let now = state.clock().now_ms();
    let due: Vec<DepotRow> = state
        .meta()
        .read(|tx| tx.trashed_depots())
        .await?
        .into_iter()
        .filter(|d| d.deleted_at.is_some_and(|t| purge_after(t) <= now))
        .collect();
    let mut done = Vec::new();
    for d in due {
        match purge(state, d.id).await {
            Ok(()) => {
                tracing::info!(depot = %d.name, "depot erased after its time in the trash");
                done.push(d.name);
            }
            Err(e) => tracing::error!(depot = %d.name, error = %e, "cannot erase the depot"),
        }
    }
    Ok(done)
}

/// Purges depots whose time in the trash is over, checking every hour until `stop` fires.
pub async fn run_purge(state: AppState, mut stop: Stop) {
    const EVERY: Duration = Duration::from_secs(60 * 60);
    loop {
        if let Err(e) = purge_due(&state).await {
            tracing::error!(error = %e, "cannot read the trash");
        }
        tokio::select! {
            () = tokio::time::sleep(EVERY) => {}
            () = stop.wait() => return,
        }
    }
}
