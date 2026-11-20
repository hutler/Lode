//! Query methods. Reads are on the [`Reads`] trait, which both transaction types implement;
//! writes are inherent methods of [`WriteTx`].

use crate::sql::{Row, Sql, Val};
use crate::vals;
use lode_core::{DepotId, ObjectId};

use crate::error::unique;
use crate::rows::*;
use crate::{MetaError, ReadTx, Result, WriteTx};

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| MetaError::BadValue(format!("{v} does not fit in an i64")))
}

fn bad(i: usize, msg: &str) -> MetaError {
    MetaError::BadValue(format!("column {i}: {msg}"))
}

const DEPOT_COLS: &str = "id, name, next_rev, created_at, deleted_at";
fn depot_row(r: &Row) -> Result<DepotRow> {
    Ok(DepotRow {
        id: DepotId(r.get(0)?),
        name: r.get(1)?,
        next_rev: r.get::<u64>(2)?,
        created_at: r.get(3)?,
        deleted_at: r.get(4)?,
    })
}

const PRINCIPAL_COLS: &str =
    "id, kind, name, display_name, owner_id, password_hash, disabled, created_at, sso_id, deleted_at, \
     password_change_by";
fn principal_row(r: &Row) -> Result<PrincipalRow> {
    let kind: String = r.get(1)?;
    Ok(PrincipalRow {
        id: PrincipalId(r.get(0)?),
        kind: match kind.as_str() {
            "user" => PrincipalKind::User,
            "agent" => PrincipalKind::Agent,
            _ => return Err(bad(1, "unknown principal kind")),
        },
        name: r.get(2)?,
        display_name: r.get(3)?,
        owner_id: r.get::<Option<i64>>(4)?.map(PrincipalId),
        password_hash: r.get(5)?,
        disabled: r.get::<i64>(6)? != 0,
        created_at: r.get(7)?,
        sso_id: r.get(8)?,
        deleted_at: r.get(9)?,
        password_change_by: r.get(10)?,
    })
}

const TOKEN_COLS: &str =
    "id, principal_id, name, hash, scopes, expires_at, last_used_at, created_at";
fn token_row(r: &Row) -> Result<TokenRow> {
    let scopes: String = r.get(4)?;
    Ok(TokenRow {
        id: TokenId(r.get(0)?),
        principal_id: PrincipalId(r.get(1)?),
        name: r.get(2)?,
        hash: r.get(3)?,
        scopes: scopes.split_whitespace().map(str::to_owned).collect(),
        expires_at: r.get(5)?,
        last_used_at: r.get(6)?,
        created_at: r.get(7)?,
    })
}

const ACL_COLS: &str =
    "id, depot_id, subject_kind, subject_id, branch_glob, path_glob, effect, level";
fn acl_row(r: &Row) -> Result<AclRow> {
    let kind: String = r.get(2)?;
    let sid: Option<i64> = r.get(3)?;
    let subject = match (kind.as_str(), sid) {
        ("all", _) => AclSubject::All,
        ("user", Some(id)) => AclSubject::User(PrincipalId(id)),
        ("group", Some(id)) => AclSubject::Group(GroupId(id)),
        _ => return Err(bad(2, "bad acl subject")),
    };
    let effect: String = r.get(6)?;
    let level: String = r.get(7)?;
    Ok(AclRow {
        id: AclId(r.get(0)?),
        depot_id: r.get::<Option<i64>>(1)?.map(DepotId),
        subject,
        branch_glob: r.get(4)?,
        path_glob: r.get(5)?,
        effect: match effect.as_str() {
            "allow" => AclEffect::Allow,
            "deny" => AclEffect::Deny,
            _ => return Err(bad(6, "bad acl effect")),
        },
        level: match level.as_str() {
            "read" => AccessLevel::Read,
            "write" => AccessLevel::Write,
            "admin" => AccessLevel::Admin,
            _ => return Err(bad(7, "bad acl level")),
        },
    })
}

const BRANCH_COLS: &str =
    "id, depot_id, name, head_rev, created_rev, parent_branch_id, parent_rev, state, stream_type";
fn branch_row(r: &Row) -> Result<BranchRow> {
    let state: String = r.get(7)?;
    Ok(BranchRow {
        id: BranchId(r.get(0)?),
        depot_id: DepotId(r.get(1)?),
        name: r.get(2)?,
        head_rev: r.get::<u64>(3)?,
        created_rev: r.get::<u64>(4)?,
        parent_branch_id: r.get::<Option<i64>>(5)?.map(BranchId),
        parent_rev: r.get::<Option<u64>>(6)?,
        state: match state.as_str() {
            "active" => BranchState::Active,
            "archived" => BranchState::Archived,
            _ => return Err(bad(7, "bad branch state")),
        },
        stream_type: match r.get::<Option<String>>(8)? {
            None => None,
            Some(t) => Some(StreamType::parse(&t).ok_or_else(|| bad(8, "bad stream type"))?),
        },
    })
}

const REVISION_COLS: &str = "depot_id, num, branch_id, parent_num, root_tree, author_id, \
     submitter_id, created_at, message, provenance, request_id";
fn revision_row(r: &Row) -> Result<RevisionRow> {
    Ok(RevisionRow {
        depot_id: DepotId(r.get(0)?),
        num: r.get::<u64>(1)?,
        branch_id: BranchId(r.get(2)?),
        parent_num: r.get::<Option<u64>>(3)?,
        root_tree: r.get::<ObjectId>(4)?,
        author_id: PrincipalId(r.get(5)?),
        submitter_id: PrincipalId(r.get(6)?),
        created_at: r.get(7)?,
        message: r.get(8)?,
        provenance: r.get(9)?,
        request_id: r.get(10)?,
    })
}

const CHANGE_COLS: &str =
    "path, action, kind, old_id, new_id, size, flags, copy_from_path, copy_from_rev";
