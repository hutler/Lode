//! Typed rows of the core tables.

use lode_core::{DepotId, ObjectId};

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub i64);
    };
}

id_type!(
    /// Row id of a principal.
    PrincipalId
);
id_type!(
    /// Row id of a group.
    GroupId
);
id_type!(
    /// Row id of a token.
    TokenId
);
id_type!(
    /// Row id of an access rule.
    AclId
);
id_type!(
    /// Row id of a branch.
    BranchId
);

/// Milliseconds since the Unix epoch, UTC. Callers pass the time in, so tests can use a
/// fixed clock.
pub type Millis = i64;

/// A depot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotRow {
    /// Id.
    pub id: DepotId,
    /// Unique name.
    pub name: String,
    /// The number the next revision will get.
    pub next_rev: u64,
    /// Creation time.
    pub created_at: Millis,
}

/// Human or agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    /// A person.
    User,
    /// A non-human principal owned by a user.
    Agent,
}

impl PrincipalKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::User => "user",
            PrincipalKind::Agent => "agent",
        }
    }
}

/// A principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRow {
    /// Id.
    pub id: PrincipalId,
    /// User or agent.
    pub kind: PrincipalKind,
    /// Unique login name.
    pub name: String,
    /// Optional display name.
    pub display_name: Option<String>,
    /// Owning user, for agents.
    pub owner_id: Option<PrincipalId>,
    /// Argon2id hash, for users with a password.
    pub password_hash: Option<String>,
    /// Disabled principals cannot authenticate.
    pub disabled: bool,
    /// Creation time.
    pub created_at: Millis,
    /// The single sign-on identity linked to this user, in lower case.
    pub sso_id: Option<String>,
}

/// Fields for a new principal.
#[derive(Debug, Clone)]
pub struct NewPrincipal {
    /// User or agent.
    pub kind: PrincipalKind,
    /// Unique login name.
    pub name: String,
    /// Optional display name.
    pub display_name: Option<String>,
    /// Owning user, required for agents by the server (not enforced here).
    pub owner_id: Option<PrincipalId>,
    /// Password hash.
    pub password_hash: Option<String>,
}

/// A group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    /// Id.
    pub id: GroupId,
    /// Unique name.
    pub name: String,
}

/// A bearer token. Only the SHA-256 of the token is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRow {
    /// Id.
    pub id: TokenId,
    /// Owner.
    pub principal_id: PrincipalId,
    /// Label chosen by the owner.
    pub name: String,
    /// SHA-256 of the token.
    pub hash: Vec<u8>,
    /// Scope names, such as `read`, `write`, `shelve`.
    pub scopes: Vec<String>,
    /// Expiry time, if any.
    pub expires_at: Option<Millis>,
    /// Last successful use.
    pub last_used_at: Option<Millis>,
    /// Creation time.
    pub created_at: Millis,
}

/// Who an access rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclSubject {
    /// Everyone.
    All,
    /// One principal.
    User(PrincipalId),
    /// Members of a group.
    Group(GroupId),
}

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclEffect {
    /// Grants the level.
    Allow,
    /// Refuses the level and every higher one.
    Deny,
}

/// Access level, ordered from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessLevel {
    /// Read.
    Read,
    /// Read and write.
    Write,
    /// Everything.
    Admin,
}

/// An access rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclRow {
    /// Id (ignored on insert).
    pub id: AclId,
    /// Depot, or every depot when `None`.
    pub depot_id: Option<DepotId>,
    /// Subject.
    pub subject: AclSubject,
    /// Branch name pattern.
    pub branch_glob: String,
    /// Path pattern.
    pub path_glob: String,
    /// Allow or deny.
    pub effect: AclEffect,
    /// Level.
    pub level: AccessLevel,
}

/// Branch state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchState {
    /// Accepts commits.
    Active,
    /// Hidden and read-only.
    Archived,
}

/// A branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchRow {
    /// Id.
    pub id: BranchId,
    /// Depot.
    pub depot_id: DepotId,
    /// Name, unique in the depot.
    pub name: String,
    /// Newest revision on the branch.
    pub head_rev: u64,
    /// The revision that created the branch.
    pub created_rev: u64,
    /// Branch it was created from.
    pub parent_branch_id: Option<BranchId>,
    /// Revision of the parent it was created from.
    pub parent_rev: Option<u64>,
    /// Active or archived.
    pub state: BranchState,
}

