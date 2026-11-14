//! Consistency check of one depot (ARCHITECTURE §11).
//!
//! Quick mode: every revision's tree decodes, every referenced object exists with the right
//! kind and a header whose length matches its payload where that can be known without
//! decoding, branch heads and parents point at real revisions, and the revision counter is
//! ahead of every revision. Full mode also decodes and re-hashes every stored object.

use std::collections::HashSet;

use futures::StreamExt;
use lode_core::{DepotId, EntryKind, ObjectId, ObjectKind, TREE_ID_CONTEXT};
use lode_meta::{Reads, RevisionFilter};
use lode_proto::admin::MaintenanceReport;
use lode_store::StoreError;
use tokio::io::AsyncReadExt;

use crate::commit::TreeSource;
use crate::error::ApiResult;
use crate::state::AppState;

/// How thorough a check is.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsckOptions {
    /// Decode and re-hash every stored object.
    pub full: bool,
}

/// Revisions read per query.
const REV_PAGE: u32 = 1_000;

struct Fsck<'a> {
    state: &'a AppState,
    depot: DepotId,
    /// Objects already checked, with whether they were fine.
    checked: std::collections::HashMap<ObjectId, bool>,
    problems: Vec<String>,
    bad_objects: HashSet<ObjectId>,
    /// Content obliterated from the depot: expected to be missing.
    obliterated: std::sync::Arc<HashSet<ObjectId>>,
}

impl Fsck<'_> {
    fn problem(&mut self, id: Option<ObjectId>, text: String) {
        if let Some(id) = id {
            self.bad_objects.insert(id);
        }
        self.problems.push(text);
    }

    /// Checks one referenced object's presence, kind and header. Remembers the answer.
    async fn object(&mut self, id: ObjectId, kind: ObjectKind, at: &str) -> bool {
        if let Some(ok) = self.checked.get(&id) {
            return *ok;
        }
        let ok = match self.state.blobs().get_stored(self.depot, &id).await {
            Ok(s) if s.kind == kind => true,
            Ok(s) => {
                self.problem(
                    Some(id),
                    format!("{at}: object {id} is a {:?}, expected {kind:?}", s.kind),
                );
                false
            }
            Err(StoreError::NotFound(_)) if self.obliterated.contains(&id) => true,
            Err(StoreError::NotFound(_)) => {
                self.problem(Some(id), format!("{at}: object {id} is missing"));
                false
            }
            Err(e) => {
                self.problem(Some(id), format!("{at}: object {id} is damaged: {e}"));
                false
            }
        };
        self.checked.insert(id, ok);
        ok
    }

    /// Walks one revision's tree, each tree once across the whole check.
    async fn revision_tree(&mut self, rev: u64, root: ObjectId) {
        let mut stack = vec![(String::new(), root)];
        while let Some((path, id)) = stack.pop() {
            if self.checked.contains_key(&id) {
                continue;
            }
            let at = if path.is_empty() {
                format!("r{rev} root")
            } else {
                format!("r{rev} {path}")
            };
            if !self.object(id, ObjectKind::Tree, &at).await {
                continue;
            }
            let tree = match self.state.trees().tree(self.depot, &id).await {
                Ok(t) => t,
                Err(e) => {
                    self.checked.insert(id, false);
                    self.problem(Some(id), format!("{at}: tree {id} does not decode: {e}"));
                    continue;
                }
            };
            for e in tree.entries() {
                let child = if path.is_empty() {
                    e.name.clone()
                } else {
                    format!("{path}/{}", e.name)
                };
                if e.kind == EntryKind::Dir {
                    stack.push((child, e.id));
                } else {
                    self.object(e.id, ObjectKind::Blob, &format!("r{rev} {child}"))
                        .await;
                }
            }
        }
    }

    /// Re-hashes every stored object.
    async fn rehash_all(&mut self) -> ApiResult<u64> {
        let mut ids = Vec::new();
        let mut listing = self.state.blobs().list(self.depot);
        while let Some(item) = listing.next().await {
            match item {
                Ok((id, _)) => ids.push(id),
                Err(e) => self.problems.push(format!("cannot list objects: {e}")),
            }
        }
        drop(listing);
        let n = ids.len() as u64;
        for id in ids {
            if let Err(why) = self.rehash(&id).await {
                self.problem(Some(id), format!("object {id}: {why}"));
            }
        }
        Ok(n)
    }

    async fn rehash(&self, id: &ObjectId) -> Result<(), String> {
        let obj = match self.state.blobs().get(self.depot, id).await {
            Ok(o) => o,
            Err(StoreError::NotFound(_)) => return Ok(()), // removed by GC meanwhile
            Err(e) => return Err(e.to_string()),
        };
        let mut hasher = match obj.kind {
            ObjectKind::Tree => blake3::Hasher::new_derive_key(TREE_ID_CONTEXT),
            _ => blake3::Hasher::new(),
        };
        let mut reader = obj.reader;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| format!("cannot be read: {e}"))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            hasher.update(buf.get(..n).unwrap_or_default());
        }
        if total != obj.raw_len {
            return Err(format!("has {total} bytes, header says {}", obj.raw_len));
        }
        let actual = ObjectId::from_bytes(*hasher.finalize().as_bytes());
        if actual != *id {
            return Err(format!("content hashes to {actual}"));
        }
        Ok(())
    }
}

