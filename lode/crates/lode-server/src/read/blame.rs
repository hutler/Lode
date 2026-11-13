//! Blame: the revision that last changed each line of a text file (ARCHITECTURE §6).
//!
//! The file's versions come from `rev_change` along the branch chain, continuing through
//! `copy_from` into the source path. Successive versions are diffed line by line, newest
//! first; a line that has no counterpart in the older version was changed by the newer one.
//! Results are cached by depot, path and the revision of the newest version, which fixes the
//! whole history walked.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lode_core::{DepotId, DepotPath, EntryKind, ErrorCode, ObjectId};
use lode_meta::{BranchId, DepotRow, MetaError, Reads};
use lode_proto::read::{BlameLine, BlameRevision, BlameView};
use lode_store::StoreError;
use similar::{capture_diff_slices_deadline, Algorithm, DiffOp};
use tokio::io::AsyncReadExt;

use super::history::principal_ref;
use super::readable_rev;
use crate::auth::{ms_to_time, Caller};
use crate::commit::lookup;
use crate::error::{ApiError, ApiResult};
use crate::perm::{DepotAccess, Level};
use crate::state::AppState;

/// Most versions of a file looked at; older lines go to the oldest one (`truncated`).
pub const BLAME_MAX_VERSIONS: usize = 1000;
/// Largest version blamed, in bytes.
pub const BLAME_FILE_MAX: u64 = 1024 * 1024;
/// Time allowed for one diff before it settles for a less minimal answer.
const DIFF_DEADLINE: Duration = Duration::from_secs(2);
/// Blame results kept in memory.
pub const BLAME_CACHE_ENTRIES: usize = 64;

/// Cache key: depot, path, revision of the newest version.
pub type BlameKey = (DepotId, String, u64);

/// A computed blame: the lines of the newest version and the revision of each.
#[derive(Debug)]
pub struct BlameData {
    lines: Vec<String>,
    revs: Vec<u64>,
    truncated: bool,
}

/// One version of the file, newest first.
struct Version {
    rev: u64,
    id: ObjectId,
}

/// The versions to look at, and whether the walk stopped before the file's creation.
struct Walk {
    versions: Vec<Version>,
    truncated: bool,
    /// Stopped at history the caller cannot read: the result is not shared through the cache.
    hidden: bool,
}

fn not_text(path: &DepotPath) -> ApiError {
    ApiError::invalid_request(format!("{path} is not a text file"))
}

/// Walks the history of `path` as seen from `branch` at `rev`.
fn walk(
    tx: &dyn Reads,
    depot: DepotId,
    access: &DepotAccess,
    branch: BranchId,
    rev: u64,
    path: &DepotPath,
) -> Result<Walk, MetaError> {
    let mut out = Walk {
        versions: Vec::new(),
        truncated: false,
        hidden: false,
    };
    let mut readable: HashMap<(BranchId, String), bool> = HashMap::new();
    let (mut cur_path, mut branch, mut before) = (path.as_str().to_owned(), branch, rev + 1);
    'paths: loop {
        let left = BLAME_MAX_VERSIONS + 1 - out.versions.len();
        let limit = u32::try_from(left).unwrap_or(u32::MAX);
        let history = tx.path_history(depot, branch, &cur_path, Some(before), limit)?;
        for h in history {
            let key = (h.branch_id, cur_path.clone());
            let can_read = match readable.get(&key) {
                Some(r) => *r,
                None => {
                    let name = tx.branch_by_id(h.branch_id)?.map(|b| b.name);
                    let r = match (name, DepotPath::parse(&cur_path)) {
                        (Some(n), Ok(p)) => access.branch(&n).allows(&p, Level::Read),
                        _ => false,
                    };
                    readable.insert(key, r);
                    r
                }
            };
            if !can_read {
                out.truncated = true;
                out.hidden = true;
                break 'paths;
            }
            let c = &h.change;
            // Deleted before it was created again, or no longer a file: the history ends.
            let (Some(id), Some(1)) = (c.new_id, c.kind) else {
                break 'paths;
            };
            if c.action == 'D' {
                break 'paths;
            }
            if out.versions.len() == BLAME_MAX_VERSIONS {
                out.truncated = true;
                break 'paths;
            }
            out.versions.push(Version { rev: h.rev, id });
            if c.action == 'A' {
                match (&c.copy_from_path, c.copy_from_rev) {
                    (Some(p), Some(r)) => match tx.revision(depot, r)? {
                        Some(src) => {
                            cur_path.clone_from(p);
                            branch = src.branch_id;
                            before = r + 1;
                            continue 'paths;
                        }
                        None => break 'paths,
                    },
                    _ => break 'paths,
                }
            }
        }
        break;
    }
    Ok(out)
}

/// Reads a version as text, or `None` when it is too large or not text.
async fn text_of(state: &AppState, depot: DepotId, id: &ObjectId) -> ApiResult<Option<String>> {
    let obj = match state.blobs().get(depot, id).await {
        Ok(o) => o,
        Err(StoreError::NotFound(_)) => {
            tracing::error!(%id, "a committed blob is missing from the store");
            return Err(ApiError::internal());
        }
        Err(e) => return Err(e.into()),
    };
    if obj.raw_len > BLAME_FILE_MAX {
        return Ok(None);
    }
    let mut buf = Vec::new();
    let mut reader = obj.reader;
    (&mut reader)
        .take(BLAME_FILE_MAX + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "cannot read a stored file");
            ApiError::internal()
        })?;
    if buf.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(buf).ok())
}

fn split(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

/// For each line of `new`, its index in `old` when it is unchanged.
fn line_map(old: &[String], new: &[String]) -> Vec<Option<usize>> {
    let deadline = Instant::now() + DIFF_DEADLINE;
    let mut map = vec![None; new.len()];
    for op in capture_diff_slices_deadline(Algorithm::Myers, old, new, Some(deadline)) {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for k in 0..len {
                map[new_index + k] = Some(old_index + k);
            }
        }
    }
    map
}

