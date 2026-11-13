//! Revision list and revision detail.

use std::collections::HashMap;

use lode_core::{ChangeAction, CopyFrom, DepotId, DepotPath, EntryKind, ErrorCode};
use lode_meta::{
    BranchId, ChangeRow, DepotRow, HistorySegment, MetaError, PrincipalId, PrincipalKind, Reads,
    RevisionFilter, RevisionRow,
};
use lode_proto::commit::{MergedFrom, Provenance};
use lode_proto::consts::{DEFAULT_REVISION_PAGE, MAX_REVISION_PAGE};
use lode_proto::read::{ChangeView, RevisionList, RevisionListQuery, RevisionView};
use lode_proto::PrincipalRef;

use crate::auth::{ms_to_time, Caller};
use crate::error::{ApiError, ApiResult};
use crate::perm::{BranchAccess, DepotAccess, Level};
use crate::state::AppState;

/// A rename chain can be long, but not unbounded.
const MAX_FOLLOW_STEPS: usize = 1_000;

/// One stretch of history to search: a path (or none) within some branches and a range.
#[derive(Debug, Clone)]
struct Scope {
    segments: Option<Vec<HistorySegment>>,
    path: Option<DepotPath>,
    min: Option<u64>,
    max: Option<u64>,
}

/// Errors inside the read transaction: a database error, or an answer for the client.
enum Fail {
    Meta(MetaError),
    Api(ApiError),
}

impl From<MetaError> for Fail {
    fn from(e: MetaError) -> Self {
        Fail::Meta(e)
    }
}

type TxResult<T> = Result<Result<T, ApiError>, MetaError>;

fn finish<T>(r: Result<T, Fail>) -> TxResult<T> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(Fail::Api(e)) => Ok(Err(e)),
        Err(Fail::Meta(e)) => Err(e),
    }
}

/// How a revision names a principal: name, kind, and an agent's owner.
pub(super) fn principal_ref(tx: &dyn Reads, id: PrincipalId) -> Result<PrincipalRef, MetaError> {
    Ok(match tx.principal(id)? {
        Some(p) => {
            let owner = match (p.kind, p.owner_id) {
                (PrincipalKind::Agent, Some(o)) => tx.principal(o)?.map(|o| o.name),
                _ => None,
            };
            PrincipalRef {
                name: p.name,
                kind: match p.kind {
                    PrincipalKind::User => lode_proto::PrincipalKind::User,
                    PrincipalKind::Agent => lode_proto::PrincipalKind::Agent,
                },
                owner,
            }
        }
        None => PrincipalRef {
            name: String::new(),
            kind: lode_proto::PrincipalKind::User,
            owner: None,
        },
    })
}

/// Caches what one listing needs repeatedly: branch names and access, principal names.
struct Lookup<'a> {
    depot: DepotId,
    access: &'a DepotAccess,
    branches: HashMap<BranchId, Option<(String, BranchAccess)>>,
    principals: HashMap<PrincipalId, PrincipalRef>,
    branch_names: HashMap<BranchId, String>,
}

impl<'a> Lookup<'a> {
    fn new(depot: DepotId, access: &'a DepotAccess) -> Self {
        Self {
            depot,
            access,
            branches: HashMap::new(),
            principals: HashMap::new(),
            branch_names: HashMap::new(),
        }
    }

    /// The branch's name and access, or `None` when the caller cannot read it at all.
    fn branch(
        &mut self,
        tx: &dyn Reads,
        id: BranchId,
    ) -> Result<Option<(String, BranchAccess)>, MetaError> {
        if let Some(b) = self.branches.get(&id) {
            return Ok(b.clone());
        }
        let found = tx.branch_by_id(id)?.and_then(|b| {
            let access = self.access.branch(&b.name);
            access.reads_anything().then_some((b.name, access))
        });
        self.branches.insert(id, found.clone());
        Ok(found)
    }

    fn branch_name(&mut self, tx: &dyn Reads, id: BranchId) -> Result<String, MetaError> {
        if let Some(n) = self.branch_names.get(&id) {
            return Ok(n.clone());
        }
        let name = tx.branch_by_id(id)?.map(|b| b.name).unwrap_or_default();
        self.branch_names.insert(id, name.clone());
        Ok(name)
    }

    fn principal(&mut self, tx: &dyn Reads, id: PrincipalId) -> Result<PrincipalRef, MetaError> {
        if let Some(p) = self.principals.get(&id) {
            return Ok(p.clone());
        }
        let p = principal_ref(tx, id)?;
        self.principals.insert(id, p.clone());
        Ok(p)
    }