/// Checks `depot`. Problems are listed in the report; an empty list means the depot is
/// consistent.
pub async fn fsck(
    state: &AppState,
    depot: DepotId,
    opts: FsckOptions,
) -> ApiResult<MaintenanceReport> {
    let mut f = Fsck {
        state,
        depot,
        checked: std::collections::HashMap::new(),
        problems: Vec::new(),
        bad_objects: HashSet::new(),
        obliterated: state.obliterated(depot).await?,
    };

    // Metadata: branches, parents, the revision counter.
    let d = depot;
    let (meta_problems, max_rev) = state
        .meta()
        .read(move |tx| {
            let mut problems = Vec::new();
            let Some(row) = tx.depot(d)? else {
                return Ok((vec![format!("depot {} does not exist", d.0)], 0));
            };
            let mut max_rev = 0;
            for b in tx.branches(d)? {
                match tx.revision(d, b.head_rev)? {
                    None => problems.push(format!(
                        "branch {}: head r{} does not exist",
                        b.name, b.head_rev
                    )),
                    Some(r) if r.branch_id != b.id => problems.push(format!(
                        "branch {}: head r{} belongs to another branch",
                        b.name, b.head_rev
                    )),
                    Some(_) => {}
                }
                if b.head_rev >= row.next_rev {
                    problems.push(format!(
                        "branch {}: head r{} is not below the next revision number {}",
                        b.name, b.head_rev, row.next_rev
                    ));
                }
                if tx.revision(d, b.created_rev)?.is_none() {
                    problems.push(format!(
                        "branch {}: creating revision r{} does not exist",
                        b.name, b.created_rev
                    ));
                }
            }
            let mut min = 1;
            loop {
                let revs = tx.find_revisions(
                    d,
                    &RevisionFilter {
                        min: Some(min),
                        ascending: true,
                        limit: REV_PAGE,
                        ..RevisionFilter::default()
                    },
                )?;
                let Some(last) = revs.last().map(|r| r.num) else {
                    break;
                };
                for r in &revs {
                    if let Some(p) = r.parent_num {
                        if p >= r.num || tx.revision(d, p)?.is_none() {
                            problems.push(format!("r{}: parent r{p} does not exist", r.num));
                        }
                    }
                    if tx.branch_by_id(r.branch_id)?.is_none() {
                        problems.push(format!("r{}: its branch does not exist", r.num));
                    }
                }
                max_rev = last;
                min = last + 1;
            }
            if row.next_rev <= max_rev {
                problems.push(format!(
                    "next revision number {} is not above the newest revision r{max_rev}",
                    row.next_rev
                ));
            }
            Ok((problems, max_rev))
        })
        .await?;
    f.problems.extend(meta_problems);

    // Trees and referenced objects, revision by revision.
    let mut min = 1;
    while min <= max_rev {
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
            break;
        };
        for r in revs {
            f.revision_tree(r.num, r.root_tree).await;
        }
        min = last + 1;
    }

    // Obliterated content that is still stored, for instance after a crash between the
    // record and the deletion. GC removes it.
    let gone: Vec<ObjectId> = f.obliterated.iter().copied().collect();
    for id in gone {
        match state.blobs().contains(depot, &id).await {
            Ok(true) => f.problem(
                Some(id),
                format!("object {id} was obliterated but is still stored; run gc"),
            ),
            Ok(false) => {}
            Err(e) => f.problems.push(format!("cannot check object {id}: {e}")),
        }
    }

    let mut checked = f.checked.len() as u64;
    if opts.full {
        checked = checked.max(f.rehash_all().await?);
    }
    Ok(MaintenanceReport {
        objects_checked: checked,
        objects_affected: f.bad_objects.len() as u64,
        bytes_affected: 0,
        dry_run: false,
        problems: f.problems,
    })
}
