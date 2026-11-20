//! The commit transaction (ARCHITECTURE §5.2).
//!
//! Commits to one branch are serialized by an in-process lock, so the head cannot move
//! between the checks and the write; the transaction still re-checks the head and the
//! request id before writing.

use std::collections::HashMap;
use std::sync::Arc;

use lode_core::tree::FLAG_EXECUTABLE;
use lode_core::{
    validate_changes, Change, ChangeAction, ChangeSetError, DepotPath, EntryKind, ErrorCode,
    LodeConfig, ObjectId, ObjectKind, TreeEntry,
};
use lode_meta::{
    BranchRow, BranchState, ChangeRow, DepotRow, MergeRow, MetaError, PrincipalRow, Reads,
    RevisionRow,
};
use lode_proto::commit::{
    CommitRequest, CommitResponse, MergedFrom, Provenance, Via, EXT_FORCE_FLOW,
};
use lode_proto::consts::MAX_PROVENANCE_BYTES;
use lode_store::{PutOptions, StoreError};
use serde_json::json;
use tokio::io::AsyncReadExt;

use super::editor::{EditError, EditOp, Expect, TreeEditor};
use super::trees::{lookup, TreeLoadError};
use super::{CommitCtx, CommittedCtx, Origin};
use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::perm::Level;
use crate::state::AppState;

/// Path of the versioned configuration file.
pub const LODECONFIG: &str = ".lodeconfig";
const MAX_LODECONFIG_BYTES: u64 = 1024 * 1024;
const MAX_REQUEST_ID: usize = 128;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CLIENT_NAME: usize = 256;

/// Server-side facts about a commit that the request cannot set.
#[derive(Debug, Clone)]
pub struct CommitOptions {
    /// The author when it is not the caller (a shelf submit commits as the shelf owner).
    pub author: Option<PrincipalRow>,
    /// How the commit arrived.
    pub origin: Origin,
    /// Endpoint family, recorded in provenance.
    pub via: Via,
    /// Client name from `User-Agent`, self-reported.
    pub client: Option<String>,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            author: None,
            origin: Origin::Direct,
            via: Via::Api,
            client: None,
        }
    }
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::invalid_request(msg)
}

fn change_set_error(e: ChangeSetError) -> ApiError {
    match &e {
        ChangeSetError::CaseCollision(a, b) => ApiError::new(ErrorCode::InvalidPath, e.to_string())
            .with_details(json!({ "paths": [a, b] })),
        ChangeSetError::FileDirConflict { file, nested } => invalid(e.to_string())
            .with_details(json!({ "paths": [file.as_str(), nested.as_str()] })),
        ChangeSetError::Duplicate(p) | ChangeSetError::Invalid { path: p, .. } => {
            invalid(e.to_string()).with_details(json!({ "paths": [p.as_str()] }))
        }
    }
}

fn load_error(e: TreeLoadError) -> ApiError {
    tracing::error!(error = %e, "cannot load a tree");
    ApiError::internal()
}

fn kind_code(k: EntryKind) -> u8 {
    match k {
        EntryKind::File => 1,
        EntryKind::Dir => 2,
        EntryKind::Symlink => 3,
    }
}

/// Reads a small blob fully.
async fn read_small_blob(
    state: &AppState,
    depot: lode_core::DepotId,
    id: &ObjectId,
    cap: u64,
) -> ApiResult<Vec<u8>> {
    let obj = state.blobs().get(depot, id).await?;
    if obj.kind != ObjectKind::Blob || obj.raw_len > cap {
        return Err(invalid(format!("{LODECONFIG} is too large")));
    }
    let mut buf = Vec::new();
    obj.reader
        .take(cap)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ApiError::from(StoreError::Io(e)))?;
    Ok(buf)
}

