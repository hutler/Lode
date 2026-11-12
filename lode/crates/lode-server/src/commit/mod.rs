//! The commit engine (ARCHITECTURE §5): tree editing, the commit transaction, module hook
//! points and the commit endpoint.

mod editor;
mod engine;
mod handlers;
mod trees;

pub use editor::{EditError, EditOp, Edited, Expect, TreeEditor};
pub use engine::{commit, CommitOptions};
pub use handlers::routes;
pub use trees::{lookup, TreeLoadError, TreeLoader, TreeSource};

use std::collections::BTreeMap;

use lode_core::{Change, DepotId, DepotPath, LodeConfig, TreeEntry};
use lode_meta::{BranchRow, PrincipalRow};

/// How a commit reached the engine. Set only by server code, never from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A commit request from a client.
    Direct,
    /// A module committing on someone's behalf, such as a shelf submit
    /// (`name = "shelves"`, `attrs = { shelf_id, shelf_version }`).
    Module {
        /// The module's name.
        name: String,
        /// Module-defined attributes.
        attrs: BTreeMap<String, String>,
    },
}

impl Origin {
    /// `direct`, or the module name, as written into events.
    pub fn label(&self) -> &str {
        match self {
            Origin::Direct => "direct",
            Origin::Module { name, .. } => name,
        }
    }
}

/// What a module sees of a commit before it is written.
pub struct CommitCtx<'a> {
    /// Depot.
    pub depot: DepotId,
    /// Target branch, as of the transaction.
    pub branch: &'a BranchRow,
    /// Who made the change.
    pub author: &'a PrincipalRow,
    /// Who committed it (the caller).
    pub submitter: &'a PrincipalRow,
    /// The commit message.
    pub message: &'a str,
    /// The change list.
    pub changes: &'a [Change],
    /// Client-supplied extensions. Never proof of anything.
    pub extensions: &'a BTreeMap<String, serde_json::Value>,
    /// Parsed `.lodeconfig` of the branch head.
    pub branch_config: &'a LodeConfig,
    /// How the commit arrived.
    pub origin: &'a Origin,
    /// The head-tree entries of the changed paths (absent for adds).
    pub head_entries: &'a std::collections::HashMap<DepotPath, TreeEntry>,
}

impl CommitCtx<'_> {
    /// The head-tree entry of a changed path, or `None` if the path is new. Only changed
    /// paths can be looked up, so a hook never needs blob or tree I/O.
    pub fn head_entry(&self, path: &DepotPath) -> Option<&TreeEntry> {
        self.head_entries.get(path)
    }

    /// The module's own section of the branch configuration, if any.
    pub fn config_section(&self, name: &str) -> Option<&toml::Table> {
        self.branch_config.section(name)
    }
}

/// What a module sees of a commit as it is written.
pub struct CommittedCtx<'a> {
    /// The commit as checked.
    pub commit: &'a CommitCtx<'a>,
    /// The new revision number.
    pub rev: u64,
}
