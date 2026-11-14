//! Online mark-and-sweep garbage collection of one depot.
//!
//! # Why this is safe while commits run
//!
//! Revisions are never removed, so an object referenced by a committed revision must never be
//! deleted, and an object about to be referenced by a commit in flight must not be either.
//!
//! 1. Mark walks the root tree of every revision, memoized by tree id, plus the roots every
//!    module reports through `gc_roots`.
//! 2. Candidates are the stored objects that are unmarked and older than the grace period.
//!    Objects stored after the listing are never candidates.
//! 3. Deletion happens in batches, each under the exclusive [`AppState::gc_fence`]. A commit
//!    holds the fence shared from its blob checks until its transaction ends, so while a batch
//!    runs no commit is between "I checked my objects exist" and "my revision is written".
//!    Before deleting, the batch marks every revision committed since the last mark and asks
//!    the modules for their roots again. Whatever a finished commit references is therefore
//!    marked; a commit that starts after the batch finds a deleted object missing and gets
//!    `missing_objects`, so the client uploads it again.
//!
//! The grace period protects uploads whose commit has not started yet: without it, a client
//! that uploads and then commits a little later would often have to upload again.

use std::collections::HashSet;
use std::time::{Duration, UNIX_EPOCH};

use futures::StreamExt;
use lode_core::{DepotId, EntryKind, ObjectId, ObjectKind};
use lode_meta::{Reads, RevisionFilter};
use lode_proto::admin::MaintenanceReport;
use lode_store::StoreError;

use crate::commit::{TreeLoadError, TreeSource};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Objects deleted per hold of the fence; commits wait at most one batch.
const BATCH: usize = 512;
/// Revisions read per query while marking.
const REV_PAGE: u32 = 1_000;

/// How to run a collection.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcOptions {
    /// Count what would be removed without removing it.
    pub dry_run: bool,
}

/// The live set and how far revisions have been marked.
struct Mark {
    depot: DepotId,
    live: HashSet<ObjectId>,
    trees_seen: HashSet<ObjectId>,
    last_rev: u64,
}

fn internal(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "gc: {what}");
    ApiError::internal()
}

impl Mark {
    /// Marks everything reachable from a tree, visiting each tree once.
    async fn tree(&mut self, state: &AppState, root: ObjectId) -> Result<(), TreeLoadError> {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !self.trees_seen.insert(id) {
                continue;
            }
            self.live.insert(id);
            let tree = state.trees().tree(self.depot, &id).await?;
            for e in tree.entries() {
                if e.kind == EntryKind::Dir {
                    stack.push(e.id);
                } else {
                    self.live.insert(e.id);
                }
            }
        }
        Ok(())
    }

    /// Marks the trees of every revision above `last_rev`.
    async fn revisions(&mut self, state: &AppState) -> ApiResult<()> {
        loop {
            let d = self.depot;
            let min = self.last_rev + 1;
            let revs = state
                .meta()
                .read(move |tx| {
                    tx.find_revisions(
                        d,
                        &RevisionFilter {
                            min: Some(min),
                            ascending: true,
                            limit: REV_PAGE,
                            ..RevisionFilter::default()
                        },
                    )
                })
                .await?;
            let Some(last) = revs.last().map(|r| r.num) else {
                return Ok(());
            };
            for r in &revs {
                self.tree(state, r.root_tree)
                    .await
                    .map_err(|e| internal("cannot read a revision's tree; nothing deleted", e))?;
            }
            self.last_rev = last;
        }
    }

    /// Marks what the modules keep alive. A root that is a tree is walked.
    async fn module_roots(&mut self, state: &AppState) -> ApiResult<()> {
        let d = self.depot;
        let modules = state.modules().to_vec();
        let roots = state
            .meta()
            .read(move |tx| {
                let mut out = Vec::new();
                for m in &modules {
                    m.gc_roots(tx, d, &mut |id| out.push(id))?;
                }
                Ok(out)
            })
            .await?;
        for id in roots {
            if self.live.contains(&id) {
                continue;
            }
            match state.blobs().get_stored(d, &id).await {
                Ok(s) if s.kind == ObjectKind::Tree => {
                    drop(s);
                    self.tree(state, id)
                        .await
                        .map_err(|e| internal("cannot read a module root tree", e))?;
                }
                Ok(_) | Err(StoreError::NotFound(_)) => {
                    self.live.insert(id);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

/// Collects garbage in `depot`. One collection runs at a time per server.
pub async fn collect_garbage(
    state: &AppState,
    depot: DepotId,
    opts: GcOptions,
) -> ApiResult<MaintenanceReport> {
    let _running = state.gc_running().try_lock().map_err(|_| {
        ApiError::new(
            lode_core::ErrorCode::Conflict,
            "a garbage collection is already running",
        )
    })?;
    let mut mark = Mark {
        depot,
        live: HashSet::new(),
        trees_seen: HashSet::new(),
        last_rev: 0,
    };
    mark.revisions(state).await?;
    mark.module_roots(state).await?;
    state.failpoints().check(crate::failpoint::GC_AFTER_MARK)?;

    // List after marking: anything stored later is not a candidate.
    // Obliterated content still stored (a crash between the record and the deletion, or an
    // upload that raced the obliterate) goes whatever the mark and the grace period say.
    let obliterated = state.obliterated(depot).await?;
    let grace = Duration::from_secs(state.config().gc.grace_seconds);
    let now_ms = u128::try_from(state.clock().now_ms()).unwrap_or(0);
    let mut checked = 0u64;
    let mut candidates = Vec::new();
    let mut listing = state.blobs().list(depot);
    while let Some(item) = listing.next().await {
        let (id, modified) = item.map_err(|e| internal("cannot list objects", e))?;
        checked += 1;
        if obliterated.contains(&id) {
            candidates.push(id);
            continue;
        }
        if mark.live.contains(&id) {
            continue;
        }
        let modified_ms = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        if now_ms.saturating_sub(modified_ms) > grace.as_millis() {
            candidates.push(id);
        }
    }
    drop(listing);

    let mut report = MaintenanceReport {
        objects_checked: checked,
        objects_affected: 0,
        bytes_affected: 0,
        dry_run: opts.dry_run,
        problems: Vec::new(),
    };
    for batch in candidates.chunks(BATCH) {
        let _exclusive = state.gc_fence().write().await;
        // Commits that finished since the last batch may reference candidates.
        mark.revisions(state).await?;
        mark.module_roots(state).await?;
        for id in batch {
            if mark.live.contains(id) && !obliterated.contains(id) {
                continue;
            }
            let size = match state.blobs().get_stored(depot, id).await {
                Ok(s) => s.stored_len,
                Err(StoreError::NotFound(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            if !opts.dry_run {
                match state.blobs().delete(depot, id).await {
                    Ok(()) | Err(StoreError::NotFound(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            report.objects_affected += 1;
            report.bytes_affected += size;
            state.failpoints().check(crate::failpoint::GC_IN_BATCH)?;
        }
    }
    tracing::info!(
        depot = depot.0,
        checked = report.objects_checked,
        removed = report.objects_affected,
        bytes = report.bytes_affected,
        dry_run = opts.dry_run,
        "garbage collection finished"
    );
    Ok(report)
}