/// The parsed `.lodeconfig` at the head of a branch, cached by blob id.
async fn branch_config(
    state: &AppState,
    depot: lode_core::DepotId,
    root: &ObjectId,
) -> ApiResult<Arc<LodeConfig>> {
    let path = DepotPath::parse(LODECONFIG).map_err(|_| ApiError::internal())?;
    let entry = lookup(state.trees(), depot, root, &path)
        .await
        .map_err(load_error)?;
    let Some(entry) = entry.filter(|e| e.kind == EntryKind::File) else {
        return Ok(Arc::new(LodeConfig::default()));
    };
    if let Some(c) = state.config_cache().get(&entry.id) {
        return Ok(c);
    }
    let bytes = read_small_blob(state, depot, &entry.id, MAX_LODECONFIG_BYTES).await?;
    let config = match LodeConfig::parse(&bytes) {
        Ok(c) => c,
        Err(e) => {
            // Commits refuse unparseable files, so this needs a head written another way.
            tracing::error!(error = %e, "branch head has an invalid .lodeconfig; using defaults");
            LodeConfig::default()
        }
    };
    let config = Arc::new(config);
    state.config_cache().insert(entry.id, config.clone());
    Ok(config)
}

fn provenance_json(req: Option<Provenance>, opts: &CommitOptions) -> ApiResult<String> {
    let mut p = req.unwrap_or_default();
    p.via = Some(opts.via);
    // Only a module (the shelves module's submit) may say where a revision came from.
    if opts.origin == Origin::Direct {
        p.submitted_shelf = None;
    }
    p.client = opts.client.as_ref().map(|c| {
        let mut c = c.clone();
        if c.len() > MAX_CLIENT_NAME {
            let mut cut = MAX_CLIENT_NAME;
            while !c.is_char_boundary(cut) {
                cut -= 1;
            }
            c.truncate(cut);
        }
        c
    });
    let text = serde_json::to_string(&p).map_err(|_| ApiError::internal())?;
    if text.len() > MAX_PROVENANCE_BYTES {
        return Err(invalid(format!(
            "provenance exceeds {MAX_PROVENANCE_BYTES} bytes"
        )));
    }
    Ok(text)
}

enum TxOutcome {
    Done(u64),
    Replay(u64, String),
    Rejected(ApiError),
}