/// A revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRow {
    /// Depot.
    pub depot_id: DepotId,
    /// Revision number, unique in the depot.
    pub num: u64,
    /// Branch it belongs to.
    pub branch_id: BranchId,
    /// Previous revision of this branch, or the source revision for a branch creation.
    pub parent_num: Option<u64>,
    /// Root tree id.
    pub root_tree: ObjectId,
    /// Who made the change.
    pub author_id: PrincipalId,
    /// Who committed it.
    pub submitter_id: PrincipalId,
    /// Commit time.
    pub created_at: Millis,
    /// Message.
    pub message: String,
    /// Provenance JSON, stored as given.
    pub provenance: Option<String>,
    /// Client request id for idempotent retries.
    pub request_id: Option<String>,
}

/// One row of the per-path history index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    /// Path.
    pub path: String,
    /// `A`, `M` or `D`.
    pub action: char,
    /// Entry kind code (1 file, 3 symlink) for adds and modifies.
    pub kind: Option<u8>,
    /// Id before the change.
    pub old_id: Option<ObjectId>,
    /// Id after the change.
    pub new_id: Option<ObjectId>,
    /// Size after the change.
    pub size: Option<u64>,
    /// Entry flags after the change.
    pub flags: u8,
    /// Copy source path.
    pub copy_from_path: Option<String>,
    /// Copy source revision.
    pub copy_from_rev: Option<u64>,
}

/// A merge recorded on a revision: an inclusive range of source revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRow {
    /// Source branch.
    pub source_branch_id: BranchId,
    /// First merged source revision.
    pub from_rev: u64,
    /// Last merged source revision.
    pub to_rev: u64,
}

/// An outbox event waiting for subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    /// Monotonic id.
    pub id: i64,
    /// Depot, if the event concerns one.
    pub depot_id: Option<DepotId>,
    /// Event name, such as `revision.created`.
    pub kind: String,
    /// JSON payload with identifiers only.
    pub payload: String,
    /// Time written.
    pub created_at: Millis,
}

/// One stretch of a branch's inherited history: revisions of `branch_id` up to `max_rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistorySegment {
    /// Branch whose own revisions are included.
    pub branch_id: BranchId,
    /// Newest revision included, or every revision when `None`.
    pub max_rev: Option<u64>,
}

/// A change to a path together with its revision, from a path-history query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHistoryEntry {
    /// The revision.
    pub rev: u64,
    /// Its branch.
    pub branch_id: BranchId,
    /// The change to the path.
    pub change: ChangeRow,
}

/// Which revisions [`crate::Reads::find_revisions`] returns.
#[derive(Debug, Clone, Default)]
pub struct RevisionFilter {
    /// Only revisions in these history segments; the whole depot when `None`.
    pub segments: Option<Vec<HistorySegment>>,
    /// Only revisions that changed this path.
    pub path: Option<String>,
    /// With `path`: only revisions that added it as a copy (`copy_from` set).
    pub copies_only: bool,
    /// Lowest number included.
    pub min: Option<u64>,
    /// Highest number included.
    pub max: Option<u64>,
    /// Oldest first instead of newest first.
    pub ascending: bool,
    /// Most rows returned.
    pub limit: u32,
    /// Only revisions by this author.
    pub author: Option<PrincipalId>,
    /// Only revisions whose author is of this kind.
    pub author_kind: Option<PrincipalKind>,
}

/// Content obliterated from a depot (T120).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObliteratedRow {
    /// Depot.
    pub depot_id: DepotId,
    /// The blob id; the object is gone from the store.
    pub id: ObjectId,
    /// Its size in bytes.
    pub size: u64,
    /// The path it was selected by (it may have been used at other paths too).
    pub path: String,
    /// Why it was removed.
    pub reason: String,
    /// Who removed it.
    pub by: PrincipalId,
    /// When.
    pub at: Millis,
}

/// The last garbage collection or consistency check of a depot (T140).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceRunRow {
    /// Depot.
    pub depot_id: DepotId,
    /// `gc`, `fsck` or `fsck-full`.
    pub kind: String,
    /// When it finished.
    pub at: Millis,
    /// Objects examined.
    pub objects_checked: u64,
    /// Objects removed (gc) or found damaged or missing (fsck).
    pub objects_affected: u64,
    /// Bytes removed.
    pub bytes_affected: u64,
    /// Problems found.
    pub problems: u64,
}

/// A change together with its revision and that revision's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevChange {
    /// The revision.
    pub rev: u64,
    /// Its branch.
    pub branch_id: BranchId,
    /// The change.
    pub change: ChangeRow,
}
