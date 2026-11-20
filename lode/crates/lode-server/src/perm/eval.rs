//! Evaluating access rules.
//!
//! Rules are loaded from the metadata store into an immutable snapshot that is swapped on
//! every change. A request narrows the snapshot to its caller and depot ([`DepotAccess`]),
//! then to one branch ([`BranchAccess`]), compiling the remaining path patterns into one
//! glob set so a whole list of paths is checked with one match per path.

use std::sync::{Arc, RwLock};

use lode_core::{DepotId, DepotPath, ErrorCode, PathGlobSet};
use lode_meta::{AccessLevel, AclEffect, AclId, AclRow, AclSubject, MetaStore, Reads};

use crate::auth::{Caller, SCOPE_ADMIN};
use crate::error::ApiError;

/// What a principal may do with a path. Ordered: each level includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// No access; the path is invisible.
    None,
    /// Read.
    Read,
    /// Read and write.
    Write,
    /// Everything.
    Admin,
}

impl Level {
    fn from_meta(l: AccessLevel) -> Self {
        match l {
            AccessLevel::Read => Level::Read,
            AccessLevel::Write => Level::Write,
            AccessLevel::Admin => Level::Admin,
        }
    }

    /// The level just below this one.
    fn below(self) -> Self {
        match self {
            Level::None | Level::Read => Level::None,
            Level::Write => Level::Read,
            Level::Admin => Level::Write,
        }
    }
}

#[derive(Debug, Clone)]
struct Rule {
    id: AclId,
    depot: Option<DepotId>,
    subject: AclSubject,
    branch: PathGlobSet,
    path: String,
    allow: bool,
    level: Level,
}

/// One consistent set of rules.
#[derive(Debug, Default)]
struct RuleSet {
    epoch: u64,
    rules: Vec<Rule>,
}

/// The rule cache. Cheap to read; reloaded after every change.
#[derive(Debug, Default)]
pub struct Permissions {
    current: RwLock<Arc<RuleSet>>,
}

fn compile(rows: Vec<AclRow>, epoch: u64) -> RuleSet {
    let rules = rows
        .into_iter()
        .filter_map(|r| {
            // Rules are validated on insert; one that no longer compiles is skipped (and so
            // grants nothing) rather than failing every request.
            let branch = match PathGlobSet::new([r.branch_glob.as_str()]) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(rule = r.id.0, error = %e, "skipping access rule");
                    return None;
                }
            };
            if let Err(e) = PathGlobSet::new([r.path_glob.as_str()]) {
                tracing::error!(rule = r.id.0, error = %e, "skipping access rule");
                return None;
            }
            Some(Rule {
                id: r.id,
                depot: r.depot_id,
                subject: r.subject,
                branch,
                path: r.path_glob,
                allow: r.effect == AclEffect::Allow,
                level: Level::from_meta(r.level),
            })
        })
        .collect();
    RuleSet { epoch, rules }
}

impl Permissions {
    /// Loads the rules.
    pub async fn load(meta: &MetaStore) -> lode_meta::Result<Self> {
        let p = Self::default();
        p.reload(meta).await?;
        Ok(p)
    }

    /// Re-reads the rules and the access epoch. Call after any change to rules or groups.
    pub async fn reload(&self, meta: &MetaStore) -> lode_meta::Result<()> {
        let (rows, epoch) = meta
            .read(|tx| Ok((tx.acl_all()?, tx.access_epoch()?)))
            .await?;
        let set = Arc::new(compile(rows, epoch));
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = set;
        Ok(())
    }