/// Runs a commit for `caller` on `branch` of `depot`. The caller must already have passed
/// the scope check and see the depot.
pub async fn commit(
    state: &AppState,
    caller: &Caller,
    depot: &DepotRow,
    branch: &str,
    req: CommitRequest,
    opts: CommitOptions,
) -> ApiResult<CommitResponse> {
    let d = depot.id;

    // --- Validation without any lock ---
    if req.request_id.is_empty() || req.request_id.len() > MAX_REQUEST_ID {
        return Err(invalid(format!(
            "request_id must have 1 to {MAX_REQUEST_ID} characters"
        )));
    }
    if req.message.len() > MAX_MESSAGE_BYTES {
        return Err(invalid("message is too long"));
    }
    check_shape(state, &req.changes, !req.merged_from.is_empty())?;

    if let Some(resp) = replay(state, d, &req.request_id).await? {
        return Ok(resp);
    }

    // From the blob checks until the revision is written, garbage collection must not delete
    // the objects this commit is about to reference (see `maintenance::gc`).
    let _fence = state.gc_fence().read().await;

    check_blobs(state, d, &req.changes).await?;
    check_lodeconfig(state, d, &req.changes).await?;
    let provenance = provenance_json(req.provenance.clone(), &opts)?;

    // --- Serialized per branch from here ---
    let lock = state.branch_lock(d, branch);
    let _guard = lock.lock().await;

    let name = branch.to_owned();
    let (branch_row, head) = state
        .meta()
        .read(move |tx| {
            let Some(b) = tx.branch(d, &name)? else {
                return Ok(None);
            };
            let head = tx.revision(d, b.head_rev)?;
            Ok(head.map(|h| (b, h)))
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no branch `{branch}`")))?;
    if branch_row.state == BranchState::Archived {
        return Err(ApiError::new(
            ErrorCode::BranchArchived,
            format!("branch `{branch}` is archived"),
        ));
    }
    if let Some(resp) = replay(state, d, &req.request_id).await? {
        return Ok(resp);
    }

    let config = branch_config(state, d, &head.root_tree).await?;

    // Compare every change with the head, building the new trees.
    let edited = TreeEditor::new(state.trees(), d)
        .apply(&head.root_tree, &edit_ops(&req.changes))
        .await
        .map_err(edit_error)?;

    // Write permission on every changed path.
    check_level(state, caller, d, branch, &req.changes, Level::Write)?;

    check_copy_from(state, caller, d, &req.changes).await?;

    let merges = merge_rows(state, d, &req.merged_from).await?;
    let forced_flow = check_stream_flow(state, caller, &branch_row, &req).await?;

    state
        .failpoints()
        .check(crate::failpoint::COMMIT_BEFORE_TREES)?;
    // Store the new trees. Not transactional; an aborted commit leaves garbage for GC.
    for (id, bytes) in &edited.new_trees {
        state
            .blobs()
            .put(
                d,
                ObjectKind::Tree,
                id,
                &mut &bytes[..],
                PutOptions {
                    expected_len: Some(bytes.len() as u64),
                },
            )
            .await?;
    }

    let author = opts
        .author
        .clone()
        .unwrap_or_else(|| caller.principal.clone());
    let submitter = caller.principal.clone();
    let now = state.clock().now_ms();
    let hooks = state.modules().to_vec();
    let request_id = req.request_id.clone();
    let message = req.message;
    let changes = req.changes;
    let extensions = req.extensions;
    let origin = opts.origin;
    let root = edited.root;
    let before = edited.before;
    let expected_head = head.num;
    let branch_id = branch_row.id;

    // A rejection from `in_commit` comes after rows were written, so the closure fails
    // (rolling back) and leaves the reason here.
    let late_reject: Arc<std::sync::Mutex<Option<ApiError>>> = Arc::default();
    let late_slot = late_reject.clone();
    let failpoints = state.failpoints().clone();
    let outcome = state
        .meta()
        .write(move |tx| {
            if let Some(r) = tx.revision_by_request_id(d, &request_id)? {
                let b = tx
                    .branch_by_id(r.branch_id)?
                    .map(|b| b.name)
                    .unwrap_or_default();
                return Ok(TxOutcome::Replay(r.num, b));
            }
            let Some(b) = tx.branch_by_id(branch_id)? else {
                return Ok(TxOutcome::Rejected(ApiError::not_found(
                    "branch was removed",
                )));
            };
            if b.state == BranchState::Archived {
                return Ok(TxOutcome::Rejected(ApiError::new(
                    ErrorCode::BranchArchived,
                    format!("branch `{}` is archived", b.name),
                )));
            }
            if b.head_rev != expected_head {
                // Commits are serialized per branch, so only another process could do this.
                return Ok(TxOutcome::Rejected(ApiError::new(
                    ErrorCode::OutOfDate,
                    "the branch moved during the commit; retry",
                )));
            }
            let ctx = CommitCtx {
                depot: d,
                branch: &b,
                author: &author,
                submitter: &submitter,
                message: &message,
                changes: &changes,
                extensions: &extensions,
                branch_config: &config,
                origin: &origin,
                head_entries: &before,
            };
            for h in &hooks {
                if let Err(e) = h.pre_commit(tx as &dyn Reads, &ctx) {
                    return Ok(TxOutcome::Rejected(e));
                }
            }
            let num = tx.allocate_rev(d)?;
            tx.insert_revision(&RevisionRow {
                depot_id: d,
                num,
                branch_id,
                parent_num: Some(expected_head),
                root_tree: root,
                author_id: author.id,
                submitter_id: submitter.id,
                created_at: now,
                message: message.clone(),
                provenance: Some(provenance.clone()),
                request_id: Some(request_id.clone()),
            })?;
            let rows: Vec<ChangeRow> = changes
                .iter()
                .map(|c| change_row(c, before.get(&c.path)))
                .collect();
            tx.insert_changes(d, num, &rows)?;
            for m in &merges {
                tx.insert_merge(d, num, m)?;
            }
            tx.set_branch_head(branch_id, num)?;
            let committed = CommittedCtx {
                commit: &ctx,
                rev: num,
                created_at: now,
            };
            for h in &hooks {
                if let Err(e) = h.in_commit(tx, &committed) {
                    if let Ok(mut slot) = late_slot.lock() {
                        *slot = Some(e);
                    }
                    return Err(MetaError::NotFound(format!("rejected by {}", h.name())));
                }
            }
            if failpoints.hit(crate::failpoint::COMMIT_IN_TRANSACTION) {
                if let Ok(mut slot) = late_slot.lock() {
                    *slot = Some(ApiError::internal());
                }
                return Err(MetaError::NotFound("failpoint".into()));
            }
            let data = crate::branches::revision_created(
                num,
                &b.name,
                &author,
                &submitter,
                origin.label(),
                forced_flow,
            );
            tx.append_outbox(Some(d), "revision.created", &data, now)?;
            Ok(TxOutcome::Done(num))
        })
        .await;

    if let Some(e) = late_reject.lock().ok().and_then(|mut s| s.take()) {
        return Err(e);
    }
    let outcome = match outcome {
        Ok(o) => o,
        // The same request id committed concurrently on another branch.
        Err(MetaError::AlreadyExists(_)) => {
            if let Some(resp) = replay(state, d, &req.request_id).await? {
                return Ok(resp);
            }
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "concurrent commit; retry",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    match outcome {
        TxOutcome::Done(rev) => {
            state.outbox_notify().notify_one();
            state
                .failpoints()
                .check(crate::failpoint::COMMIT_AFTER_TRANSACTION)?;
            Ok(CommitResponse {
                rev,
                branch: branch.to_owned(),
                replayed: false,
            })
        }
        TxOutcome::Replay(rev, branch) => Ok(CommitResponse {
            rev,
            branch,
            replayed: true,
        }),
        TxOutcome::Rejected(e) => Err(e),
    }
}

/// Count, emptiness and per-path rules of a change set.
fn check_shape(state: &AppState, changes: &[Change], has_merges: bool) -> ApiResult<()> {
    if changes.len() > state.config().limits.max_changes_per_commit {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "at most {} changes per commit",
                state.config().limits.max_changes_per_commit
            ),
        ));
    }
    if changes.is_empty() && !has_merges {
        return Err(invalid("a commit needs at least one change"));
    }
    validate_changes(changes).map_err(change_set_error)
}

/// Every new blob must exist, be a blob, and have the size the change claims.
async fn check_blobs(state: &AppState, d: lode_core::DepotId, changes: &[Change]) -> ApiResult<()> {
    let mut sizes: HashMap<ObjectId, u64> = HashMap::new();
    for c in changes {
        if let (Some(id), Some(size)) = (c.new_id, c.size) {
            if sizes.insert(id, size).is_some_and(|prev| prev != size) {
                return Err(invalid(format!("blob {id} is given two different sizes")));
            }
        }
    }
    let obliterated = state.obliterated(d).await?;
    let mut gone: Vec<&str> = changes
        .iter()
        .filter(|c| c.new_id.is_some_and(|id| obliterated.contains(&id)))
        .map(|c| c.path.as_str())
        .collect();
    if !gone.is_empty() {
        gone.sort_unstable();
        gone.dedup();
        return Err(ApiError::new(
            ErrorCode::Obliterated,
            format!(
                "{} holds content that was obliterated from this depot and cannot be committed again",
                gone.first().copied().unwrap_or_default()
            ),
        )
        .with_details(json!({ "paths": gone })));
    }
    let mut missing = Vec::new();
    for (id, size) in &sizes {
        match state.blobs().get_stored(d, id).await {
            Ok(s) if s.kind == ObjectKind::Blob => {
                if s.raw_len != *size {
                    return Err(invalid(format!(
                        "blob {id} has {} bytes, not {size}",
                        s.raw_len
                    )));
                }
            }
            Ok(s) if s.kind == ObjectKind::ChunkList => {
                drop(s);
                let total = match state.chunked().chunk_list(d, id).await? {
                    Some(list) => list.total_size(),
                    None => 0,
                };
                if total != *size {
                    return Err(invalid(format!("blob {id} has {total} bytes, not {size}")));
                }
            }
            Ok(_) | Err(StoreError::NotFound(_)) => missing.push(*id),
            Err(e) => return Err(e.into()),
        }
    }
    if !missing.is_empty() {
        missing.sort();
        return Err(ApiError::new(
            ErrorCode::MissingObjects,
            format!("{} blob(s) must be uploaded first", missing.len()),
        )
        .with_details(json!({ "ids": missing })));
    }
    Ok(())
}

/// A branch head never holds an unparseable configuration.
async fn check_lodeconfig(
    state: &AppState,
    d: lode_core::DepotId,
    changes: &[Change],
) -> ApiResult<()> {
    for c in changes {
        if c.path.as_str() == LODECONFIG && c.action != ChangeAction::Delete {
            let id = c.new_id.ok_or_else(ApiError::internal)?;
            if c.kind != Some(EntryKind::File) {
                return Err(invalid(format!("{LODECONFIG} must be a regular file")));
            }
            let bytes = read_small_blob(state, d, &id, MAX_LODECONFIG_BYTES).await?;
            LodeConfig::parse(&bytes).map_err(|e| {
                invalid(format!("invalid {LODECONFIG}: {e}"))
                    .with_details(json!({ "paths": [LODECONFIG] }))
            })?;
        }
    }
    Ok(())
}

/// The tree edits of a change set.
fn edit_ops(changes: &[Change]) -> Vec<EditOp> {
    changes
        .iter()
        .map(|c| EditOp {
            path: c.path.clone(),
            expect: match c.action {
                ChangeAction::Add => Expect::Absent,
                ChangeAction::Modify | ChangeAction::Delete => {
                    Expect::Id(c.base_id.unwrap_or(ObjectId::from_bytes([0; 32])))
                }
            },
            entry: match c.action {
                ChangeAction::Delete => None,
                _ => Some(TreeEntry {
                    name: c.path.name().to_owned(),
                    kind: c.kind.unwrap_or(EntryKind::File),
                    flags: if c.executable { FLAG_EXECUTABLE } else { 0 },
                    id: c.new_id.unwrap_or(ObjectId::from_bytes([0; 32])),
                    size: c.size.unwrap_or(0),
                }),
            },
        })
        .collect()
}

fn edit_error(e: EditError) -> ApiError {
    match e {
        EditError::OutOfDate(paths) => ApiError::new(
            ErrorCode::OutOfDate,
            format!(
                "{} path(s) changed on the branch; update first",
                paths.len()
            ),
        )
        .with_details(json!({ "paths": paths })),
        EditError::CaseCollision(a, b) => ApiError::new(
            ErrorCode::InvalidPath,
            format!("{a} and {b} differ only in case"),
        )
        .with_details(json!({ "paths": [a, b] })),
        EditError::Invalid(m) => invalid(m),
        EditError::Load(e) => load_error(e),
    }
}

/// `need` on every changed path of `branch`.
fn check_level(
    state: &AppState,
    caller: &Caller,
    d: lode_core::DepotId,
    branch: &str,
    changes: &[Change],
    need: Level,
) -> ApiResult<()> {
    let access = state.permissions().depot(caller, d).branch(branch);
    let levels = access.levels(changes.iter().map(|c| &c.path));
    let denied: Vec<&str> = changes
        .iter()
        .zip(&levels)
        .filter(|(_, l)| **l < need)
        .map(|(c, _)| c.path.as_str())
        .collect();
    if !denied.is_empty() {
        let what = if need >= Level::Write {
            "write"
        } else {
            "read"
        };
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!("no {what} access to {} path(s)", denied.len()),
        )
        .with_details(json!({ "paths": denied })));
    }
    Ok(())
}