/// Attributes the newest version's lines by diffing the versions pairwise.
async fn compute(
    state: &AppState,
    depot: DepotId,
    path: &DepotPath,
    walk: &Walk,
) -> ApiResult<BlameData> {
    let Some(newest) = walk.versions.first() else {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("no history for {path}"),
        ));
    };
    let text = text_of(state, depot, &newest.id)
        .await?
        .ok_or_else(|| not_text(path))?;
    let lines = split(&text);
    let mut revs: Vec<Option<u64>> = vec![None; lines.len()];
    // Where each unattributed line is in the version being looked at.
    let mut pos: Vec<usize> = (0..lines.len()).collect();
    let mut current = lines.clone();
    let mut truncated = walk.truncated;
    for (k, v) in walk.versions.iter().enumerate() {
        if revs.iter().all(Option::is_some) {
            break;
        }
        let older = match walk.versions.get(k + 1) {
            Some(o) => text_of(state, depot, &o.id).await?,
            None => None,
        };
        let Some(older) = older else {
            if k + 1 < walk.versions.len() {
                truncated = true;
            }
            for r in revs.iter_mut().filter(|r| r.is_none()) {
                *r = Some(v.rev);
            }
            break;
        };
        let older = split(&older);
        let (map, older) = tokio::task::spawn_blocking(move || {
            let map = line_map(&older, &current);
            (map, older)
        })
        .await
        .map_err(|_| ApiError::internal())?;
        for (i, r) in revs.iter_mut().enumerate() {
            if r.is_some() {
                continue;
            }
            match map[pos[i]] {
                Some(j) => pos[i] = j,
                None => *r = Some(v.rev),
            }
        }
        current = older;
    }
    let fallback = walk.versions.last().map_or(0, |v| v.rev);
    Ok(BlameData {
        lines,
        revs: revs.into_iter().map(|r| r.unwrap_or(fallback)).collect(),
        truncated,
    })
}

/// Blames `path` at revision `rev` of `depot`. A missing or unreadable file is `not_found`;
/// a symlink, a binary file or one over [`BLAME_FILE_MAX`] is `invalid_request`.
pub async fn blame(
    state: &AppState,
    caller: &Caller,
    depot: &DepotRow,
    rev: u64,
    path: &DepotPath,
) -> ApiResult<BlameView> {
    let target = readable_rev(state, caller, depot.id, rev).await?;
    let not_found = || ApiError::new(ErrorCode::NotFound, format!("no file {path}"));
    if !target.access.allows(path, Level::Read) {
        return Err(not_found());
    }
    let entry = lookup(state.trees(), depot.id, &target.rev.root_tree, path)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "cannot load a tree");
            ApiError::internal()
        })?
        .filter(|e| e.kind != EntryKind::Dir)
        .ok_or_else(not_found)?;
    if entry.kind != EntryKind::File {
        return Err(not_text(path));
    }
    if entry.size > BLAME_FILE_MAX {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!("{path} is larger than {BLAME_FILE_MAX} bytes; blame is not available"),
        ));
    }

    let access = state.permissions().depot(caller, depot.id);
    let (d, branch, p) = (depot.id, target.branch.id, path.clone());
    let walked = state
        .meta()
        .read(move |tx| walk(tx, d, &access, branch, rev, &p))
        .await?;
    let key = walked
        .versions
        .first()
        .map(|v| (depot.id, path.as_str().to_owned(), v.rev));
    let cached = match (&key, walked.hidden) {
        (Some(k), false) => state.blame_cache().get(k),
        _ => None,
    };
    let data = match cached {
        Some(d) => d,
        None => {
            let d = Arc::new(compute(state, depot.id, path, &walked).await?);
            if let (Some(k), false) = (key, walked.hidden) {
                state.blame_cache().insert(k, d.clone());
            }
            d
        }
    };

    let wanted: BTreeSet<u64> = data.revs.iter().copied().collect();
    let d = depot.id;
    let revisions = state
        .meta()
        .read(move |tx| {
            let mut out = Vec::with_capacity(wanted.len());
            let mut names: HashMap<BranchId, String> = HashMap::new();
            for r in wanted.into_iter().rev() {
                let Some(row) = tx.revision(d, r)? else {
                    continue;
                };
                let branch = match names.get(&row.branch_id) {
                    Some(n) => n.clone(),
                    None => {
                        let n = tx
                            .branch_by_id(row.branch_id)?
                            .map(|b| b.name)
                            .unwrap_or_default();
                        names.insert(row.branch_id, n.clone());
                        n
                    }
                };
                out.push(BlameRevision {
                    rev: r,
                    branch,
                    author: principal_ref(tx, row.author_id)?,
                    created_at: ms_to_time(row.created_at),
                    summary: row.message.lines().next().unwrap_or_default().to_owned(),
                });
            }
            Ok(out)
        })
        .await?;
    let lines = data
        .lines
        .iter()
        .zip(&data.revs)
        .map(|(l, r)| BlameLine {
            rev: *r,
            text: l
                .strip_suffix('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l))
                .unwrap_or(l)
                .to_owned(),
        })
        .collect();
    Ok(BlameView {
        path: path.clone(),
        rev,
        id: entry.id,
        lines,
        revisions,
        truncated: data.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::{line_map, split};

    #[test]
    fn unchanged_lines_map_to_the_old_version() {
        let old = split("a\nb\nc\n");
        let new = split("a\nB\nc\nd\n");
        assert_eq!(line_map(&old, &new), vec![Some(0), None, Some(2), None]);
    }
}