    fn snapshot(&self) -> Arc<RuleSet> {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The access epoch as of the last reload.
    pub fn access_epoch(&self) -> u64 {
        self.snapshot().epoch
    }

    /// Server administrators: the `admin` scope, plus either the built-in administrator or
    /// an `allow admin` rule that applies to every depot.
    pub fn is_server_admin(&self, caller: &Caller) -> bool {
        if !caller.has_scope(SCOPE_ADMIN) {
            return false;
        }
        caller.is_builtin_admin()
            || self.snapshot().rules.iter().any(|r| {
                r.depot.is_none()
                    && r.allow
                    && r.level == Level::Admin
                    && subject_matches(&r.subject, caller)
            })
    }

    /// Fails with `permission_denied` unless the caller is a server administrator.
    pub fn require_server_admin(&self, caller: &Caller) -> Result<(), ApiError> {
        if self.is_server_admin(caller) {
            Ok(())
        } else if !caller.has_scope(SCOPE_ADMIN) {
            caller.require_scope(SCOPE_ADMIN)
        } else {
            Err(ApiError::new(
                ErrorCode::PermissionDenied,
                "only a server administrator can do this",
            ))
        }
    }

    /// The rules that apply to `caller` in `depot`.
    pub fn depot(&self, caller: &Caller, depot: DepotId) -> DepotAccess {
        let set = self.snapshot();
        let rules: Vec<Rule> = set
            .rules
            .iter()
            .filter(|r| r.depot.is_none_or_eq(depot) && subject_matches(&r.subject, caller))
            .cloned()
            .collect();
        // Depot administrators bypass path rules for that depot (T012).
        let bypass = caller.has_scope(SCOPE_ADMIN)
            && (caller.is_builtin_admin()
                || rules.iter().any(|r| r.allow && r.level == Level::Admin));
        DepotAccess { bypass, rules }
    }
}

trait DepotMatch {
    fn is_none_or_eq(&self, d: DepotId) -> bool;
}

impl DepotMatch for Option<DepotId> {
    fn is_none_or_eq(&self, d: DepotId) -> bool {
        match self {
            None => true,
            Some(x) => *x == d,
        }
    }
}

fn subject_matches(subject: &AclSubject, caller: &Caller) -> bool {
    match subject {
        AclSubject::All => true,
        AclSubject::User(p) => *p == caller.principal.id,
        AclSubject::Group(g) => caller.groups.contains(g),
    }
}

/// The rules that apply to one caller in one depot.
#[derive(Debug, Clone)]
pub struct DepotAccess {
    bypass: bool,
    rules: Vec<Rule>,
}

impl DepotAccess {
    /// Whether the caller administers this depot and so bypasses path rules.
    pub fn is_depot_admin(&self) -> bool {
        self.bypass
    }

    /// Narrows to one branch, compiling the path patterns once.
    pub fn branch(&self, branch: &str) -> BranchAccess {
        if self.bypass {
            return BranchAccess {
                bypass: true,
                grants: Vec::new(),
                set: PathGlobSet::empty(),
            };
        }
        let applicable: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.branch.matches_str(branch))
            .collect();
        let set = PathGlobSet::new(applicable.iter().map(|r| r.path.as_str()))
            .unwrap_or_else(|_| PathGlobSet::empty());
        // `set` can only be empty here if a pattern failed to compile, which `compile`
        // already excluded; an empty set grants nothing, which is the safe outcome.
        let grants = if set.is_empty() {
            Vec::new()
        } else {
            applicable.iter().map(|r| (r.allow, r.level)).collect()
        };
        BranchAccess {
            bypass: false,
            grants,
            set,
        }
    }

    /// The level on one path of one branch.
    pub fn level(&self, branch: &str, path: &DepotPath) -> Level {
        self.branch(branch).level(path)
    }

    /// Whether the depot is visible to the caller at all: an administrator, or at least
    /// one allow rule applies (on any branch or path).
    pub fn is_visible(&self) -> bool {
        self.bypass || self.rules.iter().any(|r| r.allow)
    }