/// copy_from: the source must exist and be readable.
async fn check_copy_from(
    state: &AppState,
    caller: &Caller,
    d: lode_core::DepotId,
    changes: &[Change],
) -> ApiResult<()> {
    for c in changes {
        let Some(cf) = &c.copy_from else { continue };
        let rev = cf.rev;
        let src = state
            .meta()
            .read(move |tx| {
                let Some(r) = tx.revision(d, rev)? else {
                    return Ok(None);
                };
                Ok(tx.branch_by_id(r.branch_id)?.map(|b| (r, b)))
            })
            .await?;
        let bad = || {
            invalid(format!(
                "copy source {}@{} does not exist",
                cf.path.as_str(),
                cf.rev
            ))
            .with_details(json!({ "paths": [c.path.as_str()] }))
        };
        let Some((src_rev, src_branch)) = src else {
            return Err(bad());
        };
        let entry = lookup(state.trees(), d, &src_rev.root_tree, &cf.path)
            .await
            .map_err(load_error)?;
        if !entry.is_some_and(|e| e.kind != EntryKind::Dir) {
            return Err(bad());
        }
        let level = state
            .permissions()
            .depot(caller, d)
            .level(&src_branch.name, &cf.path);
        if level < Level::Read {
            return Err(bad());
        }
    }
    Ok(())
}

