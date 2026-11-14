//! Manifests (full and delta), directory listings and file content.
//!
//! Manifests are produced by a task that walks the trees and sends NDJSON in chunks, so
//! memory stays flat however many files the tree holds.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use bytes::{BufMut, Bytes, BytesMut};
use lode_core::{DepotId, DepotPath, EntryKind, ErrorCode, ObjectId, Tree, TreeEntry};
use lode_proto::read::{ManifestDelta, ManifestEntry, TreeEntryView};
use lode_store::{ByteStream, StoreError};
use tokio::sync::mpsc;

use super::Visibility;
use crate::commit::{lookup, TreeLoadError, TreeSource};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Bytes collected before a chunk is sent.
const CHUNK: usize = 64 * 1024;

type Chunk = Result<Bytes, std::io::Error>;

/// Collects NDJSON lines and sends them in chunks.
struct Sink {
    tx: mpsc::Sender<Chunk>,
    buf: BytesMut,
}

impl Sink {
    /// Appends one line; `false` once the client has gone away.
    async fn line<T: serde::Serialize>(&mut self, v: &T) -> bool {
        if serde_json::to_writer((&mut self.buf).writer(), v).is_err() {
            return false;
        }
        self.buf.put_u8(b'\n');
        if self.buf.len() >= CHUNK {
            let chunk = self.buf.split().freeze();
            return self.tx.send(Ok(chunk)).await.is_ok();
        }
        true
    }

    async fn finish(mut self) {
        if !self.buf.is_empty() {
            let chunk = self.buf.split().freeze();
            let _ = self.tx.send(Ok(chunk)).await;
        }
    }

    /// Ends the body with an error, so the client sees a broken stream, not a short one.
    async fn fail(self, e: TreeLoadError) {
        tracing::error!(error = %e, "manifest walk failed");
        let _ = self
            .tx
            .send(Err(std::io::Error::other("manifest walk failed")))
            .await;
    }
}

