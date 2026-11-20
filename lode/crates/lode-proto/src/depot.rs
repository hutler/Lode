//! Depots, branches and blob transfer helpers.

use lode_core::ObjectId;
use serde::{Deserialize, Serialize};

use crate::{rfc3339, Timestamp};

/// A depot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepotView {
    /// Name.
    pub name: String,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
}

/// `GET /depots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepotList {
    /// Depots the caller can read.
    pub depots: Vec<DepotView>,
}

/// `POST /depots` (admin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDepotRequest {
    /// Name.
    pub name: String,
}

/// Branch state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchState {
    /// Accepts commits.
    Active,
    /// Hidden and read-only.
    Archived,
}

/// Where a branch was created from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchPoint {
    /// Source branch.
    pub branch: String,
    /// Source revision.
    pub rev: u64,
}

/// A branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchView {
    /// Name.
    pub name: String,
    /// Newest revision.
    pub head_rev: u64,
    /// Revision that created the branch.
    pub created_rev: u64,
    /// Source, for branches created from another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<BranchPoint>,
    /// Active or archived.
    pub state: BranchState,
}

/// `GET /depots/{d}/branches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchList {
    /// Branches.
    pub branches: Vec<BranchView>,
}

/// `POST /depots/{d}/branches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBranchRequest {
    /// New branch name.
    pub name: String,
    /// Source branch.
    pub from_branch: String,
    /// Source revision; the source head when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_rev: Option<u64>,
}

/// `PATCH /depots/{d}/branches/{b}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchBranchRequest {
    /// New state; `archived` archives the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<BranchState>,
}

/// `POST /depots/{d}/objects/missing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingObjectsRequest {
    /// Ids to check; at most [`crate::consts::MAX_MISSING_QUERY`].
    pub ids: Vec<ObjectId>,
}

/// Answer: the subset not on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingObjectsResponse {
    /// Ids that must be uploaded.
    pub missing: Vec<ObjectId>,
}

/// `GET /depots/{d}/obliterated-ids?after=`: content obliterated from the depot, ids only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliteratedIds {
    /// Ids obliterated at or after `after` (every one without it).
    pub ids: Vec<ObjectId>,
    /// The server's time in milliseconds since the epoch; pass it, less a margin, as the
    /// next `after`.
    pub now_ms: i64,
}

/// Answer to a blob upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutBlobResponse {
    /// The id.
    pub id: ObjectId,
    /// False if the blob was already there.
    pub created: bool,
}

/// A merge already recorded into the target branch's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedRange {
    /// Revision that recorded it.
    pub rev: u64,
    /// First merged source revision.
    pub from_rev: u64,
    /// Last merged source revision.
    pub to_rev: u64,
}

/// `GET /depots/{d}/branches/{b}/merge-info?source=<branch>`: what merging `source` into the
/// branch would start from (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeInfo {
    /// The branch merged into.
    pub target: String,
    /// The branch merged from.
    pub source: String,
    /// Newest revision both branches share in their history.
    pub branch_point: u64,
    /// Merges from `source` recorded in the target's history, oldest first.
    pub merged: Vec<MergedRange>,
    /// The merge base: the highest source revision up to which every source revision after
    /// the branch point is merged, or the branch point.
    pub base: u64,
    /// Head of the source branch.
    pub source_head: u64,
    /// How many source revisions after the base a merge would bring.
    pub pending: u64,
}
