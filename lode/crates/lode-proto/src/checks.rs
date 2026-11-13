//! Checks module (ARCHITECTURE §9.5).

use serde::{Deserialize, Serialize};

use crate::{rfc3339, PrincipalRef, Timestamp};

/// Longest check name.
pub const MAX_NAME_LEN: usize = 100;
/// Longest summary, in bytes.
pub const MAX_SUMMARY_LEN: usize = 1024;
/// Longest link, in bytes.
pub const MAX_URL_LEN: usize = 2048;

/// What a check is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckTargetKind {
    /// A revision; its version is 0.
    Revision,
    /// One version of a shelf.
    Shelf,
}

impl CheckTargetKind {
    /// The wire and database form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revision => "revision",
            Self::Shelf => "shelf",
        }
    }

    /// Parses the wire form.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "revision" => Some(Self::Revision),
            "shelf" => Some(Self::Shelf),
            _ => None,
        }
    }
}

/// A revision, or one version of a shelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckTarget {
    /// Revision or shelf.
    pub kind: CheckTargetKind,
    /// Revision number or shelf id.
    pub id: u64,
    /// Shelf version (1 or more); absent or 0 for a revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

/// Result of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    /// Running or queued.
    Pending,
    /// Passed.
    Success,
    /// Ran and found a problem.
    Failure,
    /// Could not run.
    Error,
    /// Stopped before it finished.
    Cancelled,
}

impl CheckState {
    /// The wire and database form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses the wire form.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// `POST /depots/{d}/checks`: creates or replaces the check `name` on `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCheckRequest {
    /// What the result belongs to.
    pub target: CheckTarget,
    /// `[a-z0-9._/-]{1,100}`, such as `ci/build`.
    pub name: String,
    /// Result.
    pub state: CheckState,
    /// Short text, at most 1 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Link to details, `http(s)`, at most 2 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A stored check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckView {
    /// What it belongs to.
    pub target: CheckTarget,
    /// Name.
    pub name: String,
    /// Result.
    pub state: CheckState,
    /// Short text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Link to details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Who reported it last.
    pub reporter: PrincipalRef,
    /// When it was last reported.
    #[serde(with = "rfc3339")]
    pub updated_at: Timestamp,
}

/// Query of `GET /depots/{d}/checks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksQuery {
    /// Revision or shelf.
    pub target_kind: CheckTargetKind,
    /// Revision number or shelf id.
    pub target_id: u64,
    /// Shelf version; required for a shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_version: Option<u64>,
}

/// `GET /depots/{d}/checks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckList {
    /// Checks, most recently reported first.
    pub checks: Vec<CheckView>,
}