fn change_row_at(r: &Row, o: usize) -> Result<ChangeRow> {
    let action: String = r.get(o + 1)?;
    let action = match action.as_str() {
        "A" => 'A',
        "M" => 'M',
        "D" => 'D',
        _ => return Err(bad(o + 1, "bad change action")),
    };
    Ok(ChangeRow {
        path: r.get(o)?,
        action,
        kind: r.get::<Option<i64>>(o + 2)?.map(|k| k as u8),
        old_id: r.get::<Option<ObjectId>>(o + 3)?,
        new_id: r.get::<Option<ObjectId>>(o + 4)?,
        size: r.get::<Option<u64>>(o + 5)?,
        flags: r.get::<i64>(o + 6)? as u8,
        copy_from_path: r.get(o + 7)?,
        copy_from_rev: r.get::<Option<u64>>(o + 8)?,
    })
}

fn outbox_row(r: &Row) -> Result<OutboxRow> {
    Ok(OutboxRow {
        id: r.get(0)?,
        depot_id: r.get::<Option<i64>>(1)?.map(DepotId),
        kind: r.get(2)?,
        payload: r.get(3)?,
        created_at: r.get(4)?,
    })
}

/// Whether revision `num` of branch `branch` is in the history `chain`.
pub fn in_chain(chain: &[HistorySegment], branch: BranchId, num: u64) -> bool {
    chain
        .iter()
        .any(|seg| seg.branch_id == branch && seg.max_rev.is_none_or(|m| num <= m))
}

/// Read queries, available in both read and write transactions.
pub trait Reads {
    /// The connection the queries run on.
    fn db(&self) -> &dyn Sql;

    /// A live depot by name; depots in the trash are left out.
    fn depot_by_name(&self, name: &str) -> Result<Option<DepotRow>> {
        self.db().query_opt(
            &format!("SELECT {DEPOT_COLS} FROM depot WHERE name = ?1 AND deleted_at IS NULL"),
            vals![name],
            depot_row,
        )
    }

