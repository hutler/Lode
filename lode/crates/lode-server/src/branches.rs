//! Branches: list, create (ARCHITECTURE §5.3), show and archive.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::ErrorCode;
use lode_meta::{BranchRow, BranchState, MetaError, PrincipalKind, Reads, RevisionRow};
use lode_proto::consts::paths;
use lode_proto::depot::{
    BranchList, BranchPoint, BranchView, CreateBranchRequest, PatchBranchRequest,
};
use serde_json::json;

use crate::auth::{Caller, SCOPE_ADMIN, SCOPE_READ, SCOPE_WRITE};
use crate::depots::resolve;
use crate::error::{ApiError, ApiResult};
use crate::extract::{ApiJson, ApiPath, ApiQuery};
use crate::perm::Level;
use crate::state::AppState;

/// Longest branch name.
pub const MAX_BRANCH_NAME: usize = 200;

/// Branch routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(paths::BRANCHES, get(list).post(create))
        .route(paths::BRANCH, get(show).patch(patch))
}

/// Checks a new branch name: `[A-Za-z0-9._/-]`, at most 200 characters, no empty, `.` or
/// `..` segment (so no leading, trailing or doubled `/`).
pub fn validate_branch_name(name: &str) -> Result<(), ApiError> {
    let bad = |why: &str| ApiError::invalid_request(format!("invalid branch name `{name}`: {why}"));
    if name.is_empty() || name.len() > MAX_BRANCH_NAME {
        return Err(bad("must have 1 to 200 characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
    {
        return Err(bad(
            "only letters, digits, `.`, `_`, `/` and `-` are allowed",
        ));
    }
    if name
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(bad(
            "segments between `/` must be non-empty and not `.` or `..`",
        ));
    }
    Ok(())
}

/// The data of a `revision.created` event (ARCHITECTURE §8.1).
pub(crate) fn revision_created(
    rev: u64,
    branch: &str,
    author: &lode_meta::PrincipalRow,
    submitter: &lode_meta::PrincipalRow,
    origin: &str,
) -> String {
    json!({
        "rev": rev,
        "branch": branch,
        "author": author.name,
        "author_kind": match author.kind {
            PrincipalKind::User => "user",
            PrincipalKind::Agent => "agent",
        },
        "submitter": submitter.name,
        "origin": origin,
    })
    .to_string()
}

fn view(b: &BranchRow, parent_name: Option<String>) -> BranchView {
    BranchView {
        name: b.name.clone(),
        head_rev: b.head_rev,
        created_rev: b.created_rev,
        parent: match (parent_name, b.parent_rev) {
            (Some(branch), Some(rev)) => Some(BranchPoint { branch, rev }),
            _ => None,
        },
        state: match b.state {
            BranchState::Active => lode_proto::depot::BranchState::Active,
            BranchState::Archived => lode_proto::depot::BranchState::Archived,
        },
    }
}

fn views(all: &[BranchRow], rows: impl Iterator<Item = BranchRow>) -> Vec<BranchView> {
    rows.map(|b| {
        let parent = b
            .parent_branch_id
            .and_then(|p| all.iter().find(|x| x.id == p))
            .map(|p| p.name.clone());
        view(&b, parent)
    })
    .collect()
}

async fn list(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<BranchList>> {
    caller.require_scope(SCOPE_READ)?;
    let (d, access) = resolve(&state, &caller, &depot).await?;
    let include_archived = q.flag("all")?;
    let id = d.id;
    let all = state.meta().read(move |tx| tx.branches(id)).await?;
    let visible = all
        .iter()
        .filter(|b| {
            (include_archived || b.state == BranchState::Active)
                && access.branch(&b.name).reads_anything()
        })
        .cloned();
    Ok(Json(BranchList {
        branches: views(&all, visible),
    }))
}

async fn show(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, branch)): ApiPath<(String, String)>,
) -> ApiResult<Json<BranchView>> {
    caller.require_scope(SCOPE_READ)?;
    let (d, access) = resolve(&state, &caller, &depot).await?;
    let id = d.id;
    let all = state.meta().read(move |tx| tx.branches(id)).await?;
    let found = all
        .iter()
        .find(|b| b.name == branch && access.branch(&b.name).reads_anything())
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("no branch `{branch}`")))?;
    Ok(Json(views(&all, std::iter::once(found)).remove(0)))
}

enum Created {
    Done(BranchRow, String),
    Rejected(ApiError),
}

