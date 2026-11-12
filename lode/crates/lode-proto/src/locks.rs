//! Locks module (ARCHITECTURE §9.1).

use lode_core::{DepotPath, ObjectId};
use serde::{Deserialize, Serialize};

use crate::{rfc3339, PrincipalRef, Timestamp};

/// A path to lock and the content the locker has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockPath {
    /// Path.
    pub path: DepotPath,
    /// Id the locker has; must equal the branch head's.
    pub base_id: ObjectId,
}

/// `POST /depots/{d}/locks`. All or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRequest {
    /// Branch.
    pub branch: String,
    /// Workspace id of the locker.
    pub workspace: String,
    /// Comment shown to others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Paths.
    pub paths: Vec<LockPath>,
}

/// `DELETE /depots/{d}/locks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockRequest {
    /// Branch.
    pub branch: String,
    /// Paths.
    pub paths: Vec<DepotPath>,
    /// Break other principals' locks (admin).
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub force: bool,
}

/// Query of `GET /depots/{d}/locks`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LocksQuery {
    /// Branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Path prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Owner name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// A lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockView {
    /// Branch.
    pub branch: String,
    /// Path.
    pub path: DepotPath,
    /// Holder.
    pub owner: PrincipalRef,
    /// Holder's workspace.
    pub workspace: String,
    /// Comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Time taken.
    #[serde(with = "rfc3339")]
    pub created_at: Timestamp,
}

/// `GET /depots/{d}/locks` and answer to a lock request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockList {
    /// Locks.
    pub locks: Vec<LockView>,
}