    /// Builds the view of a revision. `changes` caps the listed paths; `None` leaves them out.
    fn view(
        &mut self,
        tx: &dyn Reads,
        r: &RevisionRow,
        branch: String,
        access: &BranchAccess,
        changes: Option<usize>,
    ) -> Result<RevisionView, MetaError> {
        let mut merged_from = Vec::new();
        for m in tx.merges(self.depot, r.num)? {
            merged_from.push(MergedFrom {
                branch: self.branch_name(tx, m.source_branch_id)?,
                from_rev: m.from_rev,
                to_rev: m.to_rev,
            });
        }
        let (changes, changes_truncated) = match changes {
            None => (None, false),
            Some(cap) => {
                let mut out = Vec::new();
                let mut truncated = false;
                let mut readable = access.read_filter();
                for row in tx.changes(self.depot, r.num)? {
                    let Some(view) = change_view(&row) else {
                        continue;
                    };
                    if !readable(&view.path) {
                        continue;
                    }
                    if out.len() == cap {
                        truncated = true;
                        break;
                    }
                    out.push(view);
                }
                (Some(out), truncated)
            }
        };
        Ok(RevisionView {
            rev: r.num,
            branch,
            parent: r.parent_num,
            author: self.principal(tx, r.author_id)?,
            submitter: self.principal(tx, r.submitter_id)?,
            created_at: ms_to_time(r.created_at),
            message: r.message.clone(),
            root_tree: r.root_tree,
            provenance: r
                .provenance
                .as_deref()
                .and_then(|p| serde_json::from_str::<Provenance>(p).ok()),
            merged_from,
            changes,
            changes_truncated,
        })
    }
}

fn change_view(row: &ChangeRow) -> Option<ChangeView> {
    let path = match DepotPath::parse(&row.path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "stored change has an invalid path");
            return None;
        }
    };
    let action = match row.action {
        'A' => ChangeAction::Add,
        'M' => ChangeAction::Modify,
        'D' => ChangeAction::Delete,
        _ => return None,
    };
    let kind = row.kind.and_then(|k| match k {
        1 => Some(EntryKind::File),
        2 => Some(EntryKind::Dir),
        3 => Some(EntryKind::Symlink),
        _ => None,
    });
    let copy_from = match (&row.copy_from_path, row.copy_from_rev) {
        (Some(p), Some(rev)) => DepotPath::parse(p).ok().map(|path| CopyFrom { path, rev }),
        _ => None,
    };
    Some(ChangeView {
        path,
        action,
        kind,
        old_id: row.old_id,
        new_id: row.new_id,
        size: row.size,
        executable: row.flags & lode_core::tree::FLAG_EXECUTABLE != 0,
        copy_from,
    })
}

fn cap_segments(mut segs: Vec<HistorySegment>, cap: u64) -> Vec<HistorySegment> {
    for s in &mut segs {
        s.max_rev = Some(s.max_rev.map_or(cap, |m| m.min(cap)));
    }
    segs
}

/// The scopes to search, newest first. Without `follow` there is one; with it, one per step
/// back through a copy or rename.
fn scopes(
    tx: &dyn Reads,
    depot: DepotId,
    segments: Option<Vec<HistorySegment>>,
    path: Option<DepotPath>,
    follow: bool,
) -> Result<Vec<Scope>, Fail> {
    let mut cur = Scope {
        segments,
        path,
        min: None,
        max: None,
    };
    if !follow || cur.path.is_none() {
        return Ok(vec![cur]);
    }
    let mut out = Vec::new();
    for _ in 0..MAX_FOLLOW_STEPS {
        let Some(path) = cur.path.clone() else { break };
        let found = tx.find_revisions(
            depot,
            &RevisionFilter {
                segments: cur.segments.clone(),
                path: Some(path.as_str().to_owned()),
                copies_only: true,
                max: cur.max,
                limit: 1,
                ..RevisionFilter::default()
            },
        )?;
        let source = match found.first() {
            Some(r) => tx
                .changes(depot, r.num)?
                .into_iter()
                .find(|c| c.path == path.as_str())
                .and_then(|c| Some((r.num, c.copy_from_path?, c.copy_from_rev?))),
            None => None,
        };
        let Some((at, src_path, src_rev)) = source else {
            out.push(cur);
            return Ok(out);
        };
        let Ok(src_path) = DepotPath::parse(&src_path) else {
            out.push(cur);
            return Ok(out);
        };
        if src_rev >= at {
            // A copy source is always older; anything else is a damaged row.
            out.push(cur);
            return Ok(out);
        }
        let next_segments = match &cur.segments {
            None => None,
            Some(_) => {
                let src = tx.revision(depot, src_rev)?;
                match src {
                    Some(s) => Some(cap_segments(tx.branch_chain(s.branch_id)?, src_rev)),
                    None => {
                        out.push(Scope {
                            min: Some(at),
                            ..cur
                        });
                        return Ok(out);
                    }
                }
            }
        };
        out.push(Scope {
            min: Some(at),
            ..cur
        });
        cur = Scope {
            segments: next_segments,
            path: Some(src_path),
            min: None,
            max: Some(src_rev),
        };
    }
    out.push(cur);
    Ok(out)
}

