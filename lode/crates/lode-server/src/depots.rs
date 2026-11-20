//! Depots: creation (with branch `main` at `r1`), listing, and resolving a depot for a caller.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::{ObjectKind, Tree};
use lode_meta::{DepotRow, MetaError, Reads, RevisionRow};
use lode_proto::consts::paths;
use lode_proto::depot::{CreateDepotRequest, DepotList, DepotView};
use lode_store::PutOptions;

use crate::auth::{ms_to_time, Caller, SCOPE_READ};
use crate::error::{ApiError, ApiResult};
use crate::extract::ApiJson;
use crate::perm::DepotAccess;
use crate::state::AppState;

/// Name of the branch every depot starts with.
pub const MAIN_BRANCH: &str = "main";

/// Depot routes.
pub fn routes() -> Router<AppState> {
    Router::new().route(paths::DEPOTS, get(list).post(create))
}

/// Looks up a depot the caller may see. A depot that does not exist and one the caller has
/// no access to give the same `not_found`, so names cannot be probed.
pub async fn resolve(
    state: &AppState,
    caller: &Caller,
    name: &str,
) -> ApiResult<(DepotRow, DepotAccess)> {
    let owned = name.to_owned();
    let depot = state
        .meta()
        .read(move |tx| tx.depot_by_name(&owned))
        .await?;
    if let Some(d) = depot {
        let access = state.permissions().depot(caller, d.id);
        if access.is_visible() {
            return Ok((d, access));
        }
    }
    Err(ApiError::not_found(format!("no depot `{name}`")))
}

fn view(d: &DepotRow) -> DepotView {
    DepotView {
        name: d.name.clone(),
        created_at: ms_to_time(d.created_at),
        revisions: None,
        last_commit_at: None,
        size_bytes: None,
    }
}

async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<DepotList>> {
    caller.require_scope(SCOPE_READ)?;
    Ok(Json(list_depots(&state, &caller).await?))
}

/// The depots `caller` can see, as `GET /depots` returns them.
///
/// Server administrators also get each depot's newest revision, its time and the stored size
/// (T179); others do not, since a path rule may hide what those revisions touched.
pub async fn list_depots(state: &AppState, caller: &Caller) -> ApiResult<DepotList> {
    let admin = state.permissions().is_server_admin(caller);
    let rows = state
        .meta()
        .read(move |tx| {
            let mut out = Vec::new();
            for d in tx.depots()? {
                let last = if admin { tx.last_revision(d.id)? } else { None };
                out.push((d, last));
            }
            Ok(out)
        })
        .await?;
    let mut depots = Vec::new();
    for (d, last) in rows
        .iter()
        .filter(|(d, _)| state.permissions().depot(caller, d.id).is_visible())
    {
        let mut v = view(d);
        if admin {
            v.revisions = Some(last.as_ref().map_or(0, |r| r.num));
            v.last_commit_at = last.as_ref().map(|r| ms_to_time(r.created_at));
            v.size_bytes = crate::sizes::depot_size(state, d.id).await;
        }
        depots.push(v);
    }
    Ok(DepotList { depots })
}

fn check_depot_name(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(ApiError::invalid_request(
            "invalid depot name: use 1 to 64 letters, digits, '.', '_' or '-', starting with a \
             letter or digit",
        ))
    }
}

async fn create(
    State(state): State<AppState>,
    caller: Caller,
    ApiJson(req): ApiJson<CreateDepotRequest>,
) -> ApiResult<(StatusCode, Json<DepotView>)> {
    state.permissions().require_server_admin(&caller)?;
    check_depot_name(&req.name)?;
    let depot = create_depot(&state, &caller, &req.name).await?;
    Ok((StatusCode::CREATED, Json(view(&depot))))
}

/// Creates a depot with branch `main` at revision 1 holding the empty tree.
///
/// Runs in two transactions around storing the empty tree. A depot left without branches by
/// a crash in between is completed by the next attempt instead of reported as a duplicate.
pub async fn create_depot(state: &AppState, caller: &Caller, name: &str) -> ApiResult<DepotRow> {
    let now = state.clock().now_ms();
    let owned = name.to_owned();
    let depot = state
        .meta()
        .write(move |tx| {
            if let Some(d) = tx.depot_by_name(&owned)? {
                if tx.branches(d.id)?.is_empty() {
                    return Ok(Ok(d));
                }
                return Ok(Err(ApiError::new(
                    lode_core::ErrorCode::Conflict,
                    format!("depot `{owned}` already exists"),
                )));
            }
            if tx.trashed_depot(&owned)?.is_some() {
                return Ok(Err(ApiError::new(
                    lode_core::ErrorCode::Conflict,
                    format!(
                        "a depot named `{owned}` is in the trash; restore it, or erase it for \
                         good with a forced delete, before using the name again"
                    ),
                )));
            }
            Ok(Ok(tx.create_depot(&owned, now)?))
        })
        .await??;

    let empty = Tree::from_entries(Vec::new()).map_err(|_| ApiError::internal())?;
    let bytes = empty.encode();
    let id = empty.id();
    state
        .blobs()
        .put(
            depot.id,
            ObjectKind::Tree,
            &id,
            &mut &bytes[..],
            PutOptions {
                expected_len: Some(bytes.len() as u64),
            },
        )
        .await?;

    let author = caller.principal.id;
    let d = depot.id;
    state
        .meta()
        .write(move |tx| {
            let num = tx.allocate_rev(d)?;
            let branch = tx.create_branch(d, MAIN_BRANCH, num, None)?;
            tx.insert_revision(&RevisionRow {
                depot_id: d,
                num,
                branch_id: branch,
                parent_num: None,
                root_tree: id,
                author_id: author,
                submitter_id: author,
                created_at: now,
                message: "Create depot".to_owned(),
                provenance: None,
                request_id: None,
            })?;
            Ok(())
        })
        .await
        .map_err(|e| match e {
            MetaError::AlreadyExists(_) => ApiError::new(
                lode_core::ErrorCode::Conflict,
                "depot is being created concurrently",
            ),
            other => other.into(),
        })?;
    Ok(depot)
}
