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

/// Answer to a blob upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutBlobResponse {
    /// The id.
    pub id: ObjectId,
    /// False if the blob was already there.
    pub created: bool,
}