/// `GET /depots/{d}/revisions`.
pub async fn list_revisions(
    state: &AppState,
    caller: &Caller,
    depot: &DepotRow,
    q: RevisionListQuery,
) -> ApiResult<RevisionList> {
    if q.before.is_some() && q.after.is_some() {
        return Err(ApiError::invalid_request(
            "`before` and `after` cannot be combined",
        ));
    }
    if q.follow && q.path.is_none() {
        return Err(ApiError::invalid_request("`follow` needs `path`"));
    }
    let limit = q
        .limit
        .unwrap_or(DEFAULT_REVISION_PAGE)
        .clamp(1, MAX_REVISION_PAGE) as usize;
    let cap = state.config().limits.max_changes_listed;
    let d = depot.id;
    let access = state.permissions().depot(caller, d);

    let result = state
        .meta()
        .read(move |tx| {
            finish((|| {
                let mut lookup = Lookup::new(d, &access);
                let segments = match &q.branch {
                    None => None,
                    Some(name) => {
                        let b = tx
                            .branch(d, name)?
                            .filter(|b| access.branch(&b.name).reads_anything())
                            .ok_or_else(|| {
                                Fail::Api(ApiError::not_found(format!("no branch `{name}`")))
                            })?;
                        Some(if q.own {
                            vec![HistorySegment {
                                branch_id: b.id,
                                max_rev: None,
                            }]
                        } else {
                            tx.branch_chain(b.id)?
                        })
                    }
                };
                let author = match &q.author {
                    None => None,
                    Some(name) => match tx.principal_by_name(name)? {
                        Some(p) => Some(p.id),
                        // Nobody by that name wrote anything.
                        None => {
                            return Ok(RevisionList {
                                revisions: Vec::new(),
                                more: false,
                            })
                        }
                    },
                };
                let author_kind = q.author_kind.map(|k| match k {
                    lode_proto::PrincipalKind::User => PrincipalKind::User,
                    lode_proto::PrincipalKind::Agent => PrincipalKind::Agent,
                });
                let mut scopes = scopes(tx, d, segments, q.path.clone(), q.follow)?;
                let ascending = q.after.is_some();
                if ascending {
                    scopes.reverse();
                }
                let page_min = q.after.map(|a| a.saturating_add(1));
                let page_max = match q.before {
                    Some(0) => {
                        return Ok(RevisionList {
                            revisions: Vec::new(),
                            more: false,
                        })
                    }
                    Some(b) => Some(b - 1),
                    None => None,
                };

                // Collect up to limit + 1 visible revisions to know whether more follow.
                let mut picked: Vec<(RevisionRow, String, BranchAccess)> = Vec::new();
                'scopes: for scope in &scopes {
                    let mut min = max_opt(scope.min, page_min);
                    let mut max = min_opt(scope.max, page_max);
                    loop {
                        if let (Some(lo), Some(hi)) = (min, max) {
                            if lo > hi {
                                break;
                            }
                        }
                        let want = (limit + 1 - picked.len()).max(50) as u32;
                        let rows = tx.find_revisions(
                            d,
                            &RevisionFilter {
                                segments: scope.segments.clone(),
                                path: scope.path.as_ref().map(|p| p.as_str().to_owned()),
                                copies_only: false,
                                min,
                                max,
                                ascending,
                                limit: want,
                                author,
                                author_kind,
                            },
                        )?;
                        let exhausted = rows.len() < want as usize;
                        for r in rows {
                            if ascending {
                                min = Some(r.num + 1);
                            } else if r.num == 0 {
                                break;
                            } else {
                                max = Some(r.num - 1);
                            }
                            let Some((name, baccess)) = lookup.branch(tx, r.branch_id)? else {
                                continue;
                            };
                            if let Some(p) = &scope.path {
                                if !baccess.allows(p, Level::Read) {
                                    continue;
                                }
                            }
                            picked.push((r, name, baccess));
                            if picked.len() > limit {
                                break 'scopes;
                            }
                        }
                        if exhausted {
                            break;
                        }
                    }
                }
                let more = picked.len() > limit;
                picked.truncate(limit);
                let mut revisions = Vec::with_capacity(picked.len());
                for (r, name, baccess) in &picked {
                    let changes = q.changes.then_some(cap);
                    revisions.push(lookup.view(tx, r, name.clone(), baccess, changes)?);
                }
                Ok(RevisionList { revisions, more })
            })())
        })
        .await?;
    result
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `GET /depots/{d}/revisions/{n}`: one revision with every readable change.
pub async fn revision_detail(
    state: &AppState,
    caller: &Caller,
    depot: &DepotRow,
    n: u64,
) -> ApiResult<RevisionView> {
    let d = depot.id;
    let access = state.permissions().depot(caller, d);
    let result = state
        .meta()
        .read(move |tx| {
            finish((|| {
                let not_found = || {
                    Fail::Api(ApiError::new(
                        ErrorCode::NotFound,
                        format!("no revision {n}"),
                    ))
                };
                let r = tx.revision(d, n)?.ok_or_else(not_found)?;
                let mut lookup = Lookup::new(d, &access);
                let (name, baccess) = lookup.branch(tx, r.branch_id)?.ok_or_else(not_found)?;
                Ok(lookup.view(tx, &r, name, &baccess, Some(usize::MAX))?)
            })())
        })
        .await?;
    result
}