    /// The level on one path of one branch, and the rules behind it (T142: `acl check`).
    pub fn explain(&self, branch: &str, path: &DepotPath) -> Explained {
        if self.bypass {
            let rule = self
                .rules
                .iter()
                .find(|r| r.allow && r.level == Level::Admin)
                .map(|r| r.id.0);
            return Explained {
                level: Level::Admin,
                depot_admin: true,
                matched: Vec::new(),
                decided_by: rule,
            };
        }
        let mut matched = Vec::new();
        let (mut allow, mut allow_rule) = (Level::None, None);
        let (mut cap, mut cap_rule) = (Level::Admin, None);
        for r in self.rules.iter().filter(|r| r.branch.matches_str(branch)) {
            let hit =
                PathGlobSet::new([r.path.as_str()]).is_ok_and(|g| g.matches_str(path.as_str()));
            if !hit {
                continue;
            }
            matched.push(r.id.0);
            if r.allow {
                if r.level > allow {
                    allow = r.level;
                    allow_rule = Some(r.id.0);
                }
            } else if r.level.below() < cap {
                cap = r.level.below();
                cap_rule = Some(r.id.0);
            }
        }
        let decided_by = if allow == Level::None {
            None
        } else if cap < allow {
            cap_rule
        } else {
            allow_rule
        };
        Explained {
            level: allow.min(cap),
            depot_admin: false,
            matched,
            decided_by,
        }
    }

    /// The ids of the rules that apply, for diagnostics.
    pub fn rule_ids(&self) -> Vec<i64> {
        self.rules.iter().map(|r| r.id.0).collect()
    }
}

/// A level with the rules behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    /// The level.
    pub level: Level,
    /// An administrator of the depot, which passes over path rules.
    pub depot_admin: bool,
    /// Ids of the rules that apply to the path, in evaluation order.
    pub matched: Vec<i64>,
    /// The rule that decided the level.
    pub decided_by: Option<i64>,
}

/// The rules that apply to one caller on one branch, ready for bulk checks.
#[derive(Debug, Clone)]
pub struct BranchAccess {
    bypass: bool,
    grants: Vec<(bool, Level)>,
    set: PathGlobSet,
}

impl BranchAccess {
    /// The level on `path`. Default none; a matching deny at level L caps the result below
    /// L; otherwise the highest matching allow applies.
    pub fn level(&self, path: &DepotPath) -> Level {
        let mut buf = Vec::new();
        self.level_with(path.as_str(), &mut buf)
    }

    fn level_with(&self, path: &str, buf: &mut Vec<usize>) -> Level {
        if self.bypass {
            return Level::Admin;
        }
        if self.grants.is_empty() {
            return Level::None;
        }
        self.set.matches_into(path, buf);
        let mut allow = Level::None;
        let mut cap = Level::Admin;
        for &i in buf.iter() {
            let Some(&(is_allow, level)) = self.grants.get(i) else {
                continue;
            };
            if is_allow {
                allow = allow.max(level);
            } else {
                cap = cap.min(level.below());
            }
        }
        allow.min(cap)
    }

    /// Levels for many paths, reusing one match buffer.
    pub fn levels<'a, I>(&self, paths: I) -> Vec<Level>
    where
        I: IntoIterator<Item = &'a DepotPath>,
    {
        let mut buf = Vec::new();
        paths
            .into_iter()
            .map(|p| self.level_with(p.as_str(), &mut buf))
            .collect()
    }

    /// Whether `path` has at least `need`.
    pub fn allows(&self, path: &DepotPath, need: Level) -> bool {
        self.level(path) >= need
    }