    /// A depot in the trash, by name. Purged depots have lost their name and are not found.
    fn trashed_depot(&self, name: &str) -> Result<Option<DepotRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {DEPOT_COLS} FROM depot \
                 WHERE name = ?1 AND deleted_at IS NOT NULL AND purged_at IS NULL"
            ),
            vals![name],
            depot_row,
        )
    }

    /// A depot by id, live, in the trash or purged.
    fn depot(&self, id: DepotId) -> Result<Option<DepotRow>> {
        self.db().query_opt(
            &format!("SELECT {DEPOT_COLS} FROM depot WHERE id = ?1"),
            vals![id.0],
            depot_row,
        )
    }

    /// Every live depot, by name.
    fn depots(&self) -> Result<Vec<DepotRow>> {
        self.db().query(
            &format!("SELECT {DEPOT_COLS} FROM depot WHERE deleted_at IS NULL ORDER BY name"),
            vals![],
            depot_row,
        )
    }

    /// Depots in the trash and not yet purged, oldest deletion first.
    fn trashed_depots(&self) -> Result<Vec<DepotRow>> {
        self.db().query(
            &format!(
                "SELECT {DEPOT_COLS} FROM depot WHERE deleted_at IS NOT NULL AND purged_at IS NULL \
                 ORDER BY deleted_at, name"
            ),
            vals![],
            depot_row,
        )
    }

    /// A principal by id.
    fn principal(&self, id: PrincipalId) -> Result<Option<PrincipalRow>> {
        self.db().query_opt(
            &format!("SELECT {PRINCIPAL_COLS} FROM principal WHERE id = ?1"),
            vals![id.0],
            principal_row,
        )
    }

    /// A principal by name; deleted principals are left out.
    fn principal_by_name(&self, name: &str) -> Result<Option<PrincipalRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {PRINCIPAL_COLS} FROM principal WHERE name = ?1 AND deleted_at IS NULL"
            ),
            vals![name],
            principal_row,
        )
    }

    /// A deleted principal by name. Its name stays taken, so it can be told apart from a
    /// free name.
    fn deleted_principal(&self, name: &str) -> Result<Option<PrincipalRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {PRINCIPAL_COLS} FROM principal \
                 WHERE name = ?1 AND deleted_at IS NOT NULL"
            ),
            vals![name],
            principal_row,
        )
    }

    /// The principal linked to a single sign-on identity (compared in lower case).
    fn principal_by_sso(&self, sso_id: &str) -> Result<Option<PrincipalRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {PRINCIPAL_COLS} FROM principal \
                 WHERE sso_id = ?1 AND deleted_at IS NULL"
            ),
            vals![sso_id.to_lowercase()],
            principal_row,
        )
    }

    /// Every principal that is not deleted, by name.
    fn principals(&self) -> Result<Vec<PrincipalRow>> {
        self.db().query(
            &format!(
                "SELECT {PRINCIPAL_COLS} FROM principal WHERE deleted_at IS NULL ORDER BY name"
            ),
            vals![],
            principal_row,
        )
    }

    /// A group by name.
    fn group_by_name(&self, name: &str) -> Result<Option<GroupRow>> {
        self.db().query_opt(
            "SELECT id, name FROM grp WHERE name = ?1",
            vals![name],
            |r| {
                Ok(GroupRow {
                    id: GroupId(r.get(0)?),
                    name: r.get(1)?,
                })
            },
        )
    }

    /// Every group, by name.
    fn groups(&self) -> Result<Vec<GroupRow>> {
        self.db()
            .query("SELECT id, name FROM grp ORDER BY name", vals![], |r| {
                Ok(GroupRow {
                    id: GroupId(r.get(0)?),
                    name: r.get(1)?,
                })
            })
    }

    /// Members of a group, by name.
    fn group_members(&self, group: GroupId) -> Result<Vec<PrincipalRow>> {
        self.db().query(
            &format!(
                "SELECT {} FROM principal p JOIN grp_member m ON m.principal_id = p.id \
             WHERE m.grp_id = ?1 ORDER BY p.name",
                PRINCIPAL_COLS
                    .split(", ")
                    .map(|c| format!("p.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            vals![group.0],
            principal_row,
        )
    }

    /// Groups the principal belongs to.
    fn groups_of(&self, principal: PrincipalId) -> Result<Vec<GroupId>> {
        self.db().query(
            "SELECT grp_id FROM grp_member WHERE principal_id = ?1 ORDER BY grp_id",
            vals![principal.0],
            |r| Ok(GroupId(r.get(0)?)),
        )
    }

    /// A token by the SHA-256 of its secret.
    fn token_by_hash(&self, hash: &[u8]) -> Result<Option<TokenRow>> {
        self.db().query_opt(
            &format!("SELECT {TOKEN_COLS} FROM token WHERE hash = ?1"),
            vals![hash],
            token_row,
        )
    }

    /// Tokens of a principal, oldest first.
    fn tokens_of(&self, principal: PrincipalId) -> Result<Vec<TokenRow>> {
        self.db().query(
            &format!("SELECT {TOKEN_COLS} FROM token WHERE principal_id = ?1 ORDER BY id"),
            vals![principal.0],
            token_row,
        )
    }

    /// Access rules that apply to a depot (its own and the global ones), in id order.
    fn acl_for_depot(&self, depot: DepotId) -> Result<Vec<AclRow>> {
        self.db().query(
            &format!(
                "SELECT {ACL_COLS} FROM acl WHERE depot_id = ?1 OR depot_id IS NULL ORDER BY id"
            ),
            vals![depot.0],
            acl_row,
        )
    }

    /// The access epoch: raised by every change to access rules or group membership.
    fn access_epoch(&self) -> Result<u64> {
        let v: i64 = self.db().value(
            "SELECT value FROM counter WHERE name = 'access_epoch'",
            vals![],
        )?;
        u64::try_from(v).map_err(|_| MetaError::BadValue("negative access epoch".into()))
    }

    /// Every access rule, in id order.
    fn acl_all(&self) -> Result<Vec<AclRow>> {
        self.db().query(
            &format!("SELECT {ACL_COLS} FROM acl ORDER BY id"),
            vals![],
            acl_row,
        )
    }

    /// A branch by name.
    fn branch(&self, depot: DepotId, name: &str) -> Result<Option<BranchRow>> {
        self.db().query_opt(
            &format!("SELECT {BRANCH_COLS} FROM branch WHERE depot_id = ?1 AND name = ?2"),
            vals![depot.0, name],
            branch_row,
        )
    }

    /// A branch by id.
    fn branch_by_id(&self, id: BranchId) -> Result<Option<BranchRow>> {
        self.db().query_opt(
            &format!("SELECT {BRANCH_COLS} FROM branch WHERE id = ?1"),
            vals![id.0],
            branch_row,
        )
    }

    /// Branches of a depot, by name.
    fn branches(&self, depot: DepotId) -> Result<Vec<BranchRow>> {
        self.db().query(
            &format!("SELECT {BRANCH_COLS} FROM branch WHERE depot_id = ?1 ORDER BY name"),
            vals![depot.0],
            branch_row,
        )
    }

    /// A revision.
    fn revision(&self, depot: DepotId, num: u64) -> Result<Option<RevisionRow>> {
        self.db().query_opt(
            &format!("SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND num = ?2"),
            vals![depot.0, to_i64(num)?],
            revision_row,
        )
    }

    /// The newest revision of a depot.
    fn last_revision(&self, depot: DepotId) -> Result<Option<RevisionRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 \
                 ORDER BY num DESC LIMIT 1"
            ),
            vals![depot.0],
            revision_row,
        )
    }

    /// The revision a client request id produced, for idempotent retries.
    fn revision_by_request_id(
        &self,
        depot: DepotId,
        request_id: &str,
    ) -> Result<Option<RevisionRow>> {
        self.db().query_opt(
            &format!(
                "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND request_id = ?2"
            ),
            vals![depot.0, request_id],
            revision_row,
        )
    }

    /// Revisions with numbers above `after`, ascending, across all branches.
    fn revisions_after(&self, depot: DepotId, after: u64, limit: u32) -> Result<Vec<RevisionRow>> {
        self.db().query(
            &format!(
                "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND num > ?2 \
             ORDER BY num LIMIT ?3"
            ),
            vals![depot.0, to_i64(after)?, limit],
            revision_row,
        )
    }

    /// Revisions matching `f`, in number order.
    fn find_revisions(&self, depot: DepotId, f: &RevisionFilter) -> Result<Vec<RevisionRow>> {
        use crate::sql::Val as Value;
        let mut sql = format!("SELECT {REVISION_COLS} FROM revision r WHERE r.depot_id = ?1");
        let mut args: Vec<Value<'static>> = vec![Value::Int(depot.0)];
        let mut arg = |sql: &mut String, frag: &str, v: Value<'static>| {
            args.push(v);
            sql.push_str(&frag.replace('?', &format!("?{}", args.len())));
        };
        if let Some(min) = f.min {
            arg(&mut sql, " AND r.num >= ?", Value::Int(to_i64(min)?));
        }
        if let Some(max) = f.max {
            arg(&mut sql, " AND r.num <= ?", Value::Int(to_i64(max)?));
        }
        if let Some(segs) = &f.segments {
            if segs.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(" AND (");
            for (i, seg) in segs.iter().enumerate() {
                if i > 0 {
                    sql.push_str(" OR ");
                }
                arg(&mut sql, "(r.branch_id = ?", Value::Int(seg.branch_id.0));
                match seg.max_rev {
                    Some(m) => arg(&mut sql, " AND r.num <= ?)", Value::Int(to_i64(m)?)),
                    None => sql.push(')'),
                }
            }
            sql.push(')');
        }
        if let Some(a) = f.author {
            arg(&mut sql, " AND r.author_id = ?", Value::Int(a.0));
        }
        if let Some(k) = f.author_kind {
            arg(
                &mut sql,
                " AND r.author_id IN (SELECT id FROM principal WHERE kind = ?)",
                Value::from(k.as_str()),
            );
        }
        if let Some(path) = &f.path {
            arg(
                &mut sql,
                " AND EXISTS (SELECT 1 FROM rev_change c WHERE c.depot_id = r.depot_id \
                 AND c.rev = r.num AND c.path = ?",
                Value::from(path.clone()),
            );
            if f.copies_only {
                sql.push_str(" AND c.action = 'A' AND c.copy_from_path IS NOT NULL");
            }
            sql.push(')');
        }
        sql.push_str(if f.ascending {
            " ORDER BY r.num ASC"
        } else {
            " ORDER BY r.num DESC"
        });
        arg(&mut sql, " LIMIT ?", Value::Int(i64::from(f.limit)));
        self.db().query(&sql, &args, revision_row)
    }

    /// Changes of a revision, by path.
    fn changes(&self, depot: DepotId, rev: u64) -> Result<Vec<ChangeRow>> {
        self.db().query(
            &format!(
            "SELECT {CHANGE_COLS} FROM rev_change WHERE depot_id = ?1 AND rev = ?2 ORDER BY path"
        ),
            vals![depot.0, to_i64(rev)?],
            |r| change_row_at(r, 0),
        )
    }

    /// Merges recorded on a revision.
    fn merges(&self, depot: DepotId, rev: u64) -> Result<Vec<MergeRow>> {
        self.db().query(
            "SELECT source_branch_id, from_rev, to_rev FROM rev_merge \
             WHERE depot_id = ?1 AND rev = ?2 ORDER BY rowid",
            vals![depot.0, to_i64(rev)?],
            |r| {
                Ok(MergeRow {
                    source_branch_id: BranchId(r.get(0)?),
                    from_rev: r.get::<u64>(1)?,
                    to_rev: r.get::<u64>(2)?,
                })
            },
        )
    }

    /// Metadata of a revision, by key.
    fn rev_meta(&self, depot: DepotId, rev: u64) -> Result<Vec<(String, String)>> {
        self.db().query(
            "SELECT key, value FROM rev_meta WHERE depot_id = ?1 AND rev = ?2 ORDER BY key",
            vals![depot.0, to_i64(rev)?],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// Outbox events after `after_id`, oldest first.
    fn outbox_after(&self, after_id: i64, limit: u32) -> Result<Vec<OutboxRow>> {
        self.db().query(
            "SELECT id, depot_id, kind, payload, created_at FROM outbox \
             WHERE id > ?1 ORDER BY id LIMIT ?2",
            vals![after_id, limit],
            outbox_row,
        )
    }

    /// Every subscriber cursor.
    fn outbox_cursors(&self) -> Result<Vec<(String, i64)>> {
        self.db().query(
            "SELECT subscriber, last_id FROM outbox_cursor ORDER BY subscriber",
            vals![],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// The history chain of a branch (ARCHITECTURE §4.3): its own revisions, then its
    /// parent's up to the branch point, and so on to the root branch.
    fn branch_chain(&self, branch: BranchId) -> Result<Vec<HistorySegment>> {
        let mut out = Vec::new();
        let mut current = Some((branch, None::<u64>));
        while let Some((id, cap)) = current {
            if out.len() > 10_000 {
                return Err(MetaError::BadValue("branch ancestry has a cycle".into()));
            }
            let row = self
                .branch_by_id(id)?
                .ok_or_else(|| MetaError::NotFound(format!("branch {}", id.0)))?;
            out.push(HistorySegment {
                branch_id: id,
                max_rev: cap,
            });
            current = match (row.parent_branch_id, row.parent_rev) {
                (Some(p), Some(r)) => Some((p, Some(cap.map_or(r, |c| c.min(r))))),
                _ => None,
            };
        }
        Ok(out)
    }

    /// Merges from `source` recorded on revisions in the history `chain` (from
    /// [`Reads::branch_chain`]), oldest first, each with the revision that recorded it.
    fn merges_from(
        &self,
        depot: DepotId,
        chain: &[HistorySegment],
        source: BranchId,
    ) -> Result<Vec<(u64, MergeRow)>> {
        let rows = self.db().query(
            "SELECT m.rev, r.branch_id, m.from_rev, m.to_rev FROM rev_merge m \
             JOIN revision r ON r.depot_id = m.depot_id AND r.num = m.rev \
             WHERE m.depot_id = ?1 AND m.source_branch_id = ?2 ORDER BY m.rev, m.rowid",
            vals![depot.0, source.0],
            |r| {
                Ok((
                    r.get::<u64>(0)?,
                    BranchId(r.get(1)?),
                    MergeRow {
                        source_branch_id: source,
                        from_rev: r.get::<u64>(2)?,
                        to_rev: r.get::<u64>(3)?,
                    },
                ))
            },
        )?;
        Ok(rows
            .into_iter()
            .filter(|(rev, b, _)| in_chain(chain, *b, *rev))
            .map(|(rev, _, m)| (rev, m))
            .collect())
    }

    /// Numbers of the revisions in the history `chain` newer than `after`, oldest first.
    fn chain_revs_after(
        &self,
        depot: DepotId,
        chain: &[HistorySegment],
        after: u64,
    ) -> Result<Vec<u64>> {
        let sql = "SELECT num FROM revision \
             WHERE depot_id = ?1 AND branch_id = ?2 AND num > ?3 AND num <= ?4 ORDER BY num";
        let mut out = Vec::new();
        for seg in chain {
            let cap = seg.max_rev.map(to_i64).transpose()?.unwrap_or(i64::MAX);
            out.extend(self.db().query(
                sql,
                vals![depot.0, seg.branch_id.0, to_i64(after)?, cap],
                |r| r.get::<u64>(0),
            )?);
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Changes to `path` visible from `branch`, newest first, following the branch chain.
    /// Revisions of a parent branch newer than the branch point are not included.
    fn path_history(
        &self,
        depot: DepotId,
        branch: BranchId,
        path: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PathHistoryEntry>> {
        let chain = self.branch_chain(branch)?;
        let mut out = Vec::new();
        let before = before.map(to_i64).transpose()?.unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT r.num, r.branch_id, {cols} FROM rev_change c \
             JOIN revision r ON r.depot_id = c.depot_id AND r.num = c.rev \
             WHERE c.depot_id = ?1 AND c.path = ?2 AND r.branch_id = ?3 \
               AND c.rev <= ?4 AND c.rev < ?5 \
             ORDER BY c.rev DESC LIMIT ?6",
            cols = CHANGE_COLS
                .split(", ")
                .map(|c| format!("c.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // Segments are ordered newest first and their ranges do not overlap in time beyond
        // the caps, so concatenating per-segment results and sorting gives the right order.
        for seg in &chain {
            let cap = seg.max_rev.map(to_i64).transpose()?.unwrap_or(i64::MAX);
            out.extend(self.db().query(
                &sql,
                vals![depot.0, path, seg.branch_id.0, cap, before, limit],
                |r| {
                    Ok(PathHistoryEntry {
                        rev: r.get::<u64>(0)?,
                        branch_id: BranchId(r.get(1)?),
                        change: change_row_at(r, 2)?,
                    })
                },
            )?);
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.rev));
        out.truncate(limit as usize);
        Ok(out)
    }
    /// Every obliterated id of a depot.
    fn obliterated_ids(&self, depot: DepotId) -> Result<Vec<ObjectId>> {
        self.db().query(
            "SELECT id FROM obliterated WHERE depot_id = ?1",
            vals![depot.0],
            |r| r.get::<ObjectId>(0),
        )
    }

    /// What was obliterated from a depot, oldest first.
    fn obliterations(&self, depot: DepotId) -> Result<Vec<ObliteratedRow>> {
        self.db().query(
            "SELECT depot_id, id, size, path, reason, by_id, at FROM obliterated \
             WHERE depot_id = ?1 ORDER BY at, path, id",
            vals![depot.0],
            |r| {
                Ok(ObliteratedRow {
                    depot_id: DepotId(r.get(0)?),
                    id: r.get::<ObjectId>(1)?,
                    size: r.get::<u64>(2)?,
                    path: r.get(3)?,
                    reason: r.get(4)?,
                    by: PrincipalId(r.get(5)?),
                    at: r.get(6)?,
                })
            },
        )
    }

    /// The last garbage collection and consistency checks of every depot.
    fn maintenance_runs(&self) -> Result<Vec<MaintenanceRunRow>> {
        self.db().query(
            "SELECT depot_id, kind, at, objects_checked, objects_affected, bytes_affected, \
             problems FROM maintenance_run ORDER BY depot_id, kind",
            vals![],
            |r| {
                Ok(MaintenanceRunRow {
                    depot_id: DepotId(r.get(0)?),
                    kind: r.get(1)?,
                    at: r.get(2)?,
                    objects_checked: r.get::<u64>(3)?,
                    objects_affected: r.get::<u64>(4)?,
                    bytes_affected: r.get::<u64>(5)?,
                    problems: r.get::<u64>(6)?,
                })
            },
        )
    }

    /// Every change to `path`, or to any path under it when `subtree` is set, on every
    /// branch, oldest first.
    fn changes_at_path(&self, depot: DepotId, path: &str, subtree: bool) -> Result<Vec<RevChange>> {
        let cols = CHANGE_COLS
            .split(", ")
            .map(|c| format!("c.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        // `path/` up to `path0` ('0' follows '/') is exactly the paths under `path`, and
        // the range keeps the query on the path index.
        let (cond, lo, hi) = if subtree {
            let base = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            let hi = if path.is_empty() {
                "\u{10FFFF}".to_owned()
            } else {
                format!("{path}0")
            };
            ("c.path >= ?2 AND c.path < ?3", base, Some(hi))
        } else {
            ("c.path = ?2", path.to_owned(), None)
        };
        let mut args = vec![Val::from(depot.0), Val::from(lo)];
        args.extend(hi.map(Val::from));
        self.db().query(
            &format!(
                "SELECT r.num, r.branch_id, {cols} FROM rev_change c \
             JOIN revision r ON r.depot_id = c.depot_id AND r.num = c.rev \
             WHERE c.depot_id = ?1 AND {cond} ORDER BY c.rev, c.path"
            ),
            &args,
            |r| {
                Ok(RevChange {
                    rev: r.get::<u64>(0)?,
                    branch_id: BranchId(r.get(1)?),
                    change: change_row_at(r, 2)?,
                })
            },
        )
    }

    /// Every change that stored one of `ids`, at any path, oldest first. Scans the depot's
    /// changes; meant for rare administrative use.
    fn changes_storing(&self, depot: DepotId, ids: &[ObjectId]) -> Result<Vec<RevChange>> {
        let cols = CHANGE_COLS
            .split(", ")
            .map(|c| format!("c.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut out = Vec::new();
        for part in ids.chunks(500) {
            let marks = (2..part.len() + 2)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args = vec![Val::from(depot.0)];
            args.extend(part.iter().map(|id| Val::from(id.as_bytes())));
            out.extend(self.db().query(
                &format!(
                    "SELECT r.num, r.branch_id, {cols} FROM rev_change c \
                     JOIN revision r ON r.depot_id = c.depot_id AND r.num = c.rev \
                     WHERE c.depot_id = ?1 AND c.new_id IN ({marks})"
                ),
                &args,
                |r| {
                    Ok(RevChange {
                        rev: r.get::<u64>(0)?,
                        branch_id: BranchId(r.get(1)?),
                        change: change_row_at(r, 2)?,
                    })
                },
            )?);
        }
        out.sort_by(|a, b| (a.rev, &a.change.path).cmp(&(b.rev, &b.change.path)));
        Ok(out)
    }
}

impl Reads for ReadTx<'_> {
    fn db(&self) -> &dyn Sql {
        self.conn()
    }
}

impl Reads for WriteTx<'_> {
    fn db(&self) -> &dyn Sql {
        self.conn()
    }
}

impl WriteTx<'_> {
    /// Creates a depot whose first revision will be number 1.
    pub fn create_depot(&self, name: &str, now: Millis) -> Result<DepotRow> {
        let new_id = self
            .conn()
            .value::<i64>(
                "INSERT INTO depot (name, next_rev, created_at) VALUES (?1, 1, ?2) RETURNING id",
                vals![name, now],
            )
            .map_err(unique("depot name"))?;
        let id = DepotId(new_id);
        self.depot(id)?
            .ok_or_else(|| MetaError::NotFound(format!("depot {}", id.0)))
    }

    /// Takes the next revision number of a depot.
    pub fn allocate_rev(&self, depot: DepotId) -> Result<u64> {
        let n: i64 = self
            .conn()
            .query_opt(
                "UPDATE depot SET next_rev = next_rev + 1 WHERE id = ?1 RETURNING next_rev - 1",
                vals![depot.0],
                |r| r.get(0),
            )?
            .ok_or_else(|| MetaError::NotFound(format!("depot {}", depot.0)))?;
        u64::try_from(n).map_err(|_| MetaError::BadValue("negative revision number".into()))
    }

    /// Creates a principal.
    pub fn create_principal(&self, p: &NewPrincipal, now: Millis) -> Result<PrincipalId> {
        let new_id = self.conn()
            .value::<i64>(
                "INSERT INTO principal (kind, name, display_name, owner_id, password_hash, disabled, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6) RETURNING id",
                vals![
                    p.kind.as_str(),
                    &p.name,
                    &p.display_name,
                    p.owner_id.map(|o| o.0),
                    &p.password_hash,
                    now
                ],
            )
            .map_err(unique("principal name"))?;
        Ok(PrincipalId(new_id))
    }

    /// Sets or clears a principal's password hash. The new password is an ordinary one: a
    /// pending "must change" mark is cleared.
    pub fn set_password_hash(&self, id: PrincipalId, hash: Option<&str>) -> Result<()> {
        self.set_password(id, hash, None)
    }

    /// Sets a principal's password hash together with its "must change" deadline: `Some(t)`
    /// makes the password good only for choosing a new one, and only until `t`.
    pub fn set_password(
        &self,
        id: PrincipalId,
        hash: Option<&str>,
        change_by: Option<Millis>,
    ) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE principal SET password_hash = ?2, password_change_by = ?3 WHERE id = ?1",
                vals![id.0, hash, change_by],
            )?,
            "principal",
        )
    }

    /// Links a principal to a single sign-on identity (stored in lower case), or removes the
    /// link. An identity already linked to someone else is `AlreadyExists`.
    pub fn set_principal_sso(&self, id: PrincipalId, sso_id: Option<&str>) -> Result<()> {
        let value = sso_id.map(str::to_lowercase);
        let n = self
            .conn()
            .execute(
                "UPDATE principal SET sso_id = ?2 WHERE id = ?1",
                vals![id.0, value],
            )
            .map_err(unique("single sign-on identity"))?;
        self.expect_one(n, "principal")
    }

    /// Deletes a principal: marks it deleted and disabled, drops its password, single
    /// sign-on link, tokens, group memberships and the access rules that name it. The row
    /// stays, because revisions refer to it. Raises the access epoch.
    pub fn delete_principal(&self, id: PrincipalId, now: Millis) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE principal SET deleted_at = ?2, disabled = 1, password_hash = NULL, \
                 sso_id = NULL WHERE id = ?1 AND deleted_at IS NULL",
                vals![id.0, now],
            )?,
            "principal",
        )?;
        self.conn()
            .execute("DELETE FROM token WHERE principal_id = ?1", vals![id.0])?;
        self.conn().execute(
            "DELETE FROM grp_member WHERE principal_id = ?1",
            vals![id.0],
        )?;
        self.conn().execute(
            "DELETE FROM acl WHERE subject_kind = 'user' AND subject_id = ?1",
            vals![id.0],
        )?;
        self.bump_access_epoch()
    }

    /// Moves a live depot to the trash. Its name stays taken until it is purged.
    pub fn trash_depot(&self, id: DepotId, now: Millis) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE depot SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                vals![id.0, now],
            )?,
            "depot",
        )
    }

    /// Takes a depot out of the trash.
    pub fn restore_depot(&self, id: DepotId) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE depot SET deleted_at = NULL WHERE id = ?1 AND purged_at IS NULL",
                vals![id.0],
            )?,
            "depot",
        )
    }

    /// Erases a depot's history and records from the core tables, keeping only its row as a
    /// tombstone, renamed to `#<id>` (not a valid depot name) so the name is free again and
    /// the id is never reused. Module tables are the modules' job, in the same transaction
    /// and before this. Raises the access epoch, since rules for the depot go too.
    pub fn purge_depot_rows(&self, id: DepotId, now: Millis) -> Result<()> {
        let c = self.conn();
        for table in [
            "rev_meta",
            "rev_merge",
            "rev_change",
            "revision",
            "branch",
            "acl",
            "obliterated",
            "maintenance_run",
            "outbox",
        ] {
            c.execute(
                &format!("DELETE FROM {table} WHERE depot_id = ?1"),
                vals![id.0],
            )?;
        }
        self.expect_one(
            c.execute(
                "UPDATE depot SET name = '#' || id, purged_at = ?2, \
                 deleted_at = coalesce(deleted_at, ?2) WHERE id = ?1 AND purged_at IS NULL",
                vals![id.0, now],
            )?,
            "depot",
        )?;
        self.bump_access_epoch()
    }

    /// Enables or disables a principal.
    pub fn set_principal_disabled(&self, id: PrincipalId, disabled: bool) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE principal SET disabled = ?2 WHERE id = ?1",
                vals![id.0, disabled as i64],
            )?,
            "principal",
        )
    }

    /// Creates a group.
    pub fn create_group(&self, name: &str) -> Result<GroupId> {
        let new_id = self
            .conn()
            .value::<i64>(
                "INSERT INTO grp (name) VALUES (?1) RETURNING id",
                vals![name],
            )
            .map_err(unique("group name"))?;
        Ok(GroupId(new_id))
    }

    /// Adds a member; adding an existing member does nothing. Raises the access epoch.
    pub fn add_group_member(&self, group: GroupId, principal: PrincipalId) -> Result<()> {
        self.conn().execute(
            "INSERT INTO grp_member (grp_id, principal_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            vals![group.0, principal.0],
        )?;
        self.bump_access_epoch()
    }

    /// Removes a member; removing a non-member does nothing. Raises the access epoch.
    pub fn remove_group_member(&self, group: GroupId, principal: PrincipalId) -> Result<()> {
        self.conn().execute(
            "DELETE FROM grp_member WHERE grp_id = ?1 AND principal_id = ?2",
            vals![group.0, principal.0],
        )?;
        self.bump_access_epoch()
    }

    /// Raises the access epoch by one, in this transaction.
    pub fn bump_access_epoch(&self) -> Result<()> {
        self.conn().execute(
            "UPDATE counter SET value = value + 1 WHERE name = 'access_epoch'",
            vals![],
        )?;
        Ok(())
    }

    /// Stores a token by the SHA-256 of its secret.
    pub fn create_token(
        &self,
        principal: PrincipalId,
        name: &str,
        hash: &[u8],
        scopes: &[String],
        expires_at: Option<Millis>,
        now: Millis,
    ) -> Result<TokenId> {
        if scopes
            .iter()
            .any(|s| s.is_empty() || s.contains(char::is_whitespace))
        {
            return Err(MetaError::BadValue(
                "scope names cannot contain spaces".into(),
            ));
        }
        let new_id = self
            .conn()
            .value::<i64>(
                "INSERT INTO token (principal_id, name, hash, scopes, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
                vals![principal.0, name, hash, scopes.join(" "), expires_at, now],
            )
            .map_err(unique("token"))?;
        Ok(TokenId(new_id))
    }

    /// Records a successful use of a token.
    pub fn touch_token(&self, id: TokenId, now: Millis) -> Result<()> {
        self.conn().execute(
            "UPDATE token SET last_used_at = ?2 WHERE id = ?1",
            vals![id.0, now],
        )?;
        Ok(())
    }

    /// Deletes a token.
    pub fn delete_token(&self, id: TokenId) -> Result<()> {
        self.expect_one(
            self.conn()
                .execute("DELETE FROM token WHERE id = ?1", vals![id.0])?,
            "token",
        )
    }

    /// Deletes every token of a principal.
    pub fn delete_tokens_of(&self, principal: PrincipalId) -> Result<usize> {
        self.conn().execute(
            "DELETE FROM token WHERE principal_id = ?1",
            vals![principal.0],
        )
    }

    /// Adds an access rule; `rule.id` is ignored. Raises the access epoch.
    pub fn insert_acl(&self, rule: &AclRow) -> Result<AclId> {
        let (kind, sid) = match rule.subject {
            AclSubject::All => ("all", None),
            AclSubject::User(p) => ("user", Some(p.0)),
            AclSubject::Group(g) => ("group", Some(g.0)),
        };
        let effect = match rule.effect {
            AclEffect::Allow => "allow",
            AclEffect::Deny => "deny",
        };
        let level = match rule.level {
            AccessLevel::Read => "read",
            AccessLevel::Write => "write",
            AccessLevel::Admin => "admin",
        };
        let new_id = self.conn().value::<i64>(
            "INSERT INTO acl (depot_id, subject_kind, subject_id, branch_glob, path_glob, effect, level) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
            vals![rule.depot_id.map(|d| d.0), kind, sid, &rule.branch_glob, &rule.path_glob, effect, level],
        )?;
        let id = AclId(new_id);
        self.bump_access_epoch()?;
        Ok(id)
    }

    /// Deletes an access rule. Raises the access epoch.
    pub fn delete_acl(&self, id: AclId) -> Result<()> {
        self.expect_one(
            self.conn()
                .execute("DELETE FROM acl WHERE id = ?1", vals![id.0])?,
            "acl rule",
        )?;
        self.bump_access_epoch()
    }

    /// Creates a branch row. The caller inserts the revision that `created_rev` and
    /// `head_rev` point to in the same transaction.
    pub fn create_branch(
        &self,
        depot: DepotId,
        name: &str,
        created_rev: u64,
        parent: Option<(BranchId, u64)>,
    ) -> Result<BranchId> {
        let new_id = self.conn()
            .value::<i64>(
                "INSERT INTO branch (depot_id, name, head_rev, created_rev, parent_branch_id, parent_rev) \
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5) RETURNING id",
                vals![
                    depot.0,
                    name,
                    to_i64(created_rev)?,
                    parent.map(|p| p.0 .0),
                    parent.map(|p| to_i64(p.1)).transpose()?
                ],
            )
            .map_err(unique("branch name"))?;
        Ok(BranchId(new_id))
    }

    /// Moves a branch head.
    pub fn set_branch_head(&self, branch: BranchId, rev: u64) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE branch SET head_rev = ?2 WHERE id = ?1",
                vals![branch.0, to_i64(rev)?],
            )?,
            "branch",
        )
    }

    /// Archives or reactivates a branch.
    pub fn set_branch_state(&self, branch: BranchId, state: BranchState) -> Result<()> {
        let s = match state {
            BranchState::Active => "active",
            BranchState::Archived => "archived",
        };
        self.expect_one(
            self.conn().execute(
                "UPDATE branch SET state = ?2 WHERE id = ?1",
                vals![branch.0, s],
            )?,
            "branch",
        )
    }

    /// Sets or clears a branch's stream type.
    pub fn set_branch_stream(&self, branch: BranchId, stream: Option<StreamType>) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE branch SET stream_type = ?2 WHERE id = ?1",
                vals![branch.0, stream.map(StreamType::as_str)],
            )?,
            "branch",
        )
    }

    /// Inserts a revision row.
    pub fn insert_revision(&self, r: &RevisionRow) -> Result<()> {
        self.conn()
            .execute(
                &format!("INSERT INTO revision ({REVISION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
                vals![
                    r.depot_id.0,
                    to_i64(r.num)?,
                    r.branch_id.0,
                    r.parent_num.map(to_i64).transpose()?,
                    r.root_tree.as_bytes().as_slice(),
                    r.author_id.0,
                    r.submitter_id.0,
                    r.created_at,
                    &r.message,
                    &r.provenance,
                    &r.request_id
                ],
            )
            .map_err(unique("revision number or request id"))?;
        Ok(())
    }

    /// Inserts the history-index rows of a revision.
    pub fn insert_changes(&self, depot: DepotId, rev: u64, changes: &[ChangeRow]) -> Result<()> {
        let sql = format!(
            "INSERT INTO rev_change (depot_id, rev, {CHANGE_COLS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        );
        let rev = to_i64(rev)?;
        for c in changes {
            self.conn()
                .execute(
                    &sql,
                    vals![
                        depot.0,
                        rev,
                        &c.path,
                        c.action.to_string(),
                        c.kind.map(i64::from),
                        c.old_id.as_ref().map(|i| i.as_bytes().to_vec()),
                        c.new_id.as_ref().map(|i| i.as_bytes().to_vec()),
                        c.size.map(to_i64).transpose()?,
                        i64::from(c.flags),
                        &c.copy_from_path,
                        c.copy_from_rev.map(to_i64).transpose()?
                    ],
                )
                .map_err(unique("change path in revision"))?;
        }
        Ok(())
    }

    /// Records a merge on a revision.
    pub fn insert_merge(&self, depot: DepotId, rev: u64, m: &MergeRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO rev_merge (depot_id, rev, source_branch_id, from_rev, to_rev) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            vals![
                depot.0,
                to_i64(rev)?,
                m.source_branch_id.0,
                to_i64(m.from_rev)?,
                to_i64(m.to_rev)?
            ],
        )?;
        Ok(())
    }

    /// Sets one metadata value on a revision. Keys are namespaced `<module>.<key>`.
    pub fn set_rev_meta(&self, depot: DepotId, rev: u64, key: &str, value: &str) -> Result<()> {
        if !key.contains('.') {
            return Err(MetaError::BadValue(format!(
                "rev_meta key {key:?} needs a module prefix"
            )));
        }
        self.conn().execute(
            "INSERT INTO rev_meta (depot_id, rev, key, value) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (depot_id, rev, key) DO UPDATE SET value = excluded.value",
            vals![depot.0, to_i64(rev)?, key, value],
        )?;
        Ok(())
    }

    /// Appends an event to the outbox and returns its id.
    pub fn append_outbox(
        &self,
        depot: Option<DepotId>,
        kind: &str,
        payload: &str,
        now: Millis,
    ) -> Result<i64> {
        let new_id = self.conn().value::<i64>(
            "INSERT INTO outbox (depot_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4) RETURNING id",
            vals![depot.map(|d| d.0), kind, payload, now],
        )?;
        Ok(new_id)
    }

    /// Moves a subscriber's cursor.
    pub fn set_outbox_cursor(&self, subscriber: &str, last_id: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO outbox_cursor (subscriber, last_id) VALUES (?1, ?2) \
             ON CONFLICT (subscriber) DO UPDATE SET last_id = excluded.last_id",
            vals![subscriber, last_id],
        )?;
        Ok(())
    }

    /// Removes a subscriber's cursor.
    pub fn delete_outbox_cursor(&self, subscriber: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM outbox_cursor WHERE subscriber = ?1",
            vals![subscriber],
        )?;
        Ok(())
    }

    /// Deletes outbox rows with ids up to and including `upto`.
    pub fn prune_outbox(&self, upto: i64) -> Result<usize> {
        self.conn()
            .execute("DELETE FROM outbox WHERE id <= ?1", vals![upto])
    }

    fn expect_one(&self, changed: usize, what: &str) -> Result<()> {
        if changed == 0 {
            Err(MetaError::NotFound(what.to_owned()))
        } else {
            Ok(())
        }
    }

    /// Records a finished garbage collection or consistency check, replacing the depot's
    /// previous one of that kind.
    pub fn record_maintenance_run(&self, row: &MaintenanceRunRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO maintenance_run (depot_id, kind, at, objects_checked, \
             objects_affected, bytes_affected, problems) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (depot_id, kind) DO UPDATE SET at = excluded.at, \
             objects_checked = excluded.objects_checked, \
             objects_affected = excluded.objects_affected, \
             bytes_affected = excluded.bytes_affected, problems = excluded.problems",
            vals![
                row.depot_id.0,
                &row.kind,
                row.at,
                to_i64(row.objects_checked)?,
                to_i64(row.objects_affected)?,
                to_i64(row.bytes_affected)?,
                to_i64(row.problems)?
            ],
        )?;
        Ok(())
    }

    /// Records obliterated content. Recording the same id again keeps the first record.
    pub fn insert_obliterated(&self, row: &ObliteratedRow) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT INTO obliterated (depot_id, id, size, path, reason, by_id, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (depot_id, id) DO NOTHING",
            vals![
                row.depot_id.0,
                row.id.as_bytes().as_slice(),
                to_i64(row.size)?,
                &row.path,
                &row.reason,
                row.by.0,
                row.at
            ],
        )?;
        Ok(n == 1)
    }
}