/// SPEC §5.3a: the stream rules for every `merged_from` range. Returns whether a depot admin
/// skipped them with [`EXT_FORCE_FLOW`] (only then, and only when they would have refused).
async fn check_stream_flow(
    state: &AppState,
    caller: &Caller,
    target: &BranchRow,
    req: &CommitRequest,
) -> ApiResult<bool> {
    if req.merged_from.is_empty() {
        return Ok(false);
    }
    let force = req
        .extensions
        .get(EXT_FORCE_FLOW)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let t = target.clone();
    let merged = req.merged_from.clone();
    let verdict = state
        .meta()
        .read(move |tx| crate::branches::check_flow(tx, &t, &merged))
        .await?;
    let Err(refused) = verdict else {
        return Ok(false);
    };
    if !force {
        return Err(refused);
    }
    let admin = caller.has_scope(crate::auth::SCOPE_ADMIN)
        && state
            .permissions()
            .depot(caller, target.depot_id)
            .is_depot_admin();
    if !admin {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "skipping the stream rules needs admin on the depot",
        ));
    }
    Ok(true)
}

/// merged_from: the source branch exists and the range lies in its history.
async fn merge_rows(
    state: &AppState,
    d: lode_core::DepotId,
    merged_from: &[MergedFrom],
) -> ApiResult<Vec<MergeRow>> {
    let mut merges = Vec::new();
    for m in merged_from {
        let name = m.branch.clone();
        let src = state
            .meta()
            .read(move |tx| tx.branch(d, &name))
            .await?
            .ok_or_else(|| invalid(format!("merged_from: no branch `{}`", m.branch)))?;
        if m.from_rev == 0 || m.from_rev > m.to_rev || m.to_rev > src.head_rev {
            return Err(invalid(format!(
                "merged_from: {}..{} is not in the history of `{}`",
                m.from_rev, m.to_rev, m.branch
            )));
        }
        merges.push(MergeRow {
            source_branch_id: src.id,
            from_rev: m.from_rev,
            to_rev: m.to_rev,
        });
    }
    Ok(merges)
}