fn stream_body<F, Fut>(work: F) -> Body
where
    F: FnOnce(Sink) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Chunk>(4);
    let sink = Sink {
        tx,
        buf: BytesMut::with_capacity(CHUNK + 1024),
    };
    tokio::spawn(work(sink));
    Body::from_stream(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

fn child(prefix: Option<&DepotPath>, name: &str) -> Option<DepotPath> {
    let r = match prefix {
        Some(p) => p.join(name),
        None => DepotPath::parse(name),
    };
    match r {
        Ok(p) => Some(p),
        Err(e) => {
            // Trees are validated when committed; a bad name means a damaged object.
            tracing::error!(error = %e, "tree holds an invalid name");
            None
        }
    }
}

fn entry_line(path: DepotPath, e: &TreeEntry) -> ManifestEntry {
    ManifestEntry {
        path,
        kind: e.kind,
        id: e.id,
        size: e.size,
        executable: e.executable(),
    }
}

/// Streams every visible file of the tree at `root`.
pub fn full_manifest(state: AppState, depot: DepotId, root: ObjectId, vis: Visibility) -> Body {
    stream_body(move |mut sink| async move {
        let mut stack: Vec<(Option<DepotPath>, ObjectId)> = vec![(None, root)];
        while let Some((prefix, id)) = stack.pop() {
            let tree = match state.trees().tree(depot, &id).await {
                Ok(t) => t,
                Err(e) => return sink.fail(e).await,
            };
            let mut dirs = Vec::new();
            for e in tree.entries() {
                let Some(path) = child(prefix.as_ref(), &e.name) else {
                    continue;
                };
                if e.kind == EntryKind::Dir {
                    dirs.push((Some(path), e.id));
                } else if vis.shows(&path) && !sink.line(&entry_line(path, e)).await {
                    return;
                }
            }
            // Reverse so directories come out in name order.
            stack.extend(dirs.into_iter().rev());
        }
        sink.finish().await;
    })
}

enum Step {
    Pair(Option<DepotPath>, Option<ObjectId>, Option<ObjectId>),
    Put(DepotPath, TreeEntry),
}

async fn load(
    state: &AppState,
    depot: DepotId,
    id: Option<ObjectId>,
) -> Result<Option<Arc<Tree>>, TreeLoadError> {
    match id {
        Some(id) => Ok(Some(state.trees().tree(depot, &id).await?)),
        None => Ok(None),
    }
}

/// Streams the differences between the trees at `old_root` and `new_root`: a `put` for each
/// visible file that is new or changed, a `del` for each visible file that is gone. Each side
/// is filtered by its own visibility. When both use the same rules, identical subtrees are
/// skipped by id.
pub fn delta_manifest(
    state: AppState,
    depot: DepotId,
    old_root: ObjectId,
    new_root: ObjectId,
    old_vis: Visibility,
    new_vis: Visibility,
    same_rules: bool,
) -> Body {
    stream_body(move |mut sink| async move {
        let mut stack = vec![Step::Pair(None, Some(old_root), Some(new_root))];
        while let Some(step) = stack.pop() {
            let (prefix, old_id, new_id) = match step {
                Step::Put(path, e) => {
                    if !sink.line(&ManifestDelta::Put(entry_line(path, &e))).await {
                        return;
                    }
                    continue;
                }
                Step::Pair(p, o, n) => (p, o, n),
            };
            if same_rules && old_id == new_id {
                continue;
            }
            let (old, new) = match (
                load(&state, depot, old_id).await,
                load(&state, depot, new_id).await,
            ) {
                (Ok(o), Ok(n)) => (o, n),
                (Err(e), _) | (_, Err(e)) => return sink.fail(e).await,
            };
            let names: BTreeSet<&str> = old
                .iter()
                .chain(new.iter())
                .flat_map(|t| t.entries().iter().map(|e| e.name.as_str()))
                .collect();
            let mut deferred: Vec<Step> = Vec::new();
            for name in names {
                let o = old.as_ref().and_then(|t| t.get(name));
                let n = new.as_ref().and_then(|t| t.get(name));
                let Some(path) = child(prefix.as_ref(), name) else {
                    continue;
                };
                let is_dir = |e: Option<&TreeEntry>| e.is_some_and(|e| e.kind == EntryKind::Dir);
                let is_file = |e: Option<&TreeEntry>| e.is_some_and(|e| e.kind != EntryKind::Dir);
                // Directory sides recurse; deletions under an old directory must come before
                // a file that replaces it.
                if is_dir(o) || is_dir(n) {
                    let od = o.filter(|e| e.kind == EntryKind::Dir).map(|e| e.id);
                    let nd = n.filter(|e| e.kind == EntryKind::Dir).map(|e| e.id);
                    if is_file(o) && old_vis.shows(&path) {
                        let del = ManifestDelta::Del { path: path.clone() };
                        if !sink.line(&del).await {
                            return;
                        }
                    }
                    deferred.push(Step::Pair(Some(path.clone()), od, nd));
                    if let Some(ne) = n.filter(|e| e.kind != EntryKind::Dir) {
                        if new_vis.shows(&path) {
                            deferred.push(Step::Put(path, ne.clone()));
                        }
                    }
                    continue;
                }
                let was = o.filter(|_| old_vis.shows(&path));
                let now = n.filter(|_| new_vis.shows(&path));
                match (was, now) {
                    (Some(a), Some(b))
                        if a.id == b.id && a.kind == b.kind && a.flags == b.flags => {}
                    (_, Some(b)) => {
                        let put = ManifestDelta::Put(entry_line(path, b));
                        if !sink.line(&put).await {
                            return;
                        }
                    }
                    (Some(_), None) => {
                        if !sink.line(&ManifestDelta::Del { path }).await {
                            return;
                        }
                    }
                    (None, None) => {}
                }
            }
            stack.extend(deferred.into_iter().rev());
        }
        sink.finish().await;
    })
}

/// Whether the subtree at `id` holds at least one visible file.
async fn has_visible(
    state: &AppState,
    depot: DepotId,
    dir: DepotPath,
    id: ObjectId,
    vis: &Visibility,
) -> Result<bool, TreeLoadError> {
    if vis.is_everything() {
        return Ok(true);
    }
    let mut stack = vec![(dir, id)];
    while let Some((prefix, id)) = stack.pop() {
        let tree = state.trees().tree(depot, &id).await?;
        for e in tree.entries() {
            let Some(path) = child(Some(&prefix), &e.name) else {
                continue;
            };
            if e.kind == EntryKind::Dir {
                stack.push((path, e.id));
            } else if vis.shows(&path) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn load_error(e: TreeLoadError) -> ApiError {
    tracing::error!(error = %e, "cannot load a tree");
    ApiError::internal()
}

/// The visible entries of the directory at `path` (the root when `None`). A directory with
/// nothing visible in it is `not_found`, like one that does not exist.
pub async fn list_tree(
    state: &AppState,
    depot: DepotId,
    root: ObjectId,
    path: Option<&DepotPath>,
    vis: &Visibility,
) -> ApiResult<Vec<TreeEntryView>> {
    let not_found = || ApiError::new(ErrorCode::NotFound, "no such directory");
    let dir_id = match path {
        None => root,
        Some(p) => {
            let e = lookup(state.trees(), depot, &root, p)
                .await
                .map_err(load_error)?
                .ok_or_else(not_found)?;
            if e.kind != EntryKind::Dir {
                if vis.shows(p) {
                    return Err(ApiError::invalid_request(format!(
                        "{} is not a directory",
                        p.as_str()
                    )));
                }
                return Err(not_found());
            }
            e.id
        }
    };
    let tree = state
        .trees()
        .tree(depot, &dir_id)
        .await
        .map_err(load_error)?;
    let mut out = Vec::new();
    for e in tree.entries() {
        let Some(child_path) = child(path, &e.name) else {
            continue;
        };
        let visible = if e.kind == EntryKind::Dir {
            has_visible(state, depot, child_path, e.id, vis)
                .await
                .map_err(load_error)?
        } else {
            vis.shows(&child_path)
        };
        if visible {
            out.push(TreeEntryView {
                name: e.name.clone(),
                kind: e.kind,
                id: e.id,
                size: if e.kind == EntryKind::Dir { 0 } else { e.size },
                executable: e.executable(),
            });
        }
    }
    if out.is_empty() && path.is_some() {
        return Err(not_found());
    }
    Ok(out)
}

/// A file's content and metadata.
pub struct FileContent {
    /// The tree entry.
    pub entry: TreeEntry,
    /// Decoded content.
    pub reader: ByteStream,
    /// Length of the content.
    pub len: u64,
}

/// Opens the file at `path`. Missing, unreadable and directories are all `not_found`.
pub async fn open_file(
    state: &AppState,
    depot: DepotId,
    root: ObjectId,
    path: &DepotPath,
    vis: &Visibility,
) -> ApiResult<FileContent> {
    let not_found = || ApiError::new(ErrorCode::NotFound, format!("no file {}", path.as_str()));
    if !vis.shows(path) {
        return Err(not_found());
    }
    let entry = lookup(state.trees(), depot, &root, path)
        .await
        .map_err(load_error)?
        .filter(|e| e.kind != EntryKind::Dir)
        .ok_or_else(not_found)?;
    if state.is_obliterated(depot, &entry.id).await? {
        return Err(crate::maintenance::gone(&entry.id));
    }
    let obj = match state.blobs().get(depot, &entry.id).await {
        Ok(o) => o,
        Err(StoreError::NotFound(_)) => {
            return Err(crate::maintenance::missing_object(state, depot, &entry.id).await)
        }
        Err(e) => return Err(e.into()),
    };
    Ok(FileContent {
        entry,
        len: obj.raw_len,
        reader: obj.reader,
    })
}

/// One file that differs between two trees. A side is `None` where the file is absent or
/// hidden from the caller.
#[derive(Debug, Clone)]
pub struct FileChange {
    /// Path of the file.
    pub path: DepotPath,
    /// The entry in the old tree.
    pub old: Option<TreeEntry>,
    /// The entry in the new tree.
    pub new: Option<TreeEntry>,
}

/// The visible files that differ between the trees at `old_root` and `new_root`, below
/// `under` when given, sorted by path. Each side is filtered by its own visibility; when
/// both use the same rules, identical subtrees are skipped by id. Stops after `limit`
/// changes and reports whether more were left.
#[allow(clippy::too_many_arguments)]
pub async fn changed_files(
    state: &AppState,
    depot: DepotId,
    old_root: ObjectId,
    new_root: ObjectId,
    old_vis: &Visibility,
    new_vis: &Visibility,
    same_rules: bool,
    under: Option<&DepotPath>,
    limit: usize,
) -> ApiResult<(Vec<FileChange>, bool)> {
    let mut out = Vec::new();
    let mut stack: Vec<(Option<DepotPath>, Option<ObjectId>, Option<ObjectId>)> = Vec::new();
    match under {
        None => stack.push((None, Some(old_root), Some(new_root))),
        Some(p) => {
            let o = lookup(state.trees(), depot, &old_root, p)
                .await
                .map_err(load_error)?;
            let n = lookup(state.trees(), depot, &new_root, p)
                .await
                .map_err(load_error)?;
            let dir = |e: &Option<TreeEntry>| {
                e.as_ref()
                    .filter(|e| e.kind == EntryKind::Dir)
                    .map(|e| e.id)
            };
            stack.push((Some(p.clone()), dir(&o), dir(&n)));
            let file = |e: Option<TreeEntry>, vis: &Visibility| {
                e.filter(|e| e.kind != EntryKind::Dir && vis.shows(p))
            };
            push_change(&mut out, p.clone(), file(o, old_vis), file(n, new_vis));
        }
    }
    while let Some((prefix, old_id, new_id)) = stack.pop() {
        if out.len() > limit {
            break;
        }
        if old_id.is_none() && new_id.is_none() {
            continue;
        }
        if same_rules && old_id == new_id {
            continue;
        }
        let old = load(state, depot, old_id).await.map_err(load_error)?;
        let new = load(state, depot, new_id).await.map_err(load_error)?;
        let names: BTreeSet<&str> = old
            .iter()
            .chain(new.iter())
            .flat_map(|t| t.entries().iter().map(|e| e.name.as_str()))
            .collect();
        for name in names {
            let o = old.as_ref().and_then(|t| t.get(name));
            let n = new.as_ref().and_then(|t| t.get(name));
            let Some(path) = child(prefix.as_ref(), name) else {
                continue;
            };
            let od = o.filter(|e| e.kind == EntryKind::Dir).map(|e| e.id);
            let nd = n.filter(|e| e.kind == EntryKind::Dir).map(|e| e.id);
            if od.is_some() || nd.is_some() {
                stack.push((Some(path.clone()), od, nd));
            }
            let was = o.filter(|e| e.kind != EntryKind::Dir && old_vis.shows(&path));
            let now = n.filter(|e| e.kind != EntryKind::Dir && new_vis.shows(&path));
            push_change(&mut out, path, was.cloned(), now.cloned());
        }
    }
    out.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    let more = out.len() > limit;
    out.truncate(limit);
    Ok((out, more))
}

fn push_change(
    out: &mut Vec<FileChange>,
    path: DepotPath,
    old: Option<TreeEntry>,
    new: Option<TreeEntry>,
) {
    let same = match (&old, &new) {
        (Some(a), Some(b)) => a.id == b.id && a.kind == b.kind && a.flags == b.flags,
        (None, None) => true,
        _ => false,
    };
    if !same {
        out.push(FileChange { path, old, new });
    }
}