    /// A predicate for listings: keeps the paths the caller can read. Holds its own match
    /// buffer, so use one per thread.
    pub fn read_filter(&self) -> impl FnMut(&DepotPath) -> bool + '_ {
        let mut buf = Vec::new();
        move |p: &DepotPath| self.level_with(p.as_str(), &mut buf) >= Level::Read
    }

    /// Whether any path may be readable: an administrator, or at least one allow rule of
    /// level read or above applies to the branch. A branch without one is hidden entirely.
    pub fn reads_anything(&self) -> bool {
        self.grants_somewhere(Level::Read)
    }

    /// Whether some allow rule of at least `level` applies to the branch, on any path.
    pub fn grants_somewhere(&self, level: Level) -> bool {
        self.bypass || self.grants.iter().any(|(allow, l)| *allow && *l >= level)
    }

    /// Whether every path is readable without checking (administrators, or no deny rule
    /// and an allow-read rule on `**`). Lets listings skip per-path checks.
    pub fn reads_everything(&self) -> bool {
        if self.bypass {
            return true;
        }
        let pats = self.set.patterns();
        let any_deny = self.grants.iter().any(|(allow, _)| !allow);
        !any_deny
            && self
                .grants
                .iter()
                .zip(pats)
                .any(|((allow, level), p)| *allow && *level >= Level::Read && p == "**")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lode_meta::{GroupId, PrincipalId, PrincipalKind, PrincipalRow, TokenId};

    const MAIN: &str = "main";
    const D1: DepotId = DepotId(1);
    const D2: DepotId = DepotId(2);
    const ANA: PrincipalId = PrincipalId(10);
    const BOB: PrincipalId = PrincipalId(11);
    const BOT: PrincipalId = PrincipalId(12);
    const ARTISTS: GroupId = GroupId(100);

    fn caller(id: PrincipalId, groups: &[GroupId], scopes: &[&str]) -> Caller {
        let (name, kind, owner) = match id {
            ANA => ("ana", PrincipalKind::User, None),
            BOB => ("bob", PrincipalKind::User, None),
            BOT => ("bot", PrincipalKind::Agent, Some(ANA)),
            _ => ("admin", PrincipalKind::User, None),
        };
        Caller {
            principal: PrincipalRow {
                id,
                kind,
                name: name.into(),
                display_name: None,
                owner_id: owner,
                password_hash: None,
                disabled: false,
                created_at: 0,
                sso_id: None,
                deleted_at: None,
            },
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            token_id: TokenId(1),
            groups: groups.to_vec(),
            via: lode_proto::commit::Via::Api,
        }
    }

    fn ana() -> Caller {
        caller(ANA, &[], &["read", "write"])
    }

    fn rule(
        depot: Option<DepotId>,
        subject: AclSubject,
        branch: &str,
        path: &str,
        effect: AclEffect,
        level: AccessLevel,
    ) -> AclRow {
        AclRow {
            id: AclId(0),
            depot_id: depot,
            subject,
            branch_glob: branch.into(),
            path_glob: path.into(),
            effect,
            level,
        }
    }

    use AccessLevel::{Admin as A, Read as R, Write as W};
    use AclEffect::{Allow, Deny};
    use AclSubject::{All, Group, User};

    fn perms(rows: Vec<AclRow>) -> Permissions {
        let p = Permissions::default();
        *p.current.write().unwrap() = Arc::new(compile(rows, 1));
        p
    }

    fn p(s: &str) -> DepotPath {
        DepotPath::parse(s).unwrap()
    }

    struct Case {
        name: &'static str,
        rules: Vec<AclRow>,
        caller: Caller,
        depot: DepotId,
        branch: &'static str,
        path: &'static str,
        want: Level,
    }

    #[test]
    fn spec_rules_table() {
        let all_read = rule(None, All, "*", "**", Allow, R);
        let cases = vec![
            Case {
                name: "default deny",
                rules: vec![],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "a.txt",
                want: Level::None,
            },
            Case {
                name: "allow all",
                rules: vec![all_read.clone()],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "a/b.txt",
                want: Level::Read,
            },
            Case {
                name: "allow user",
                rules: vec![rule(Some(D1), User(ANA), "*", "**", Allow, W)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "x",
                want: Level::Write,
            },
            Case {
                name: "user rule does not apply to others",
                rules: vec![rule(Some(D1), User(ANA), "*", "**", Allow, W)],
                caller: caller(BOB, &[], &["read"]),
                depot: D1,
                branch: MAIN,
                path: "x",
                want: Level::None,
            },
            Case {
                name: "allow group",
                rules: vec![rule(Some(D1), Group(ARTISTS), "*", "Art/**", Allow, W)],
                caller: caller(BOB, &[ARTISTS], &["read"]),
                depot: D1,
                branch: MAIN,
                path: "Art/hero.psd",
                want: Level::Write,
            },
            Case {
                name: "group rule needs membership",
                rules: vec![rule(Some(D1), Group(ARTISTS), "*", "Art/**", Allow, W)],
                caller: caller(BOB, &[], &["read"]),
                depot: D1,
                branch: MAIN,
                path: "Art/hero.psd",
                want: Level::None,
            },
            Case {
                name: "highest allow wins",
                rules: vec![
                    all_read.clone(),
                    rule(None, User(ANA), "*", "src/**", Allow, W),
                ],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "src/main.rs",
                want: Level::Write,
            },
            Case {
                name: "deny read overrides allow admin",
                rules: vec![
                    rule(None, User(ANA), "*", "**", Allow, A),
                    rule(None, All, "*", "secret/**", Deny, R),
                ],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "secret/key.txt",
                want: Level::None,
            },
            Case {
                name: "deny write still permits read",
                rules: vec![
                    rule(None, User(ANA), "*", "**", Allow, A),
                    rule(None, User(ANA), "*", "**", Deny, W),
                ],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::Read,
            },
            Case {
                name: "deny admin leaves write",
                rules: vec![
                    rule(None, User(ANA), "*", "**", Allow, A),
                    rule(None, All, "*", "**", Deny, A),
                ],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::Write,
            },
            Case {
                name: "deny on another path does not apply",
                rules: vec![all_read.clone(), rule(None, All, "*", "secret/**", Deny, R)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "public/a",
                want: Level::Read,
            },
            Case {
                name: "branch glob matches",
                rules: vec![rule(None, All, "release/*", "**", Allow, W)],
                caller: ana(),
                depot: D1,
                branch: "release/1.0",
                path: "a",
                want: Level::Write,
            },
            Case {
                name: "branch glob star stops at slash",
                rules: vec![rule(None, All, "release/*", "**", Allow, W)],
                caller: ana(),
                depot: D1,
                branch: "release/1.0/hotfix",
                path: "a",
                want: Level::None,
            },
            Case {
                name: "branch glob other branch",
                rules: vec![rule(None, All, "release/*", "**", Allow, W)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::None,
            },
            Case {
                name: "path glob single star stays in directory",
                rules: vec![rule(None, All, "*", "docs/*.md", Allow, R)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "docs/sub/a.md",
                want: Level::None,
            },
            Case {
                name: "path glob extension anywhere",
                rules: vec![rule(None, All, "*", "**/*.md", Allow, R)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "docs/sub/a.md",
                want: Level::Read,
            },
            Case {
                name: "depot-wide rule applies to every depot",
                rules: vec![rule(None, User(ANA), "*", "**", Allow, R)],
                caller: ana(),
                depot: D2,
                branch: MAIN,
                path: "a",
                want: Level::Read,
            },
            Case {
                name: "depot rule applies only to its depot",
                rules: vec![rule(Some(D1), User(ANA), "*", "**", Allow, R)],
                caller: ana(),
                depot: D2,
                branch: MAIN,
                path: "a",
                want: Level::None,
            },
            Case {
                name: "agent does not inherit owner access",
                rules: vec![rule(None, User(ANA), "*", "**", Allow, W)],
                caller: caller(BOT, &[], &["read", "write"]),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::None,
            },
            Case {
                name: "agent gets its own grant",
                rules: vec![rule(None, User(BOT), "*", "**", Allow, R)],
                caller: caller(BOT, &[], &["read"]),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::Read,
            },
            Case {
                name: "built-in admin with admin scope bypasses denies",
                rules: vec![rule(None, All, "*", "**", Deny, R)],
                caller: caller(PrincipalId(1), &[], &["read", "admin"]),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::Admin,
            },
            Case {
                name: "built-in admin without admin scope is evaluated normally",
                rules: vec![rule(None, All, "*", "**", Allow, R)],
                caller: caller(PrincipalId(1), &[], &["read"]),
                depot: D1,
                branch: MAIN,
                path: "a",
                want: Level::Read,
            },
            Case {
                name: "depot admin rule plus admin scope bypasses path rules",
                rules: vec![
                    rule(Some(D1), User(ANA), "*", "Config/**", Allow, A),
                    rule(None, All, "*", "secret/**", Deny, R),
                ],
                caller: caller(ANA, &[], &["read", "admin"]),
                depot: D1,
                branch: MAIN,
                path: "secret/a",
                want: Level::Admin,
            },
            Case {
                name: "depot admin rule without admin scope is a plain rule",
                rules: vec![rule(Some(D1), User(ANA), "*", "Config/**", Allow, A)],
                caller: ana(),
                depot: D1,
                branch: MAIN,
                path: "Other/a",
                want: Level::None,
            },
        ];
        for c in cases {
            let got = perms(c.rules)
                .depot(&c.caller, c.depot)
                .level(c.branch, &p(c.path));
            assert_eq!(got, c.want, "{}", c.name);
        }
    }

    #[test]
    fn server_admin_standing() {
        let admin = caller(PrincipalId(1), &[], &["admin"]);
        let none = perms(vec![]);
        assert!(none.is_server_admin(&admin));
        assert!(!none.is_server_admin(&caller(PrincipalId(1), &[], &["read"])));
        let anas = caller(ANA, &[], &["admin"]);
        assert!(!none.is_server_admin(&anas));
        let global = perms(vec![rule(None, User(ANA), "*", "**", Allow, A)]);
        assert!(global.is_server_admin(&anas));
        let depot_only = perms(vec![rule(Some(D1), User(ANA), "*", "**", Allow, A)]);
        assert!(!depot_only.is_server_admin(&anas));
        assert!(depot_only.depot(&anas, D1).is_depot_admin());
        assert!(!depot_only.depot(&anas, D2).is_depot_admin());
    }

    #[test]
    fn filter_and_reads_everything() {
        let p1 = perms(vec![
            rule(None, All, "*", "**", Allow, R),
            rule(None, All, "*", "secret/**", Deny, R),
        ]);
        let b = p1.depot(&ana(), D1).branch(MAIN);
        assert!(!b.reads_everything());
        let paths = [p("a"), p("secret/x"), p("b/c")];
        let mut keep = b.read_filter();
        let kept: Vec<_> = paths.iter().filter(|x| keep(x)).collect();
        assert_eq!(kept, [&paths[0], &paths[2]]);
        let open = perms(vec![rule(None, All, "*", "**", Allow, R)]);
        assert!(open.depot(&ana(), D1).branch(MAIN).reads_everything());
    }

    #[test]
    fn bulk_check_of_100k_paths_against_50_rules() {
        let mut rows = Vec::new();
        for i in 0..25 {
            rows.push(rule(None, All, "*", &format!("dir{i}/**"), Allow, R));
            rows.push(rule(
                None,
                All,
                "*",
                &format!("dir{i}/**/*.secret{i}"),
                Deny,
                R,
            ));
        }
        let perms = perms(rows);
        let paths: Vec<DepotPath> = (0..100_000)
            .map(|i| p(&format!("dir{}/sub{}/file{i}.txt", i % 30, i % 7)))
            .collect();
        let start = std::time::Instant::now();
        let branch = perms.depot(&ana(), D1).branch(MAIN);
        let levels = branch.levels(&paths);
        let elapsed = start.elapsed();
        assert_eq!(levels.len(), 100_000);
        assert_eq!(levels[0], Level::Read);
        assert_eq!(levels[29], Level::None);
        eprintln!("bulk check: {elapsed:?}");
        // "Well under a second" with a generous bound for slow CI machines.
        assert!(elapsed.as_secs_f64() < 1.0, "took {elapsed:?}");
    }
}
