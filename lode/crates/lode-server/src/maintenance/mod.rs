//! Depot maintenance (ARCHITECTURE §11): garbage collection, consistency checks, backup.

mod gc;

pub use gc::{collect_garbage, GcOptions};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use lode_proto::admin::MaintenanceReport;
use lode_proto::consts::paths;

use crate::auth::{Caller, SCOPE_ADMIN};
use crate::depots::resolve;
use crate::error::ApiResult;
use crate::extract::{ApiPath, ApiQuery};
use crate::state::AppState;

/// Maintenance routes. Server administrators only.
pub fn routes() -> Router<AppState> {
    Router::new().route(paths::ADMIN_GC, post(gc_handler))
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
