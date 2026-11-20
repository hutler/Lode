//! Administration: principals, groups, access rules, maintenance.

use serde::{Deserialize, Serialize};

use crate::auth::PrincipalView;
use crate::PrincipalKind;
use lode_core::ObjectId;

/// `POST /admin/principals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePrincipalRequest {
    /// User or agent.
    pub kind: PrincipalKind,
    /// Login name.
    pub name: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Owning user; required for agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Initial password, for users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// The single sign-on identity to link, for users, such as their e-mail address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<String>,
}

/// `GET /admin/principals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalList {
    /// Principals.
    pub principals: Vec<PrincipalView>,
}

/// `PATCH /admin/principals/{name}`. Absent fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchPrincipalRequest {
    /// Disable or re-enable. Disabled principals cannot authenticate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// New password, for users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Link a single sign-on identity, for users; an empty string removes the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso: Option<String>,
}

/// `POST /admin/groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    /// Name.
    pub name: String,
}

/// A group with its members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupView {
    /// Name.
    pub name: String,
    /// Member names.
    #[serde(default)]
    pub members: Vec<String>,
}

/// `GET /admin/groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupList {
    /// Groups.
    pub groups: Vec<GroupView>,
}

/// Who an access rule applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "lowercase")]
pub enum AclSubject {
    /// Everyone.
    All,
    /// One principal, by name.
    User(String),
    /// A group, by name.
    Group(String),
}

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AclEffect {
    /// Grants the level.
    Allow,
    /// Refuses the level and every higher one.
    Deny,
}

/// Access level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessLevel {
    /// Read.
    Read,
    /// Read and write.
    Write,
    /// Everything.
    Admin,
}

/// An access rule. `id` is absent when creating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclRule {
    /// Id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Depot; every depot when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depot: Option<String>,
    /// Subject.
    pub subject: AclSubject,
    /// Branch pattern.
    pub branch: String,
    /// Path pattern.
    pub path: String,
    /// Allow or deny.
    pub effect: AclEffect,
    /// Level.
    pub level: AccessLevel,
}

/// `GET /admin/acl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclList {
    /// Rules in evaluation order.
    pub rules: Vec<AclRule>,
}

/// Answer of a garbage collection or consistency check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceReport {
    /// Objects examined.
    pub objects_checked: u64,
    /// Objects removed (gc) or found damaged or missing (fsck).
    pub objects_affected: u64,
    /// Bytes removed (gc), or that a dry run would remove.
    #[serde(default)]
    pub bytes_affected: u64,
    /// True when nothing was changed.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub dry_run: bool,
    /// One line per problem found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// `GET /admin/acl/check?principal=&depot=&branch=&path=`: what the rules give a principal,
/// as with a token that has every scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessCheck {
    /// Principal checked.
    pub principal: String,
    /// Depot.
    pub depot: String,
    /// Branch.
    pub branch: String,
    /// Path.
    pub path: lode_core::DepotPath,
    /// The level granted; `None` when the path is invisible.
    pub level: Option<AccessLevel>,
    /// The principal administers the depot, which passes over path rules.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub depot_admin: bool,
    /// The ids of the rules that apply to this path, in evaluation order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched: Vec<i64>,
    /// The rule that decided the level: the deny that capped it, or the allow that granted
    /// it (for a depot administrator, the rule that made them one). Absent when no rule
    /// grants anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<i64>,
}

/// One thing to obliterate: every version of `path` (or of every file under it) on every
/// branch, or only the version `rev` had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliterateTarget {
    /// A file or a folder.
    pub path: lode_core::DepotPath,
    /// Only the version at this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

/// `POST /admin/depots/{d}/obliterate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliterateRequest {
    /// What to remove.
    pub targets: Vec<ObliterateTarget>,
    /// Why; recorded with each object. Required unless `dry_run`.
    #[serde(default)]
    pub reason: String,
    /// Report what would be removed without removing it.
    #[serde(default)]
    pub dry_run: bool,
}

/// A revision that stored obliterated content, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentUse {
    /// The revision.
    pub rev: u64,
    /// Its branch.
    pub branch: String,
    /// The path it stored the content at.
    pub path: lode_core::DepotPath,
}

/// An active branch whose head still holds selected content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentInUse {
    /// The branch.
    pub branch: String,
    /// Where its head holds the content.
    pub path: lode_core::DepotPath,
    /// The content.
    pub id: ObjectId,
}

/// One object an obliterate removes (or would remove).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliteratedObject {
    /// Blob id.
    pub id: ObjectId,
    /// Size in bytes.
    pub size: u64,
    /// Every revision that stored it, at any path, oldest first.
    pub uses: Vec<ContentUse>,
}

/// Answer of an obliterate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliterateReport {
    /// True when nothing was changed.
    #[serde(default)]
    pub dry_run: bool,
    /// The objects removed, or that would be.
    pub objects: Vec<ObliteratedObject>,
    /// Their total size.
    pub bytes: u64,
    /// Active branch heads that still hold selected content. Only a dry run answers with
    /// any; otherwise they are a `conflict`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_use: Vec<ContentInUse>,
}

/// One record of obliterated content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Obliteration {
    /// Blob id.
    pub id: ObjectId,
    /// Size in bytes.
    pub size: u64,
    /// The path it was selected by.
    pub path: String,
    /// Why.
    pub reason: String,
    /// Who removed it.
    pub by: String,
    /// When.
    #[serde(with = "crate::rfc3339")]
    pub at: crate::Timestamp,
}

/// `GET /admin/depots/{d}/obliterated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliterationList {
    /// Oldest first.
    pub obliterated: Vec<Obliteration>,
}

/// `DELETE /admin/principals/{name}`: what was deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePrincipalResponse {
    /// The deleted user or agent.
    pub name: String,
    /// Agents deleted with their owner.
    #[serde(default)]
    pub agents: Vec<String>,
}

/// `DELETE /admin/depots/{d}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteDepotRequest {
    /// The depot's name again, typed by the person, as confirmation.
    pub confirm: String,
    /// Erase the depot now instead of moving it to the trash. Also erases a depot that is
    /// already in the trash.
    #[serde(default)]
    pub force: bool,
}

/// Answer to `DELETE /admin/depots/{d}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteDepotResponse {
    /// The depot.
    pub name: String,
    /// True when it was erased for good; false when it went to the trash.
    pub purged: bool,
    /// When a depot in the trash will be erased.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::rfc3339::option"
    )]
    pub purge_after: Option<crate::Timestamp>,
}

/// A depot in the trash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedDepot {
    /// Name; it stays taken until the depot is erased.
    pub name: String,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: crate::Timestamp,
    /// When it was deleted.
    #[serde(with = "crate::rfc3339")]
    pub deleted_at: crate::Timestamp,
    /// When it will be erased.
    #[serde(with = "crate::rfc3339")]
    pub purge_after: crate::Timestamp,
}

/// `GET /admin/trash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashList {
    /// Depots in the trash, oldest deletion first.
    pub depots: Vec<TrashedDepot>,
}
