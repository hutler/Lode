//! Query methods. Reads are on the [`Reads`] trait, which both transaction types implement;
//! writes are inherent methods of [`WriteTx`].

use lode_core::{DepotId, ObjectId};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::unique;
use crate::rows::*;
use crate::{MetaError, ReadTx, Result, WriteTx};

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| MetaError::BadValue(format!("{v} does not fit in an i64")))
}

fn get_u64(r: &Row<'_>, i: usize) -> rusqlite::Result<u64> {
    let v: i64 = r.get(i)?;
    u64::try_from(v).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Integer, Box::new(e))
    })
}

fn get_opt_u64(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<u64>> {
    let v: Option<i64> = r.get(i)?;
    v.map(|v| {
        u64::try_from(v).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                i,
                rusqlite::types::Type::Integer,
                Box::new(e),
            )
        })
    })
    .transpose()
}

fn bad(i: usize, msg: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, msg.to_owned().into())
}

fn get_id(r: &Row<'_>, i: usize) -> rusqlite::Result<ObjectId> {
    let b: Vec<u8> = r.get(i)?;
    let arr: [u8; 32] = b
        .try_into()
        .map_err(|_| bad(i, "object id is not 32 bytes"))?;
    Ok(ObjectId::from_bytes(arr))
}

fn get_opt_id(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<ObjectId>> {
    let b: Option<Vec<u8>> = r.get(i)?;
    b.map(|b| {
        let arr: [u8; 32] = b
            .try_into()
            .map_err(|_| bad(i, "object id is not 32 bytes"))?;
        Ok(ObjectId::from_bytes(arr))
    })
    .transpose()
}

const DEPOT_COLS: &str = "id, name, next_rev, created_at, deleted_at";
fn depot_row(r: &Row<'_>) -> rusqlite::Result<DepotRow> {
    Ok(DepotRow {
        id: DepotId(r.get(0)?),
        name: r.get(1)?,
        next_rev: get_u64(r, 2)?,
        created_at: r.get(3)?,
        deleted_at: r.get(4)?,
    })
}

const PRINCIPAL_COLS: &str =
    "id, kind, name, display_name, owner_id, password_hash, disabled, created_at, sso_id, deleted_at, \
     password_change_by";
fn principal_row(r: &Row<'_>) -> rusqlite::Result<PrincipalRow> {
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
        owner_id: r.get::<_, Option<i64>>(4)?.map(PrincipalId),
        password_hash: r.get(5)?,
        disabled: r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
        sso_id: r.get(8)?,
        deleted_at: r.get(9)?,
        password_change_by: r.get(10)?,
    })
}

const TOKEN_COLS: &str =
    "id, principal_id, name, hash, scopes, expires_at, last_used_at, created_at";
