//! The commit request (ARCHITECTURE §5.1) and provenance (SPEC §7.3).

use std::collections::BTreeMap;

use lode_core::Change;
use serde::{Deserialize, Serialize};

/// A range of source revisions merged into this commit, inclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedFrom {
    /// Source branch.
    pub branch: String,
    /// First merged revision.
    pub from_rev: u64,
    /// Last merged revision.
    pub to_rev: u64,
}

/// Agent fields of a provenance record, such as `model`, `session` and `tool`: any string
/// fields, stored as given (SPEC §7.3). Self-reported. Anything but an object of strings is
/// refused.
pub type AgentProvenance = std::collections::BTreeMap<String, String>;

/// Which server endpoint a change came through. Set by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// The HTTP API, including the CLI and the local `lode mcp`.
    Api,
    /// The server MCP endpoint.
    Mcp,
}

/// Provenance record. Only `via` is set by the server; everything else is reported by the
/// client and must not be used for decisions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Provenance {
    /// Client name and version, from the `User-Agent` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Endpoint used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// Agent details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentProvenance>,
    /// The shelf this revision was submitted from. Set by the server on a shelf submit and
    /// removed from direct commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_shelf: Option<u64>,
}

/// `POST /depots/{d}/branches/{b}/commits`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitRequest {
    /// Client-chosen id; a retry with the same id returns the same revision.
    pub request_id: String,
    /// Message.
    pub message: String,
    /// File changes.
    pub changes: Vec<Change>,
    /// Merge origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<MergedFrom>,
    /// Self-reported provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Hints for modules, such as `locks.keep`. Never proof of anything.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Answer to a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResponse {
    /// New revision number.
    pub rev: u64,
    /// Branch committed to.
    pub branch: String,
    /// True if this answers a retry of an earlier request.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub replayed: bool,
}