/// A change set as a shelf stores it, checked the way a commit would check it but against
/// `base_rev` of `branch` instead of the head: the change-set rules, blob existence and
/// sizes, a parseable `.lodeconfig`, `base_id` against the file at `base_rev`, `copy_from`,
/// `merged_from` and the provenance size. The caller needs `need` on every changed path.
/// Returns the branch row. Hold the GC fence (shared) across this check and the write that
/// stores the change set.
#[allow(clippy::too_many_arguments)]
pub async fn check_change_set(
    state: &AppState,
    caller: &Caller,
    depot: &DepotRow,
    branch: &str,
    base_rev: u64,
    changes: &[Change],
    merged_from: &[MergedFrom],
    provenance: Option<&Provenance>,
    need: Level,
) -> ApiResult<BranchRow> {
    let d = depot.id;
    check_shape(state, changes, !merged_from.is_empty())?;
    let access = state.permissions().depot(caller, d).branch(branch);
    let not_found = || ApiError::not_found(format!("no branch `{branch}`"));
    if !access.reads_anything() {
        return Err(not_found());
    }
    let name = branch.to_owned();
    let found = state
        .meta()
        .read(move |tx| {
            let Some(b) = tx.branch(d, &name)? else {
                return Ok(None);
            };
            // The base must lie in the branch's history (§4.3).
            let chain = tx.branch_chain(b.id)?;
            let base = tx.revision(d, base_rev)?.filter(|r| {
                r.num <= b.head_rev
                    && chain.iter().any(|seg| {
                        seg.branch_id == r.branch_id && seg.max_rev.is_none_or(|m| r.num <= m)
                    })
            });
            Ok(Some((b, base)))
        })
        .await?;
    let (b, base) = found.ok_or_else(not_found)?;
    let base = base.ok_or_else(|| {
        invalid(format!(
            "revision {base_rev} is not in the history of `{branch}`"
        ))
    })?;
    if b.state == BranchState::Archived {
        return Err(ApiError::new(
            ErrorCode::BranchArchived,
            format!("branch `{branch}` is archived"),
        ));
    }
    check_level(state, caller, d, branch, changes, need)?;
    check_blobs(state, d, changes).await?;
    check_lodeconfig(state, d, changes).await?;
    TreeEditor::new(state.trees(), d)
        .apply(&base.root_tree, &edit_ops(changes))
        .await
        .map_err(|e| match e {
            EditError::OutOfDate(paths) => ApiError::new(
                ErrorCode::OutOfDate,
                format!("{} path(s) do not match revision {base_rev}", paths.len()),
            )
            .with_details(json!({ "paths": paths })),
            other => edit_error(other),
        })?;
    check_copy_from(state, caller, d, changes).await?;
    merge_rows(state, d, merged_from).await?;
    if let Some(p) = provenance {
        let text = serde_json::to_string(p).map_err(|_| ApiError::internal())?;
        if text.len() > MAX_PROVENANCE_BYTES {
            return Err(invalid(format!(
                "provenance exceeds {MAX_PROVENANCE_BYTES} bytes"
            )));
        }
    }
    Ok(b)
}

