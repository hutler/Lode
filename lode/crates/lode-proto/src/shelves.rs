//! Shelves module (ARCHITECTURE §9.2).

use lode_core::{Change, DepotPath};
use serde::{Deserialize, Serialize};

use crate::commit::{MergedFrom, Provenance};
use crate::{rfc3339, PrincipalRef, Timestamp};

/// Shelf state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShelfState {
    /// Can be updated or submitted.
    Open,
    /// Committed.
    Submitted,
    /// Removed.
    Deleted,
}

/// `POST /depots/{d}/shelves`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateShelfRequest {
    /// Target branch.
    pub branch: String,
    /// Revision the changes are based on.
    pub base_rev: u64,
    /// Description.
    pub description: String,
    /// Changes, exactly as a commit would send them.
    pub changes: Vec<Change>,
    /// Merge origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<MergedFrom>,
    /// Self-reported provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// `PUT /depots/{d}/shelves/{id}`: replaces everything and raises `version` by one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateShelfRequest {
    /// Revision the changes are based on.
    pub base_rev: u64,
    /// Description.
    pub description: String,
    /// Changes.
    pub changes: Vec<Change>,
    /// Merge origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<MergedFrom>,
    /// Self-reported provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Query of `GET /depots/{d}/shelves`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShelvesQuery {
    /// Owner name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// State.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ShelfState>,
}

/// A shelf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfView {
    /// Id.
    pub id: u64,
    /// Identifies one exact state; raised by every update.
    pub version: u64,
    /// Target branch.
    pub branch: String,
    /// Base revision.
    pub base_rev: u64,
    /// Owner, with kind, so clients can tell agent work apart.
    pub owner: PrincipalRef,
    /// The user who owns the agent, when the owner is an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_owner: Option<String>,
    /// Description.
    pub description: String,
    /// State.
    pub state: ShelfState,
    /// Revision it became, once submitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_rev: Option<u64>,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
    /// Last update.
    #[serde(with = "rfc3339")]
    pub updated_at: Timestamp,
    /// Changes; present on `GET …/{id}`, absent in lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<Change>>,
    /// Merge origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<MergedFrom>,
    /// Provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Per-file diffs against the base; present on `GET …/{id}?diff=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<Vec<ShelfFileDiff>>,
}

/// The diff of one changed file of a shelf against its base revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfFileDiff {
    /// Path.
    pub path: DepotPath,
    /// Unified diff (`--- a/path`, `+++ b/path`), for text files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Why there is no patch: `binary`, `symlink` or `too_large`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// `GET /depots/{d}/shelves`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfList {
    /// Shelves.
    pub shelves: Vec<ShelfView>,
}

/// `POST /depots/{d}/shelves/{id}/submit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitShelfRequest {
    /// The version that was reviewed; refused with `conflict` if the shelf changed since.
    pub expected_version: u64,
    /// Commit message; the shelf description when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