fn token_row(r: &Row<'_>) -> rusqlite::Result<TokenRow> {
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
fn acl_row(r: &Row<'_>) -> rusqlite::Result<AclRow> {
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
        depot_id: r.get::<_, Option<i64>>(1)?.map(DepotId),
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
    "id, depot_id, name, head_rev, created_rev, parent_branch_id, parent_rev, state";
fn branch_row(r: &Row<'_>) -> rusqlite::Result<BranchRow> {
    let state: String = r.get(7)?;
    Ok(BranchRow {
        id: BranchId(r.get(0)?),
        depot_id: DepotId(r.get(1)?),
        name: r.get(2)?,
        head_rev: get_u64(r, 3)?,
        created_rev: get_u64(r, 4)?,
        parent_branch_id: r.get::<_, Option<i64>>(5)?.map(BranchId),
        parent_rev: get_opt_u64(r, 6)?,
        state: match state.as_str() {
            "active" => BranchState::Active,
            "archived" => BranchState::Archived,
            _ => return Err(bad(7, "bad branch state")),
        },
    })
}

const REVISION_COLS: &str = "depot_id, num, branch_id, parent_num, root_tree, author_id, \
     submitter_id, created_at, message, provenance, request_id";
fn revision_row(r: &Row<'_>) -> rusqlite::Result<RevisionRow> {
    Ok(RevisionRow {
        depot_id: DepotId(r.get(0)?),
        num: get_u64(r, 1)?,
        branch_id: BranchId(r.get(2)?),
        parent_num: get_opt_u64(r, 3)?,
        root_tree: get_id(r, 4)?,
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
fn change_row_at(r: &Row<'_>, o: usize) -> rusqlite::Result<ChangeRow> {
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
        kind: r.get::<_, Option<i64>>(o + 2)?.map(|k| k as u8),
        old_id: get_opt_id(r, o + 3)?,
        new_id: get_opt_id(r, o + 4)?,
        size: get_opt_u64(r, o + 5)?,
        flags: r.get::<_, i64>(o + 6)? as u8,
        copy_from_path: r.get(o + 7)?,
        copy_from_rev: get_opt_u64(r, o + 8)?,
    })
}

fn outbox_row(r: &Row<'_>) -> rusqlite::Result<OutboxRow> {
    Ok(OutboxRow {
        id: r.get(0)?,
        depot_id: r.get::<_, Option<i64>>(1)?.map(DepotId),
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

fn collect<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// Read queries, available in both read and write transactions.
pub trait Reads {
    /// The connection the queries run on.
    fn db(&self) -> &Connection;

    /// A live depot by name; depots in the trash are left out.
    fn depot_by_name(&self, name: &str) -> Result<Option<DepotRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {DEPOT_COLS} FROM depot WHERE name = ?1 AND deleted_at IS NULL"),
                [name],
                depot_row,
            )
            .optional()?)
    }

    /// A depot in the trash, by name. Purged depots have lost their name and are not found.
    fn trashed_depot(&self, name: &str) -> Result<Option<DepotRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {DEPOT_COLS} FROM depot \
                     WHERE name = ?1 AND deleted_at IS NOT NULL AND purged_at IS NULL"
                ),
                [name],
                depot_row,
            )
            .optional()?)
    }

    /// A depot by id, live, in the trash or purged.
    fn depot(&self, id: DepotId) -> Result<Option<DepotRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {DEPOT_COLS} FROM depot WHERE id = ?1"),
                [id.0],
                depot_row,
            )
            .optional()?)
    }

    /// Every live depot, by name.
    fn depots(&self) -> Result<Vec<DepotRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {DEPOT_COLS} FROM depot WHERE deleted_at IS NULL ORDER BY name"
        ))?;
        let rows = st.query_map([], depot_row)?;
        collect(rows)
    }

    /// Depots in the trash and not yet purged, oldest deletion first.
    fn trashed_depots(&self) -> Result<Vec<DepotRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {DEPOT_COLS} FROM depot WHERE deleted_at IS NOT NULL AND purged_at IS NULL \
             ORDER BY deleted_at, name"
        ))?;
        let rows = st.query_map([], depot_row)?;
        collect(rows)
    }

    /// A principal by id.
    fn principal(&self, id: PrincipalId) -> Result<Option<PrincipalRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {PRINCIPAL_COLS} FROM principal WHERE id = ?1"),
                [id.0],
                principal_row,
            )
            .optional()?)
    }

    /// A principal by name; deleted principals are left out.
    fn principal_by_name(&self, name: &str) -> Result<Option<PrincipalRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {PRINCIPAL_COLS} FROM principal WHERE name = ?1 AND deleted_at IS NULL"
                ),
                [name],
                principal_row,
            )
            .optional()?)
    }

    /// A deleted principal by name. Its name stays taken, so it can be told apart from a
    /// free name.
    fn deleted_principal(&self, name: &str) -> Result<Option<PrincipalRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {PRINCIPAL_COLS} FROM principal \
                     WHERE name = ?1 AND deleted_at IS NOT NULL"
                ),
                [name],
                principal_row,
            )
            .optional()?)
    }

    /// The principal linked to a single sign-on identity (compared in lower case).
    fn principal_by_sso(&self, sso_id: &str) -> Result<Option<PrincipalRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {PRINCIPAL_COLS} FROM principal \
                     WHERE sso_id = ?1 AND deleted_at IS NULL"
                ),
                [sso_id.to_lowercase()],
                principal_row,
            )
            .optional()?)
    }

    /// Every principal that is not deleted, by name.
    fn principals(&self) -> Result<Vec<PrincipalRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {PRINCIPAL_COLS} FROM principal WHERE deleted_at IS NULL ORDER BY name"
        ))?;
        let rows = st.query_map([], principal_row)?;
        collect(rows)
    }

    /// A group by name.
    fn group_by_name(&self, name: &str) -> Result<Option<GroupRow>> {
        Ok(self
            .db()
            .query_row("SELECT id, name FROM grp WHERE name = ?1", [name], |r| {
                Ok(GroupRow {
                    id: GroupId(r.get(0)?),
                    name: r.get(1)?,
                })
            })
            .optional()?)
    }

    /// Every group, by name.
    fn groups(&self) -> Result<Vec<GroupRow>> {
        let mut st = self
            .db()
            .prepare("SELECT id, name FROM grp ORDER BY name")?;
        let rows = st.query_map([], |r| {
            Ok(GroupRow {
                id: GroupId(r.get(0)?),
                name: r.get(1)?,
            })
        })?;
        collect(rows)
    }

    /// Members of a group, by name.
    fn group_members(&self, group: GroupId) -> Result<Vec<PrincipalRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {} FROM principal p JOIN grp_member m ON m.principal_id = p.id \
             WHERE m.grp_id = ?1 ORDER BY p.name",
            PRINCIPAL_COLS
                .split(", ")
                .map(|c| format!("p.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = st.query_map([group.0], principal_row)?;
        collect(rows)
    }

    /// Groups the principal belongs to.
    fn groups_of(&self, principal: PrincipalId) -> Result<Vec<GroupId>> {
        let mut st = self
            .db()
            .prepare("SELECT grp_id FROM grp_member WHERE principal_id = ?1 ORDER BY grp_id")?;
        let rows = st.query_map([principal.0], |r| Ok(GroupId(r.get(0)?)))?;
        collect(rows)
    }

    /// A token by the SHA-256 of its secret.
    fn token_by_hash(&self, hash: &[u8]) -> Result<Option<TokenRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {TOKEN_COLS} FROM token WHERE hash = ?1"),
                [hash],
                token_row,
            )
            .optional()?)
    }

    /// Tokens of a principal, oldest first.
    fn tokens_of(&self, principal: PrincipalId) -> Result<Vec<TokenRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {TOKEN_COLS} FROM token WHERE principal_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([principal.0], token_row)?;
        collect(rows)
    }

    /// Access rules that apply to a depot (its own and the global ones), in id order.
    fn acl_for_depot(&self, depot: DepotId) -> Result<Vec<AclRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {ACL_COLS} FROM acl WHERE depot_id = ?1 OR depot_id IS NULL ORDER BY id"
        ))?;
        let rows = st.query_map([depot.0], acl_row)?;
        collect(rows)
    }

    /// The access epoch: raised by every change to access rules or group membership.
    fn access_epoch(&self) -> Result<u64> {
        let v: i64 = self.db().query_row(
            "SELECT value FROM counter WHERE name = 'access_epoch'",
            [],
            |r| r.get(0),
        )?;
        u64::try_from(v).map_err(|_| MetaError::BadValue("negative access epoch".into()))
    }

    /// Every access rule, in id order.
    fn acl_all(&self) -> Result<Vec<AclRow>> {
        let mut st = self
            .db()
            .prepare(&format!("SELECT {ACL_COLS} FROM acl ORDER BY id"))?;
        let rows = st.query_map([], acl_row)?;
        collect(rows)
    }

    /// A branch by name.
    fn branch(&self, depot: DepotId, name: &str) -> Result<Option<BranchRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {BRANCH_COLS} FROM branch WHERE depot_id = ?1 AND name = ?2"),
                params![depot.0, name],
                branch_row,
            )
            .optional()?)
    }

    /// A branch by id.
    fn branch_by_id(&self, id: BranchId) -> Result<Option<BranchRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {BRANCH_COLS} FROM branch WHERE id = ?1"),
                [id.0],
                branch_row,
            )
            .optional()?)
    }

    /// Branches of a depot, by name.
    fn branches(&self, depot: DepotId) -> Result<Vec<BranchRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {BRANCH_COLS} FROM branch WHERE depot_id = ?1 ORDER BY name"
        ))?;
        let rows = st.query_map([depot.0], branch_row)?;
        collect(rows)
    }

    /// A revision.
    fn revision(&self, depot: DepotId, num: u64) -> Result<Option<RevisionRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND num = ?2"),
                params![depot.0, to_i64(num)?],
                revision_row,
            )
            .optional()?)
    }

    /// The newest revision of a depot.
    fn last_revision(&self, depot: DepotId) -> Result<Option<RevisionRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 \
                     ORDER BY num DESC LIMIT 1"
                ),
                params![depot.0],
                revision_row,
            )
            .optional()?)
    }

    /// The revision a client request id produced, for idempotent retries.
    fn revision_by_request_id(
        &self,
        depot: DepotId,
        request_id: &str,
    ) -> Result<Option<RevisionRow>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND request_id = ?2"
                ),
                params![depot.0, request_id],
                revision_row,
            )
            .optional()?)
    }

    /// Revisions with numbers above `after`, ascending, across all branches.
    fn revisions_after(&self, depot: DepotId, after: u64, limit: u32) -> Result<Vec<RevisionRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {REVISION_COLS} FROM revision WHERE depot_id = ?1 AND num > ?2 \
             ORDER BY num LIMIT ?3"
        ))?;
        let rows = st.query_map(params![depot.0, to_i64(after)?, limit], revision_row)?;
        collect(rows)
    }

    /// Revisions matching `f`, in number order.
    fn find_revisions(&self, depot: DepotId, f: &RevisionFilter) -> Result<Vec<RevisionRow>> {
        use rusqlite::types::Value;
        let mut sql = format!("SELECT {REVISION_COLS} FROM revision r WHERE r.depot_id = ?1");
        let mut args: Vec<Value> = vec![Value::Integer(depot.0)];
        let mut arg = |sql: &mut String, frag: &str, v: Value| {
            args.push(v);
            sql.push_str(&frag.replace('?', &format!("?{}", args.len())));
        };
        if let Some(min) = f.min {
            arg(&mut sql, " AND r.num >= ?", Value::Integer(to_i64(min)?));
        }
        if let Some(max) = f.max {
            arg(&mut sql, " AND r.num <= ?", Value::Integer(to_i64(max)?));
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
                arg(
                    &mut sql,
                    "(r.branch_id = ?",
                    Value::Integer(seg.branch_id.0),
                );
                match seg.max_rev {
                    Some(m) => arg(&mut sql, " AND r.num <= ?)", Value::Integer(to_i64(m)?)),
                    None => sql.push(')'),
                }
            }
            sql.push(')');
        }
        if let Some(a) = f.author {
            arg(&mut sql, " AND r.author_id = ?", Value::Integer(a.0));
        }
        if let Some(k) = f.author_kind {
            arg(
                &mut sql,
                " AND r.author_id IN (SELECT id FROM principal WHERE kind = ?)",
                Value::Text(k.as_str().to_owned()),
            );
        }
        if let Some(path) = &f.path {
            arg(
                &mut sql,
                " AND EXISTS (SELECT 1 FROM rev_change c WHERE c.depot_id = r.depot_id \
                 AND c.rev = r.num AND c.path = ?",
                Value::Text(path.clone()),
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
        arg(&mut sql, " LIMIT ?", Value::Integer(i64::from(f.limit)));
        let mut st = self.db().prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), revision_row)?;
        collect(rows)
    }

    /// Changes of a revision, by path.
    fn changes(&self, depot: DepotId, rev: u64) -> Result<Vec<ChangeRow>> {
        let mut st = self.db().prepare(&format!(
            "SELECT {CHANGE_COLS} FROM rev_change WHERE depot_id = ?1 AND rev = ?2 ORDER BY path"
        ))?;
        let rows = st.query_map(params![depot.0, to_i64(rev)?], |r| change_row_at(r, 0))?;
        collect(rows)
    }

    /// Merges recorded on a revision.
    fn merges(&self, depot: DepotId, rev: u64) -> Result<Vec<MergeRow>> {
        let mut st = self.db().prepare(
            "SELECT source_branch_id, from_rev, to_rev FROM rev_merge \
             WHERE depot_id = ?1 AND rev = ?2 ORDER BY rowid",
        )?;
        let rows = st.query_map(params![depot.0, to_i64(rev)?], |r| {
            Ok(MergeRow {
                source_branch_id: BranchId(r.get(0)?),
                from_rev: get_u64(r, 1)?,
                to_rev: get_u64(r, 2)?,
            })
        })?;
        collect(rows)
    }

    /// Metadata of a revision, by key.
    fn rev_meta(&self, depot: DepotId, rev: u64) -> Result<Vec<(String, String)>> {
        let mut st = self.db().prepare(
            "SELECT key, value FROM rev_meta WHERE depot_id = ?1 AND rev = ?2 ORDER BY key",
        )?;
        let rows = st.query_map(params![depot.0, to_i64(rev)?], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        collect(rows)
    }

    /// Outbox events after `after_id`, oldest first.
    fn outbox_after(&self, after_id: i64, limit: u32) -> Result<Vec<OutboxRow>> {
        let mut st = self.db().prepare(
            "SELECT id, depot_id, kind, payload, created_at FROM outbox \
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = st.query_map(params![after_id, limit], outbox_row)?;
        collect(rows)
    }

    /// Every subscriber cursor.
    fn outbox_cursors(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self
            .db()
            .prepare("SELECT subscriber, last_id FROM outbox_cursor ORDER BY subscriber")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        collect(rows)
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
        let mut st = self.db().prepare(
            "SELECT m.rev, r.branch_id, m.from_rev, m.to_rev FROM rev_merge m \
             JOIN revision r ON r.depot_id = m.depot_id AND r.num = m.rev \
             WHERE m.depot_id = ?1 AND m.source_branch_id = ?2 ORDER BY m.rev, m.rowid",
        )?;
        let rows = st.query_map(params![depot.0, source.0], |r| {
            Ok((
                get_u64(r, 0)?,
                BranchId(r.get(1)?),
                MergeRow {
                    source_branch_id: source,
                    from_rev: get_u64(r, 2)?,
                    to_rev: get_u64(r, 3)?,
                },
            ))
        })?;
        Ok(collect(rows)?
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
        let mut st = self.db().prepare(
            "SELECT num FROM revision \
             WHERE depot_id = ?1 AND branch_id = ?2 AND num > ?3 AND num <= ?4 ORDER BY num",
        )?;
        let mut out = Vec::new();
        for seg in chain {
            let cap = seg.max_rev.map(to_i64).transpose()?.unwrap_or(i64::MAX);
            let rows = st.query_map(
                params![depot.0, seg.branch_id.0, to_i64(after)?, cap],
                |r| get_u64(r, 0),
            )?;
            out.extend(collect(rows)?);
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
        let mut st = self.db().prepare(&format!(
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
        ))?;
        // Segments are ordered newest first and their ranges do not overlap in time beyond
        // the caps, so concatenating per-segment results and sorting gives the right order.
        for seg in &chain {
            let cap = seg.max_rev.map(to_i64).transpose()?.unwrap_or(i64::MAX);
            let rows = st.query_map(
                params![depot.0, path, seg.branch_id.0, cap, before, limit],
                |r| {
                    Ok(PathHistoryEntry {
                        rev: get_u64(r, 0)?,
                        branch_id: BranchId(r.get(1)?),
                        change: change_row_at(r, 2)?,
                    })
                },
            )?;
            out.extend(collect(rows)?);
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.rev));
        out.truncate(limit as usize);
        Ok(out)
    }
    /// Every obliterated id of a depot.
    fn obliterated_ids(&self, depot: DepotId) -> Result<Vec<ObjectId>> {
        let mut st = self
            .db()
            .prepare("SELECT id FROM obliterated WHERE depot_id = ?1")?;
        let rows = st.query_map([depot.0], |r| get_id(r, 0))?;
        collect(rows)
    }

    /// What was obliterated from a depot, oldest first.
    fn obliterations(&self, depot: DepotId) -> Result<Vec<ObliteratedRow>> {
        let mut st = self.db().prepare(
            "SELECT depot_id, id, size, path, reason, by_id, at FROM obliterated \
             WHERE depot_id = ?1 ORDER BY at, path, id",
        )?;
        let rows = st.query_map([depot.0], |r| {
            Ok(ObliteratedRow {
                depot_id: DepotId(r.get(0)?),
                id: get_id(r, 1)?,
                size: get_u64(r, 2)?,
                path: r.get(3)?,
                reason: r.get(4)?,
                by: PrincipalId(r.get(5)?),
                at: r.get(6)?,
            })
        })?;
        collect(rows)
    }

    /// The last garbage collection and consistency checks of every depot.
    fn maintenance_runs(&self) -> Result<Vec<MaintenanceRunRow>> {
        let mut st = self.db().prepare(
            "SELECT depot_id, kind, at, objects_checked, objects_affected, bytes_affected, \
             problems FROM maintenance_run ORDER BY depot_id, kind",
        )?;
        let rows = st.query_map([], |r| {
            Ok(MaintenanceRunRow {
                depot_id: DepotId(r.get(0)?),
                kind: r.get(1)?,
                at: r.get(2)?,
                objects_checked: get_u64(r, 3)?,
                objects_affected: get_u64(r, 4)?,
                bytes_affected: get_u64(r, 5)?,
                problems: get_u64(r, 6)?,
            })
        })?;
        collect(rows)
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
            ("c.path >= ?2 AND c.path < ?3", base, hi)
        } else {
            ("c.path = ?2 AND ?3 = ?3", path.to_owned(), String::new())
        };
        let mut st = self.db().prepare(&format!(
            "SELECT r.num, r.branch_id, {cols} FROM rev_change c \
             JOIN revision r ON r.depot_id = c.depot_id AND r.num = c.rev \
             WHERE c.depot_id = ?1 AND {cond} ORDER BY c.rev, c.path"
        ))?;
        let rows = st.query_map(params![depot.0, lo, hi], |r| {
            Ok(RevChange {
                rev: get_u64(r, 0)?,
                branch_id: BranchId(r.get(1)?),
                change: change_row_at(r, 2)?,
            })
        })?;
        collect(rows)
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
            let marks = vec!["?"; part.len()].join(", ");
            let mut st = self.db().prepare(&format!(
                "SELECT r.num, r.branch_id, {cols} FROM rev_change c \
                 JOIN revision r ON r.depot_id = c.depot_id AND r.num = c.rev \
                 WHERE c.depot_id = ? AND c.new_id IN ({marks})"
            ))?;
            let mut args: Vec<rusqlite::types::Value> = vec![depot.0.into()];
            args.extend(part.iter().map(|id| id.as_bytes().to_vec().into()));
            let rows = st.query_map(rusqlite::params_from_iter(args), |r| {
                Ok(RevChange {
                    rev: get_u64(r, 0)?,
                    branch_id: BranchId(r.get(1)?),
                    change: change_row_at(r, 2)?,
                })
            })?;
            out.extend(collect(rows)?);
        }
        out.sort_by(|a, b| (a.rev, &a.change.path).cmp(&(b.rev, &b.change.path)));
        Ok(out)
    }
}

