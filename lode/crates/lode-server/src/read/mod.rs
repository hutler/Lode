//! The read side of a depot (ARCHITECTURE §6): revision history, manifests, directory
//! listings and file content. Every listing is filtered by read permission and by the
//! caller's view.

mod handlers;
mod history;
mod manifest;

pub use handlers::routes;
pub use history::{list_revisions, revision_detail};
pub use manifest::{delta_manifest, full_manifest, list_tree, open_file, FileContent};

use lode_core::{DepotId, DepotPath, ErrorCode, PathGlobSet};
use lode_meta::{BranchRow, Reads, RevisionRow};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::perm::{BranchAccess, Level};
use crate::state::AppState;

/// A workspace view: include and exclude patterns. Without includes everything is included.
#[derive(Debug, Clone)]
pub struct View {
    include: PathGlobSet,
    exclude: PathGlobSet,
}

impl Default for View {
    fn default() -> Self {
        Self {
            include: PathGlobSet::empty(),
            exclude: PathGlobSet::empty(),
        }
    }
}

impl View {
    /// Compiles the patterns; a bad one is `invalid_request`.
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> ApiResult<Self> {
        let compile = |p: Vec<String>| {
            PathGlobSet::new(p).map_err(|e| ApiError::invalid_request(e.to_string()))
        };
        Ok(Self {
            include: compile(include)?,
            exclude: compile(exclude)?,
        })
    }

    /// Whether the view selects nothing out.
    pub fn is_everything(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether `path` is in the view.
    pub fn contains(&self, path: &DepotPath) -> bool {
        (self.include.is_empty() || self.include.matches(path)) && !self.exclude.matches(path)
    }
}

/// Read permission and view together: the paths a listing may show.
#[derive(Debug, Clone)]
pub struct Visibility {
    access: BranchAccess,
    view: View,
    everything: bool,
}

impl Visibility {
    /// Combines a branch's access with a view.
    pub fn new(access: BranchAccess, view: View) -> Self {
        let everything = access.reads_everything() && view.is_everything();
        Self {
            access,
            view,
            everything,
        }
    }

    /// Whether every path is visible, so per-path checks can be skipped.
    pub fn is_everything(&self) -> bool {
        self.everything
    }

    /// Whether `path` may be listed.
    pub fn shows(&self, path: &DepotPath) -> bool {
        self.everything || (self.view.contains(path) && self.access.allows(path, Level::Read))
    }
}

/// A revision the caller may read, with its branch and the access on that branch.
pub struct ReadableRev {
    /// The revision.
    pub rev: RevisionRow,
    /// Its branch.
    pub branch: BranchRow,
    /// The caller's access on that branch.
    pub access: BranchAccess,
}

/// Loads revision `n` of `depot`. A missing revision and one on a branch the caller cannot
/// read at all are both `not_found`.
pub async fn readable_rev(
    state: &AppState,
    caller: &Caller,
    depot: DepotId,
    n: u64,
) -> ApiResult<ReadableRev> {
    let found = state
        .meta()
        .read(move |tx| {
            let Some(r) = tx.revision(depot, n)? else {
                return Ok(None);
            };
            Ok(tx.branch_by_id(r.branch_id)?.map(|b| (r, b)))
        })
        .await?;
    let not_found = || ApiError::new(ErrorCode::NotFound, format!("no revision {n}"));
    let (rev, branch) = found.ok_or_else(not_found)?;
    let access = state
        .permissions()
        .depot(caller, depot)
        .branch(&branch.name);
    if !access.reads_anything() {
        return Err(not_found());
    }
    Ok(ReadableRev {
        rev,
        branch,
        access,
    })
}
