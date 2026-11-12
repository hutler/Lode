//! Administration: principals, groups, access rules, maintenance.

use serde::{Deserialize, Serialize};

use crate::auth::PrincipalView;
use crate::PrincipalKind;

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
}

/// `GET /admin/principals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalList {
    /// Principals.
    pub principals: Vec<PrincipalView>,
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
    /// One line per problem found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}