fn change_row(c: &Change, before: Option<&TreeEntry>) -> ChangeRow {
    let deleted = c.action == ChangeAction::Delete;
    ChangeRow {
        path: c.path.as_str().to_owned(),
        action: c.action.letter(),
        kind: if deleted { None } else { c.kind.map(kind_code) },
        old_id: before.map(|e| e.id),
        new_id: c.new_id,
        size: c.size,
        flags: if c.executable { FLAG_EXECUTABLE } else { 0 },
        copy_from_path: c.copy_from.as_ref().map(|cf| cf.path.as_str().to_owned()),
        copy_from_rev: c.copy_from.as_ref().map(|cf| cf.rev),
    }
}

async fn replay(
    state: &AppState,
    d: lode_core::DepotId,
    request_id: &str,
) -> ApiResult<Option<CommitResponse>> {
    let rid = request_id.to_owned();
    let found = state
        .meta()
        .read(move |tx| {
            let Some(r) = tx.revision_by_request_id(d, &rid)? else {
                return Ok(None);
            };
            let b: Option<BranchRow> = tx.branch_by_id(r.branch_id)?;
            Ok(Some((r.num, b.map(|b| b.name).unwrap_or_default())))
        })
        .await?;
    Ok(found.map(|(rev, branch)| CommitResponse {
        rev,
        branch,
        replayed: true,
    }))
}