impl Reads for ReadTx<'_> {
    fn db(&self) -> &Connection {
        self.conn()
    }
}

impl Reads for WriteTx<'_> {
    fn db(&self) -> &Connection {
        self.conn()
    }
}

impl WriteTx<'_> {
    /// Creates a depot whose first revision will be number 1.
    pub fn create_depot(&self, name: &str, now: Millis) -> Result<DepotRow> {
        self.conn()
            .execute(
                "INSERT INTO depot (name, next_rev, created_at) VALUES (?1, 1, ?2)",
                params![name, now],
            )
            .map_err(unique("depot name"))?;
        let id = DepotId(self.conn().last_insert_rowid());
        self.depot(id)?
            .ok_or_else(|| MetaError::NotFound(format!("depot {}", id.0)))
    }

    /// Takes the next revision number of a depot.
    pub fn allocate_rev(&self, depot: DepotId) -> Result<u64> {
        let n: i64 = self
            .conn()
            .query_row(
                "UPDATE depot SET next_rev = next_rev + 1 WHERE id = ?1 RETURNING next_rev - 1",
                [depot.0],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| MetaError::NotFound(format!("depot {}", depot.0)))?;
        u64::try_from(n).map_err(|_| MetaError::BadValue("negative revision number".into()))
    }

    /// Creates a principal.
    pub fn create_principal(&self, p: &NewPrincipal, now: Millis) -> Result<PrincipalId> {
        self.conn()
            .execute(
                "INSERT INTO principal (kind, name, display_name, owner_id, password_hash, disabled, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![
                    p.kind.as_str(),
                    p.name,
                    p.display_name,
                    p.owner_id.map(|o| o.0),
                    p.password_hash,
                    now
                ],
            )
            .map_err(unique("principal name"))?;
        Ok(PrincipalId(self.conn().last_insert_rowid()))
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
                params![id.0, hash, change_by],
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
                params![id.0, value],
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
                params![id.0, now],
            )?,
            "principal",
        )?;
        self.conn()
            .execute("DELETE FROM token WHERE principal_id = ?1", [id.0])?;
        self.conn()
            .execute("DELETE FROM grp_member WHERE principal_id = ?1", [id.0])?;
        self.conn().execute(
            "DELETE FROM acl WHERE subject_kind = 'user' AND subject_id = ?1",
            [id.0],
        )?;
        self.bump_access_epoch()
    }

    /// Moves a live depot to the trash. Its name stays taken until it is purged.
    pub fn trash_depot(&self, id: DepotId, now: Millis) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE depot SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                params![id.0, now],
            )?,
            "depot",
        )
    }

    /// Takes a depot out of the trash.
    pub fn restore_depot(&self, id: DepotId) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE depot SET deleted_at = NULL WHERE id = ?1 AND purged_at IS NULL",
                [id.0],
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
            c.execute(&format!("DELETE FROM {table} WHERE depot_id = ?1"), [id.0])?;
        }
        self.expect_one(
            c.execute(
                "UPDATE depot SET name = '#' || id, purged_at = ?2, \
                 deleted_at = coalesce(deleted_at, ?2) WHERE id = ?1 AND purged_at IS NULL",
                params![id.0, now],
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
                params![id.0, disabled as i64],
            )?,
            "principal",
        )
    }

    /// Creates a group.
    pub fn create_group(&self, name: &str) -> Result<GroupId> {
        self.conn()
            .execute("INSERT INTO grp (name) VALUES (?1)", [name])
            .map_err(unique("group name"))?;
        Ok(GroupId(self.conn().last_insert_rowid()))
    }

    /// Adds a member; adding an existing member does nothing. Raises the access epoch.
    pub fn add_group_member(&self, group: GroupId, principal: PrincipalId) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO grp_member (grp_id, principal_id) VALUES (?1, ?2)",
            params![group.0, principal.0],
        )?;
        self.bump_access_epoch()
    }

    /// Removes a member; removing a non-member does nothing. Raises the access epoch.
    pub fn remove_group_member(&self, group: GroupId, principal: PrincipalId) -> Result<()> {
        self.conn().execute(
            "DELETE FROM grp_member WHERE grp_id = ?1 AND principal_id = ?2",
            params![group.0, principal.0],
        )?;
        self.bump_access_epoch()
    }

    /// Raises the access epoch by one, in this transaction.
    pub fn bump_access_epoch(&self) -> Result<()> {
        self.conn().execute(
            "UPDATE counter SET value = value + 1 WHERE name = 'access_epoch'",
            [],
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
        self.conn()
            .execute(
                "INSERT INTO token (principal_id, name, hash, scopes, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![principal.0, name, hash, scopes.join(" "), expires_at, now],
            )
            .map_err(unique("token"))?;
        Ok(TokenId(self.conn().last_insert_rowid()))
    }

    /// Records a successful use of a token.
    pub fn touch_token(&self, id: TokenId, now: Millis) -> Result<()> {
        self.conn().execute(
            "UPDATE token SET last_used_at = ?2 WHERE id = ?1",
            params![id.0, now],
        )?;
        Ok(())
    }

    /// Deletes a token.
    pub fn delete_token(&self, id: TokenId) -> Result<()> {
        self.expect_one(
            self.conn()
                .execute("DELETE FROM token WHERE id = ?1", [id.0])?,
            "token",
        )
    }

    /// Deletes every token of a principal.
    pub fn delete_tokens_of(&self, principal: PrincipalId) -> Result<usize> {
        Ok(self
            .conn()
            .execute("DELETE FROM token WHERE principal_id = ?1", [principal.0])?)
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
        self.conn().execute(
            "INSERT INTO acl (depot_id, subject_kind, subject_id, branch_glob, path_glob, effect, level) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![rule.depot_id.map(|d| d.0), kind, sid, rule.branch_glob, rule.path_glob, effect, level],
        )?;
        let id = AclId(self.conn().last_insert_rowid());
        self.bump_access_epoch()?;
        Ok(id)
    }

    /// Deletes an access rule. Raises the access epoch.
    pub fn delete_acl(&self, id: AclId) -> Result<()> {
        self.expect_one(
            self.conn()
                .execute("DELETE FROM acl WHERE id = ?1", [id.0])?,
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
        self.conn()
            .execute(
                "INSERT INTO branch (depot_id, name, head_rev, created_rev, parent_branch_id, parent_rev) \
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                params![
                    depot.0,
                    name,
                    to_i64(created_rev)?,
                    parent.map(|p| p.0 .0),
                    parent.map(|p| to_i64(p.1)).transpose()?
                ],
            )
            .map_err(unique("branch name"))?;
        Ok(BranchId(self.conn().last_insert_rowid()))
    }

    /// Moves a branch head.
    pub fn set_branch_head(&self, branch: BranchId, rev: u64) -> Result<()> {
        self.expect_one(
            self.conn().execute(
                "UPDATE branch SET head_rev = ?2 WHERE id = ?1",
                params![branch.0, to_i64(rev)?],
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
                params![branch.0, s],
            )?,
            "branch",
        )
    }

    /// Inserts a revision row.
    pub fn insert_revision(&self, r: &RevisionRow) -> Result<()> {
        self.conn()
            .execute(
                &format!("INSERT INTO revision ({REVISION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
                params![
                    r.depot_id.0,
                    to_i64(r.num)?,
                    r.branch_id.0,
                    r.parent_num.map(to_i64).transpose()?,
                    r.root_tree.as_bytes().as_slice(),
                    r.author_id.0,
                    r.submitter_id.0,
                    r.created_at,
                    r.message,
                    r.provenance,
                    r.request_id
                ],
            )
            .map_err(unique("revision number or request id"))?;
        Ok(())
    }

    /// Inserts the history-index rows of a revision.
    pub fn insert_changes(&self, depot: DepotId, rev: u64, changes: &[ChangeRow]) -> Result<()> {
        let mut st = self.conn().prepare_cached(&format!(
            "INSERT INTO rev_change (depot_id, rev, {CHANGE_COLS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        ))?;
        let rev = to_i64(rev)?;
        for c in changes {
            st.execute(params![
                depot.0,
                rev,
                c.path,
                c.action.to_string(),
                c.kind.map(i64::from),
                c.old_id.as_ref().map(|i| i.as_bytes().to_vec()),
                c.new_id.as_ref().map(|i| i.as_bytes().to_vec()),
                c.size.map(to_i64).transpose()?,
                i64::from(c.flags),
                c.copy_from_path,
                c.copy_from_rev.map(to_i64).transpose()?
            ])
            .map_err(unique("change path in revision"))?;
        }
        Ok(())
    }

    /// Records a merge on a revision.
    pub fn insert_merge(&self, depot: DepotId, rev: u64, m: &MergeRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO rev_merge (depot_id, rev, source_branch_id, from_rev, to_rev) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
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
            params![depot.0, to_i64(rev)?, key, value],
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
        self.conn().execute(
            "INSERT INTO outbox (depot_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![depot.map(|d| d.0), kind, payload, now],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Moves a subscriber's cursor.
    pub fn set_outbox_cursor(&self, subscriber: &str, last_id: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO outbox_cursor (subscriber, last_id) VALUES (?1, ?2) \
             ON CONFLICT (subscriber) DO UPDATE SET last_id = excluded.last_id",
            params![subscriber, last_id],
        )?;
        Ok(())
    }

    /// Removes a subscriber's cursor.
    pub fn delete_outbox_cursor(&self, subscriber: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM outbox_cursor WHERE subscriber = ?1",
            [subscriber],
        )?;
        Ok(())
    }

    /// Deletes outbox rows with ids up to and including `upto`.
    pub fn prune_outbox(&self, upto: i64) -> Result<usize> {
        Ok(self
            .conn()
            .execute("DELETE FROM outbox WHERE id <= ?1", [upto])?)
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
            params![
                row.depot_id.0,
                row.kind,
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
            params![
                row.depot_id.0,
                row.id.as_bytes().as_slice(),
                to_i64(row.size)?,
                row.path,
                row.reason,
                row.by.0,
                row.at
            ],
        )?;
        Ok(n == 1)
    }
}
