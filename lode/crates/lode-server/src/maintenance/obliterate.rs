//! Obliterate (T120): permanently remove content from a depot.
//!
//! Content is removed, history is not. Revisions, paths and trees stay exactly as they were,
//! so every tree id and revision remains valid; the blob objects are deleted and the depot
//! records each obliterated id. From then on the id is refused for upload and answers
//! `obliterated` wherever it would be read.
//!
//! Content still in an active branch's head is refused: removing it would break every
//! checkout of that branch. The check, the record and the deletion run under the exclusive
//! GC fence, so no commit can make the content current in between (a commit holds the fence
//! shared from its object checks to its transaction).

use std::collections::{BTreeMap, HashMap, HashSet};

use lode_core::{DepotId, DepotPath, EntryKind, ErrorCode, ObjectId};
use lode_meta::{BranchState, ObliteratedRow, PrincipalId, Reads};
use lode_proto::admin::{
    ContentInUse, ContentUse, ObliterateReport, ObliterateRequest, ObliterateTarget,
    ObliteratedObject, Obliteration,
};
use lode_store::StoreError;

use crate::commit::{lookup, TreeLoadError, TreeSource};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// At most this many targets per request.
pub const MAX_TARGETS: usize = 1_000;
/// Longest reason kept.
pub const MAX_REASON: usize = 1_000;

/// The answer for content that was obliterated.
pub fn gone(id: &ObjectId) -> ApiError {
    ApiError::new(
        ErrorCode::Obliterated,
        format!("content {id} was obliterated from this depot"),
    )
}

/// The error for an object a revision references but the store does not have: `obliterated`
/// when it was, otherwise an internal fault.
pub async fn missing_object(state: &AppState, depot: DepotId, id: &ObjectId) -> ApiError {
    match state.is_obliterated(depot, id).await {
        Ok(true) => gone(id),
        Ok(false) => {
            tracing::error!(%id, "a committed object is missing from the store");
            ApiError::internal()
        }
        Err(e) => e,
    }
}

