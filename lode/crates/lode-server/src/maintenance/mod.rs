//! Depot maintenance (ARCHITECTURE §11): garbage collection, consistency checks, backup,
//! moving content to S3 and metadata to Postgres.

mod backup;
mod fsck;
mod gc;
mod migrate;
mod migrate_db;
mod obliterate;
mod schedule;

pub use backup::{backup, BackupError, BackupReport, DATABASE_NOTE_FILE};
pub use fsck::{fsck, FsckOptions};
pub use gc::{collect_garbage, GcOptions};
pub use migrate::{migrate_to_s3, MigrateError, MigrateOptions, MigrateReport};
pub use migrate_db::{migrate_db_to_postgres, MigrateDbError, MigrateDbReport};
pub use obliterate::{gone, missing_object, obliterate, obliterations, MAX_REASON, MAX_TARGETS};
pub use schedule::{
    backup_tick, newest_backup, run_backups, BackupSchedule, BackupTick, BACKUP_PREFIX,
};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use lode_proto::admin::{MaintenanceReport, ObliterateReport, ObliterateRequest, ObliterationList};
use lode_proto::consts::paths;

use crate::auth::{Caller, SCOPE_ADMIN};
use crate::depots::resolve;
use crate::error::ApiResult;
use crate::extract::{ApiJson, ApiPath, ApiQuery};
use crate::state::AppState;

/// Records a finished collection or check as the depot's last one of `kind`. A failure to
/// record is logged; the run itself succeeded.
async fn record_run(
    state: &AppState,
    depot: lode_core::DepotId,
    kind: &str,
    r: &MaintenanceReport,
) {
    let row = lode_meta::MaintenanceRunRow {
        depot_id: depot,
        kind: kind.to_owned(),
        at: state.clock().now_ms(),
        objects_checked: r.objects_checked,
        objects_affected: r.objects_affected,
        bytes_affected: r.bytes_affected,
        problems: r.problems.len() as u64,
    };
    if let Err(e) = state
        .meta()
        .write(move |tx| tx.record_maintenance_run(&row))
        .await
    {
        tracing::warn!(error = %e, depot = depot.0, kind, "cannot record the maintenance run");
    }
}

/// Maintenance routes. Server administrators only.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::ADMIN_GC, post(gc_handler))
        .route(paths::ADMIN_FSCK, post(fsck_handler))
        .route(paths::ADMIN_OBLITERATE, post(obliterate_handler))
        .route(paths::ADMIN_OBLITERATED, get(obliterated_handler))
}

async fn obliterate_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<ObliterateRequest>,
) -> ApiResult<Json<ObliterateReport>> {
    caller.require_scope(SCOPE_ADMIN)?;
    state.permissions().require_server_admin(&caller)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    Ok(Json(
        obliterate(&state, d.id, caller.principal.id, req).await?,
    ))
}

async fn obliterated_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
) -> ApiResult<Json<ObliterationList>> {
    caller.require_scope(SCOPE_ADMIN)?;
    state.permissions().require_server_admin(&caller)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    Ok(Json(ObliterationList {
        obliterated: obliterations(&state, d.id).await?,
    }))
}

async fn fsck_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<MaintenanceReport>> {
    caller.require_scope(SCOPE_ADMIN)?;
    state.permissions().require_server_admin(&caller)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let opts = FsckOptions {
        full: q.flag("full")?,
    };
    Ok(Json(fsck(&state, d.id, opts).await?))
}

async fn gc_handler(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<MaintenanceReport>> {
    caller.require_scope(SCOPE_ADMIN)?;
    state.permissions().require_server_admin(&caller)?;
    let (d, _) = resolve(&state, &caller, &depot).await?;
    let opts = GcOptions {
        dry_run: q.flag("dry_run")?,
    };
    Ok(Json(collect_garbage(&state, d.id, opts).await?))
}