async fn create(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<CreateBranchRequest>,
) -> ApiResult<(StatusCode, Json<BranchView>)> {
    caller.require_scope(SCOPE_WRITE)?;
    let (d, access) = resolve(&state, &caller, &depot).await?;
    validate_branch_name(&req.name)?;
    if !access.branch(&req.name).grants_somewhere(Level::Write) {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!("no write access to branch `{}`", req.name),
        ));
    }
    let source_access = access.branch(&req.from_branch);
    if !source_access.reads_anything() {
        return Err(ApiError::not_found(format!(
            "no branch `{}`",
            req.from_branch
        )));
    }

    let id = d.id;
    let now = state.clock().now_ms();
    let author = caller.principal.clone();
    let outcome = state
        .meta()
        .write(move |tx| {
            let reject = |e: ApiError| Ok(Created::Rejected(e));
            let branches = tx.branches(id)?;
            let folded = req.name.to_lowercase();
            if branches.iter().any(|b| b.name.to_lowercase() == folded) {
                return reject(ApiError::new(
                    ErrorCode::Conflict,
                    format!("branch `{}` already exists", req.name),
                ));
            }
            let Some(src) = branches.iter().find(|b| b.name == req.from_branch) else {
                return reject(ApiError::not_found(format!(
                    "no branch `{}`",
                    req.from_branch
                )));
            };
            let from_rev = req.from_rev.unwrap_or(src.head_rev);
            // The source revision must lie in the source branch's history (§4.3).
            let chain = tx.branch_chain(src.id)?;
            let source = tx.revision(id, from_rev)?.filter(|r| {
                chain.iter().any(|seg| {
                    seg.branch_id == r.branch_id && seg.max_rev.map_or(true, |m| r.num <= m)
                }) && r.num <= src.head_rev
            });
            let Some(source) = source else {
                return reject(ApiError::invalid_request(format!(
                    "revision {from_rev} is not in the history of `{}`",
                    req.from_branch
                )));
            };
            let num = tx.allocate_rev(id)?;
            let branch_id = tx.create_branch(id, &req.name, num, Some((src.id, from_rev)))?;
            tx.insert_revision(&RevisionRow {
                depot_id: id,
                num,
                branch_id,
                parent_num: Some(from_rev),
                root_tree: source.root_tree,
                author_id: author.id,
                submitter_id: author.id,
                created_at: now,
                message: format!(
                    "Create branch {} from {}@{from_rev}",
                    req.name, req.from_branch
                ),
                provenance: None,
                request_id: None,
            })?;
            let rev_event = revision_created(num, &req.name, &author, &author, "direct");
            tx.append_outbox(Some(id), "revision.created", &rev_event, now)?;
            let branch_event = json!({
                "branch": req.name,
                "rev": num,
                "from_branch": req.from_branch,
                "from_rev": from_rev,
            });
            tx.append_outbox(Some(id), "branch.created", &branch_event.to_string(), now)?;
            let row = tx
                .branch_by_id(branch_id)?
                .ok_or_else(|| MetaError::NotFound("new branch".into()))?;
            Ok(Created::Done(row, src.name.clone()))
        })
        .await;
    match outcome {
        Ok(Created::Done(row, parent)) => {
            state.outbox_notify().notify_one();
            Ok((StatusCode::CREATED, Json(view(&row, Some(parent)))))
        }
        Ok(Created::Rejected(e)) => Err(e),
        Err(MetaError::AlreadyExists(_)) => {
            Err(ApiError::new(ErrorCode::Conflict, "branch already exists"))
        }
        Err(e) => Err(e.into()),
    }
}

async fn patch(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, branch)): ApiPath<(String, String)>,
    ApiJson(req): ApiJson<PatchBranchRequest>,
) -> ApiResult<Json<BranchView>> {
    caller.require_scope(SCOPE_ADMIN)?;
    let (d, access) = resolve(&state, &caller, &depot).await?;
    if !access.is_depot_admin() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "archiving a branch needs admin on the depot",
        ));
    }
    let id = d.id;
    let now = state.clock().now_ms();
    let target = req.state.map(|s| match s {
        lode_proto::depot::BranchState::Active => BranchState::Active,
        lode_proto::depot::BranchState::Archived => BranchState::Archived,
    });
    // Commits hold the branch lock while they check the state, so take it here too.
    let lock = state.branch_lock(id, &branch);
    let _guard = lock.lock().await;
    let name = branch.clone();
    let (row, all, changed) = state
        .meta()
        .write(move |tx| {
            let Some(b) = tx.branch(id, &name)? else {
                return Ok(None);
            };
            let mut changed = false;
            if let Some(t) = target {
                if t != b.state {
                    tx.set_branch_state(b.id, t)?;
                    changed = true;
                    if t == BranchState::Archived {
                        let data = json!({ "branch": b.name });
                        tx.append_outbox(Some(id), "branch.archived", &data.to_string(), now)?;
                    }
                }
            }
            let row = tx.branch_by_id(b.id)?;
            let all = tx.branches(id)?;
            Ok(row.map(|r| (r, all, changed)))
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no branch `{branch}`")))?;
    if changed {
        state.outbox_notify().notify_one();
    }
    Ok(Json(views(&all, std::iter::once(row)).remove(0)))
}