fn load_error(e: TreeLoadError) -> ApiError {
    tracing::error!(error = %e, "obliterate: cannot load a tree");
    ApiError::internal()
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Every file and symlink under the tree `root`, with its path.
async fn files_under(
    state: &AppState,
    depot: DepotId,
    base: &str,
    root: ObjectId,
    out: &mut Vec<(String, ObjectId, u64)>,
) -> ApiResult<()> {
    let mut stack = vec![(base.to_owned(), root)];
    while let Some((dir, id)) = stack.pop() {
        let tree = state.trees().tree(depot, &id).await.map_err(load_error)?;
        for e in tree.entries() {
            let path = join(&dir, &e.name);
            if e.kind == EntryKind::Dir {
                stack.push((path, e.id));
            } else {
                out.push((path, e.id, e.size));
            }
        }
    }
    Ok(())
}

/// The selected content: id → (size, the path it was selected by).
type Selection = BTreeMap<ObjectId, (u64, String)>;

async fn select(
    state: &AppState,
    depot: DepotId,
    targets: &[ObliterateTarget],
) -> ApiResult<Selection> {
    let mut sel = Selection::new();
    for t in targets {
        let path = t.path.as_str().to_owned();
        match t.rev {
            None => {
                let p = path.clone();
                let rows = state
                    .meta()
                    .read(move |tx| {
                        let mut rows = tx.changes_at_path(depot, &p, false)?;
                        rows.extend(tx.changes_at_path(depot, &p, true)?);
                        Ok(rows)
                    })
                    .await?;
                if rows.is_empty() {
                    return Err(ApiError::not_found(format!("{path} has no history")));
                }
                for r in rows {
                    if r.change.action == 'D' {
                        continue;
                    }
                    if let Some(id) = r.change.new_id {
                        sel.entry(id)
                            .or_insert((r.change.size.unwrap_or(0), r.change.path.clone()));
                    }
                }
            }
            Some(rev) => {
                let row = state
                    .meta()
                    .read(move |tx| tx.revision(depot, rev))
                    .await?
                    .ok_or_else(|| ApiError::not_found(format!("no revision r{rev}")))?;
                let entry = lookup(state.trees(), depot, &row.root_tree, &t.path)
                    .await
                    .map_err(load_error)?
                    .ok_or_else(|| ApiError::not_found(format!("no {path} at r{rev}")))?;
                let mut files = Vec::new();
                if entry.kind == EntryKind::Dir {
                    files_under(state, depot, &path, entry.id, &mut files).await?;
                } else {
                    files.push((path.clone(), entry.id, entry.size));
                }
                for (p, id, size) in files {
                    sel.entry(id).or_insert((size, p));
                }
            }
        }
    }
    // The empty file is shared by every empty file in the depot; it holds nothing to hide.
    let empty = ObjectId::from_bytes(*blake3::hash(&[]).as_bytes());
    sel.remove(&empty);
    let gone = state.obliterated(depot).await?;
    sel.retain(|id, _| !gone.contains(id));
    Ok(sel)
}

/// Finds selected content in the heads of active branches. Trees found free of it are
/// remembered in `clean`, so a second pass only reads what changed.
async fn in_active_heads(
    state: &AppState,
    depot: DepotId,
    ids: &HashSet<ObjectId>,
    clean: &mut HashSet<ObjectId>,
) -> ApiResult<Vec<ContentInUse>> {
    let branches = state.meta().read(move |tx| tx.branches(depot)).await?;
    let mut out = Vec::new();
    for b in branches
        .into_iter()
        .filter(|b| b.state == BranchState::Active)
    {
        let head = b.head_rev;
        let Some(rev) = state
            .meta()
            .read(move |tx| tx.revision(depot, head))
            .await?
        else {
            continue;
        };
        // Depth first; a tree is marked clean once everything below it was.
        let mut stack = vec![(String::new(), rev.root_tree, false)];
        let mut dirty: HashSet<ObjectId> = HashSet::new();
        let mut found_here = HashSet::new();
        while let Some((dir, id, done)) = stack.pop() {
            if done {
                if !dirty.contains(&id) {
                    clean.insert(id);
                }
                continue;
            }
            if clean.contains(&id) {
                continue;
            }
            stack.push((dir.clone(), id, true));
            let tree = state.trees().tree(depot, &id).await.map_err(load_error)?;
            for e in tree.entries() {
                let path = join(&dir, &e.name);
                if e.kind == EntryKind::Dir {
                    stack.push((path, e.id, false));
                } else if ids.contains(&e.id) {
                    // Every ancestor of this entry is still open on the stack.
                    for (_, anc, done) in stack.iter() {
                        if *done {
                            dirty.insert(*anc);
                        }
                    }
                    if found_here.insert((e.id, path.clone())) {
                        let path = DepotPath::parse(&path).map_err(|_| ApiError::internal())?;
                        out.push(ContentInUse {
                            branch: b.name.clone(),
                            path,
                            id: e.id,
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Runs an obliterate of `depot` for `by`.
pub async fn obliterate(
    state: &AppState,
    depot: DepotId,
    by: PrincipalId,
    req: ObliterateRequest,
) -> ApiResult<ObliterateReport> {
    if req.targets.is_empty() {
        return Err(ApiError::invalid_request("name at least one path"));
    }
    if req.targets.len() > MAX_TARGETS {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!("at most {MAX_TARGETS} paths per call"),
        ));
    }
    let reason = req.reason.trim().to_owned();
    if !req.dry_run && reason.is_empty() {
        return Err(ApiError::invalid_request(
            "say why the content is obliterated (reason)",
        ));
    }
    if reason.chars().count() > MAX_REASON {
        return Err(ApiError::invalid_request(format!(
            "the reason is longer than {MAX_REASON} characters"
        )));
    }

    let sel = select(state, depot, &req.targets).await?;
    let ids: HashSet<ObjectId> = sel.keys().copied().collect();
    let objects = describe(state, depot, &sel).await?;
    let bytes = sel.values().map(|(size, _)| *size).sum();
    let mut clean = HashSet::new();
    let in_use = in_active_heads(state, depot, &ids, &mut clean).await?;
    if req.dry_run {
        return Ok(ObliterateReport {
            dry_run: true,
            objects,
            bytes,
            in_use,
        });
    }
    if !in_use.is_empty() {
        return Err(refused(in_use));
    }
    if sel.is_empty() {
        return Ok(ObliterateReport {
            dry_run: false,
            objects,
            bytes,
            in_use,
        });
    }

    let _running = state.gc_running().try_lock().map_err(|_| {
        ApiError::new(
            ErrorCode::Conflict,
            "a garbage collection or obliterate is running; try again when it has finished",
        )
    })?;
    let _exclusive = state.gc_fence().write().await;
    // Heads may have moved since the first look.
    let in_use = in_active_heads(state, depot, &ids, &mut clean).await?;
    if !in_use.is_empty() {
        return Err(refused(in_use));
    }
    let now = state.clock().now_ms();
    // A chunked file's chunks would let the content be put together again: the ones no other
    // file needs are obliterated with it.
    let chunks = own_chunks(state, depot, &sel).await?;
    let row = |id: &ObjectId, size: u64, path: &str| ObliteratedRow {
        depot_id: depot,
        id: *id,
        size,
        path: path.to_owned(),
        reason: reason.clone(),
        by,
        at: now,
    };
    let mut rows: Vec<ObliteratedRow> = sel
        .iter()
        .map(|(id, (size, path))| row(id, *size, path))
        .collect();
    rows.extend(chunks.iter().map(|(id, (size, path))| row(id, *size, path)));
    state
        .meta()
        .write(move |tx| {
            for r in &rows {
                tx.insert_obliterated(r)?;
            }
            Ok(())
        })
        .await?;
    state.forget_obliterated(depot);
    // Blame results hold file content.
    state.blame_cache().clear();
    state
        .failpoints()
        .check(crate::failpoint::OBLITERATE_AFTER_RECORD)?;
    for id in sel.keys().chain(chunks.keys()) {
        match state.blobs().delete(depot, id).await {
            Ok(()) | Err(StoreError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    tracing::info!(
        depot = depot.0,
        objects = sel.len(),
        bytes,
        by = by.0,
        "content obliterated"
    );
    Ok(ObliterateReport {
        dry_run: false,
        objects,
        bytes,
        in_use: Vec::new(),
    })
}

/// The chunks of the selected chunked files that nothing else reachable uses, with their
/// size and the path of the file they belong to.
async fn own_chunks(
    state: &AppState,
    depot: DepotId,
    sel: &BTreeMap<ObjectId, (u64, String)>,
) -> ApiResult<BTreeMap<ObjectId, (u64, String)>> {
    let mut candidates = BTreeMap::new();
    for (id, (size, path)) in sel {
        if *size < lode_core::CHUNK_THRESHOLD {
            continue;
        }
        match state.chunked().chunk_list(depot, id).await {
            Ok(Some(list)) => {
                for c in list.chunks {
                    if !sel.contains_key(&c.id) {
                        candidates.insert(c.id, (u64::from(c.size), path.clone()));
                    }
                }
            }
            Ok(None) | Err(StoreError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if candidates.is_empty() {
        return Ok(candidates);
    }
    let live = super::gc::live_without(state, depot, sel.keys().copied().collect()).await?;
    candidates.retain(|id, _| !live.contains(id));
    Ok(candidates)
}

fn refused(in_use: Vec<ContentInUse>) -> ApiError {
    let first = in_use
        .first()
        .map(|u| format!(" (first: {} on {})", u.path, u.branch))
        .unwrap_or_default();
    let n = in_use.len();
    ApiError::new(
        ErrorCode::Conflict,
        format!(
            "the content is still in {n} place{} of an active branch{first}; commit its \
             removal or a fixed file first",
            if n == 1 { "" } else { "s" }
        ),
    )
    .with_details(serde_json::json!({ "in_use": in_use }))
}

/// Every revision and path that stored each selected object.
async fn describe(
    state: &AppState,
    depot: DepotId,
    sel: &Selection,
) -> ApiResult<Vec<ObliteratedObject>> {
    let ids: Vec<ObjectId> = sel.keys().copied().collect();
    let (rows, names) = state
        .meta()
        .read(move |tx| {
            let rows = tx.changes_storing(depot, &ids)?;
            let names: HashMap<_, _> = tx
                .branches(depot)?
                .into_iter()
                .map(|b| (b.id, b.name))
                .collect();
            Ok((rows, names))
        })
        .await?;
    let mut uses: HashMap<ObjectId, Vec<ContentUse>> = HashMap::new();
    for r in rows {
        let Some(id) = r.change.new_id else { continue };
        let Ok(path) = DepotPath::parse(&r.change.path) else {
            continue;
        };
        uses.entry(id).or_default().push(ContentUse {
            rev: r.rev,
            branch: names.get(&r.branch_id).cloned().unwrap_or_default(),
            path,
        });
    }
    Ok(sel
        .iter()
        .map(|(id, (size, _))| ObliteratedObject {
            id: *id,
            size: *size,
            uses: uses.remove(id).unwrap_or_default(),
        })
        .collect())
}

/// What was obliterated from `depot`, oldest first.
pub async fn obliterations(state: &AppState, depot: DepotId) -> ApiResult<Vec<Obliteration>> {
    let (rows, names) = state
        .meta()
        .read(move |tx| {
            let rows = tx.obliterations(depot)?;
            let mut names = HashMap::new();
            for r in &rows {
                if let std::collections::hash_map::Entry::Vacant(v) = names.entry(r.by) {
                    v.insert(tx.principal(r.by)?.map(|p| p.name).unwrap_or_default());
                }
            }
            Ok((rows, names))
        })
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Obliteration {
            id: r.id,
            size: r.size,
            by: names.get(&r.by).cloned().unwrap_or_default(),
            path: r.path,
            reason: r.reason,
            at: crate::auth::ms_to_time(r.at),
        })
        .collect())
}
