//! Branches: list, create (ARCHITECTURE §5.3), show and archive.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::ErrorCode;
use lode_meta::{BranchRow, BranchState, MetaError, PrincipalKind, Reads, RevisionRow};
use lode_proto::consts::paths;
use lode_proto::depot::{
    BranchList, BranchPoint, BranchView, CreateBranchRequest, MergeInfo, MergedRange,
    PatchBranchRequest,
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
        .route(paths::MERGE_INFO, get(merge_info))
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
    let include_archived = q.flag("all")?;
    Ok(Json(
        list_branches(&state, &caller, &depot, include_archived).await?,
    ))
}

/// The branches of `depot` that `caller` can read, as `GET /branches` returns them.
pub async fn list_branches(
    state: &AppState,
    caller: &Caller,
    depot: &str,
    include_archived: bool,
) -> ApiResult<BranchList> {
    let (d, access) = resolve(state, caller, depot).await?;
    let id = d.id;
    let all = state.meta().read(move |tx| tx.branches(id)).await?;
    let visible = all
        .iter()
        .filter(|b| {
            (include_archived || b.state == BranchState::Active)
                && access.branch(&b.name).reads_anything()
        })
        .cloned();
    Ok(BranchList {
        branches: views(&all, visible),
    })
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

async fn merge_info(
    State(state): State<AppState>,
    caller: Caller,
    ApiPath((depot, target)): ApiPath<(String, String)>,
    q: ApiQuery,
) -> ApiResult<Json<MergeInfo>> {
    caller.require_scope(SCOPE_READ)?;
    let source = q
        .get("source")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::invalid_request("`source` is required"))?;
    let (d, access) = resolve(&state, &caller, &depot).await?;
    for b in [&target, &source] {
        if !access.branch(b).reads_anything() {
            return Err(ApiError::not_found(format!("no branch `{b}`")));
        }
    }
    if target == source {
        return Err(ApiError::invalid_request(
            "a branch cannot be merged into itself",
        ));
    }
    let id = d.id;
    let info = state
        .meta()
        .read(move |tx| {
            let (Some(t), Some(s)) = (tx.branch(id, &target)?, tx.branch(id, &source)?) else {
                return Ok(None);
            };
            let t_chain = tx.branch_chain(t.id)?;
            let s_chain = tx.branch_chain(s.id)?;
            // The newest shared stretch of history; its end is the branch point.
            let point = t_chain.iter().find_map(|ts| {
                s_chain
                    .iter()
                    .find(|ss| ss.branch_id == ts.branch_id)
                    .map(|ss| match (ts.max_rev, ss.max_rev) {
                        (Some(a), Some(b)) => a.min(b),
                        (Some(a), None) | (None, Some(a)) => a,
                        (None, None) => 0,
                    })
            });
            let merged = tx.merges_from(id, &t_chain, s.id)?;
            let revs = match point {
                Some(p) => tx.chain_revs_after(id, &s_chain, p)?,
                None => Vec::new(),
            };
            Ok(Some((t, s, point, merged, revs)))
        })
        .await?;
    let Some((t, s, point, merged, revs)) = info else {
        return Err(ApiError::not_found("no such branch"));
    };
    let Some(point) = point else {
        return Err(ApiError::invalid_request(format!(
            "`{}` and `{}` share no history",
            s.name, t.name
        )));
    };
    Ok(Json(merge_base(
        &t.name, &s.name, point, s.head_rev, &merged, &revs,
    )))
}

/// SPEC §5.4: the base is the highest source revision `R` such that every source revision
/// after the branch point up to `R` is inside a recorded range; the branch point otherwise.
fn merge_base(
    target: &str,
    source: &str,
    point: u64,
    source_head: u64,
    merged: &[(u64, lode_meta::MergeRow)],
    source_revs: &[u64],
) -> MergeInfo {
    let covered = |r: u64| merged.iter().any(|(_, m)| m.from_rev <= r && r <= m.to_rev);
    let done = source_revs.iter().take_while(|r| covered(**r)).count();
    let base = if done == 0 {
        point
    } else {
        source_revs[done - 1]
    };
    MergeInfo {
        target: target.to_owned(),
        source: source.to_owned(),
        branch_point: point,
        merged: merged
            .iter()
            .map(|(rev, m)| MergedRange {
                rev: *rev,
                from_rev: m.from_rev,
                to_rev: m.to_rev,
            })
            .collect(),
        base,
        source_head,
        pending: (source_revs.len() - done) as u64,
    }
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
                    seg.branch_id == r.branch_id && seg.max_rev.is_none_or(|m| r.num <= m)
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

#[cfg(test)]
mod tests {
    use super::merge_base;
    use lode_meta::{BranchId, MergeRow};

    fn m(from_rev: u64, to_rev: u64) -> (u64, MergeRow) {
        (
            100,
            MergeRow {
                source_branch_id: BranchId(2),
                from_rev,
                to_rev,
            },
        )
    }

    #[test]
    fn base_follows_the_covered_prefix() {
        let revs = [5, 7, 9, 12];
        let none = merge_base("main", "feat", 3, 12, &[], &revs);
        assert_eq!((none.base, none.pending), (3, 4));
        let some = merge_base("main", "feat", 3, 12, &[m(4, 9)], &revs);
        assert_eq!((some.base, some.pending), (9, 1));
        // A pick of 12 alone leaves the gap at 5..9: the base stays at the branch point.
        let gap = merge_base("main", "feat", 3, 12, &[m(12, 12)], &revs);
        assert_eq!((gap.base, gap.pending), (3, 4));
        let all = merge_base("main", "feat", 3, 12, &[m(4, 9), m(10, 12)], &revs);
        assert_eq!((all.base, all.pending), (12, 0));
        assert_eq!(all.merged.len(), 2);
    }
}
