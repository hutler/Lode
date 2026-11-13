//! Revisions, manifests, trees and files.

use lode_core::{ChangeAction, CopyFrom, DepotPath, EntryKind, ObjectId};
use serde::{Deserialize, Serialize};

use crate::commit::{MergedFrom, Provenance};
use crate::{rfc3339, PrincipalRef, Timestamp};

/// One changed path in a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeView {
    /// Path.
    pub path: DepotPath,
    /// Add, modify or delete.
    pub action: ChangeAction,
    /// File or symlink, for adds and modifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
    /// Id before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_id: Option<ObjectId>,
    /// Id after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_id: Option<ObjectId>,
    /// Size after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Executable after.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub executable: bool,
    /// Copy or rename origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_from: Option<CopyFrom>,
}

/// A revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionView {
    /// Number.
    pub rev: u64,
    /// Branch.
    pub branch: String,
    /// Previous revision of the branch, or the source of a branch creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// Who made the change.
    pub author: PrincipalRef,
    /// Who committed it.
    pub submitter: PrincipalRef,
    /// Commit time.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
    /// Message.
    pub message: String,
    /// Root tree id.
    pub root_tree: ObjectId,
    /// Provenance; see [`Provenance`] for what can be trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Merge origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<MergedFrom>,
    /// Changed paths, when asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<ChangeView>>,
    /// True when `changes` was capped.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub changes_truncated: bool,
}

/// Query of `GET /depots/{d}/revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevisionListQuery {
    /// Limit to one branch (with inherited history unless `own`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Limit to revisions that changed this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<DepotPath>,
    /// Newest first, below this number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    /// Oldest first, above this number (polling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// Only the branch's own revisions.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub own: bool,
    /// With `path`: continue through copies and renames (`copy_from`) to the source's history.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub follow: bool,
    /// Include changed paths.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub changes: bool,
    /// Only revisions by this principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Only revisions whose author is a user, or an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_kind: Option<crate::PrincipalKind>,
    /// Page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Answer of `GET /depots/{d}/revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionList {
    /// Revisions in the requested order.
    pub revisions: Vec<RevisionView>,
    /// True if more revisions follow in the same direction.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub more: bool,
}

/// Query of `GET /depots/{d}/manifest`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ManifestQuery {
    /// Revision.
    pub rev: u64,
    /// Return only differences from this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<u64>,
    /// View include patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// View exclude patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// One file in a full manifest: one NDJSON line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path.
    pub path: DepotPath,
    /// File or symlink.
    pub kind: EntryKind,
    /// Blob id.
    pub id: ObjectId,
    /// Size.
    pub size: u64,
    /// Executable bit.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub executable: bool,
}

/// One line of a delta manifest (`base` given).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ManifestDelta {
    /// The path is added or changed.
    Put(ManifestEntry),
    /// The path is removed.
    Del {
        /// Path.
        path: DepotPath,
    },
}

/// Query of `GET /depots/{d}/tree` and `/files`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PathAtRevQuery {
    /// Revision.
    pub rev: u64,
    /// Path; the root when absent (tree only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<DepotPath>,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntryView {
    /// Name.
    pub name: String,
    /// Kind.
    pub kind: EntryKind,
    /// Blob or tree id.
    pub id: ObjectId,
    /// Size (zero for directories).
    pub size: u64,
    /// Executable bit.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub executable: bool,
}

/// Answer of `GET /depots/{d}/tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeListing {
    /// Revision.
    pub rev: u64,
    /// Directory; absent for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<DepotPath>,
    /// Entries visible to the caller.
    pub entries: Vec<TreeEntryView>,
}

/// One line of a blame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameLine {
    /// The revision that last changed the line (with `truncated`: at the latest).
    pub rev: u64,
    /// The line, without its line ending.
    pub text: String,
}

/// A revision named by a blame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameRevision {
    /// Number.
    pub rev: u64,
    /// Its branch.
    pub branch: String,
    /// Who made the change.
    pub author: PrincipalRef,
    /// Commit time.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
    /// First line of the message.
    pub summary: String,
}

/// `GET /depots/{d}/blame?rev=&path=`: the revision that last changed each line of a text
/// file, following the file's history through copies and renames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameView {
    /// The file.
    pub path: DepotPath,
    /// The revision asked for.
    pub rev: u64,
    /// Content id of the file at `rev`.
    pub id: ObjectId,
    /// Every line, in order.
    pub lines: Vec<BlameLine>,
    /// The revisions the lines name, newest first.
    pub revisions: Vec<BlameRevision>,
    /// True when the history walk stopped early (depth cap, a version too large or not
    /// text, or history the caller cannot read): lines from the oldest version looked at are
    /// attributed to it, though they may be older.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub truncated: bool,
}
