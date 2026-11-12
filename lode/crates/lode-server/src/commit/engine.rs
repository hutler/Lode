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
use lode_proto::commit::{CommitRequest, CommitResponse, Provenance, Via};
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
    if req.changes.len() > state.config().limits.max_changes_per_commit {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "at most {} changes per commit",
                state.config().limits.max_changes_per_commit
            ),
        ));
    }
    if req.changes.is_empty() && req.merged_from.is_empty() {
        return Err(invalid("a commit needs at least one change"));
    }
    validate_changes(&req.changes).map_err(change_set_error)?;

    if let Some(resp) = replay(state, d, &req.request_id).await? {
        return Ok(resp);
    }

    // Every new blob must exist, be a blob, and have the size the change claims.
    let mut sizes: HashMap<ObjectId, u64> = HashMap::new();
    for c in &req.changes {
        if let (Some(id), Some(size)) = (c.new_id, c.size) {
            if sizes.insert(id, size).is_some_and(|prev| prev != size) {
                return Err(invalid(format!("blob {id} is given two different sizes")));
            }
        }
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

    // A branch head never holds an unparseable configuration.
    for c in &req.changes {
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
    let ops: Vec<EditOp> = req
        .changes
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
        .collect();
    let edited = TreeEditor::new(state.trees(), d)
        .apply(&head.root_tree, &ops)
        .await
        .map_err(|e| match e {
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
        })?;

    // Write permission on every changed path.
    let access = state.permissions().depot(caller, d).branch(branch);
    let levels = access.levels(req.changes.iter().map(|c| &c.path));
    let denied: Vec<&str> = req
        .changes
        .iter()
        .zip(&levels)
        .filter(|(_, l)| **l < Level::Write)
        .map(|(c, _)| c.path.as_str())
        .collect();
    if !denied.is_empty() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!("no write access to {} path(s)", denied.len()),
        )
        .with_details(json!({ "paths": denied })));
    }

    // copy_from: the source must exist and be readable.
    for c in &req.changes {
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

    // merged_from: the source branch exists and the range lies in its history.
    let mut merges = Vec::new();
    for m in &req.merged_from {
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
            };
            for h in &hooks {
                if let Err(e) = h.in_commit(tx, &committed) {
                    if let Ok(mut slot) = late_slot.lock() {
                        *slot = Some(e);
                    }
                    return Err(MetaError::NotFound(format!("rejected by {}", h.name())));
                }
            }
            let data = crate::branches::revision_created(
                num,
                &b.name,
                &author,
                &submitter,
                origin.label(),
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
