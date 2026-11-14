//! The module's tables: `mod_shelves_shelf` and `mod_shelves_change`.

use lode_core::tree::FLAG_EXECUTABLE;
use lode_core::{Change, ChangeAction, CopyFrom, DepotId, DepotPath, EntryKind, ObjectId};
use lode_meta::{vals, MetaError, Migration, Millis, Result, Row, Sql};

pub(crate) const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_shelves_shelf (
              id            INTEGER PRIMARY KEY,
              depot_id      INTEGER NOT NULL,
              branch_id     INTEGER NOT NULL,
              base_rev      INTEGER NOT NULL,
              owner_id      INTEGER NOT NULL,
              description   TEXT NOT NULL,
              state         TEXT NOT NULL,
              submitted_rev INTEGER,
              provenance    TEXT,
              version       INTEGER NOT NULL DEFAULT 1,
              merged_from   TEXT,
              created_at    INTEGER NOT NULL,
              updated_at    INTEGER NOT NULL
          );
          CREATE INDEX mod_shelves_shelf_depot ON mod_shelves_shelf (depot_id, state);
          CREATE TABLE mod_shelves_change (
              shelf_id       INTEGER NOT NULL,
              path           TEXT NOT NULL,
              action         TEXT NOT NULL,
              kind           TEXT,
              base_id        BLOB,
              new_id         BLOB,
              size           INTEGER,
              flags          INTEGER NOT NULL DEFAULT 0,
              copy_from_path TEXT,
              copy_from_rev  INTEGER,
              PRIMARY KEY (shelf_id, path)
          ) WITHOUT ROWID;",
}];

/// Shelf states as stored.
pub(crate) const OPEN: &str = "open";
pub(crate) const SUBMITTED: &str = "submitted";
pub(crate) const DELETED: &str = "deleted";

/// One row of `mod_shelves_shelf`.
#[derive(Debug, Clone)]
pub(crate) struct ShelfRow {
    pub id: i64,
    pub branch_id: i64,
    pub base_rev: u64,
    pub owner_id: i64,
    pub description: String,
    pub state: String,
    pub submitted_rev: Option<u64>,
    pub provenance: Option<String>,
    pub version: u64,
    pub merged_from: Option<String>,
    pub created_at: Millis,
    pub updated_at: Millis,
}

const SHELF_COLUMNS: &str = "id, branch_id, base_rev, owner_id, description, state, \
     submitted_rev, provenance, version, merged_from, created_at, updated_at";

fn shelf_row(r: &Row) -> Result<ShelfRow> {
    Ok(ShelfRow {
        id: r.get(0)?,
        branch_id: r.get(1)?,
        base_rev: r.get::<i64>(2)? as u64,
        owner_id: r.get(3)?,
        description: r.get(4)?,
        state: r.get(5)?,
        submitted_rev: r.get::<Option<i64>>(6)?.map(|n| n as u64),
        provenance: r.get(7)?,
        version: r.get::<i64>(8)? as u64,
        merged_from: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// One shelf of `depot`.
pub(crate) fn shelf(conn: &dyn Sql, depot: DepotId, id: i64) -> Result<Option<ShelfRow>> {
    conn.query_opt(
        &format!("SELECT {SHELF_COLUMNS} FROM mod_shelves_shelf WHERE depot_id = ?1 AND id = ?2"),
        vals![depot.0, id],
        shelf_row,
    )
}

/// Shelves of `depot`, newest first, filtered.
pub(crate) fn shelves(
    conn: &dyn Sql,
    depot: DepotId,
    branch: Option<i64>,
    owner: Option<i64>,
    state: &str,
) -> Result<Vec<ShelfRow>> {
    conn.query(
        &format!(
            "SELECT {SHELF_COLUMNS} FROM mod_shelves_shelf
             WHERE depot_id = ?1 AND state = ?2
               AND (?3 IS NULL OR branch_id = ?3)
               AND (?4 IS NULL OR owner_id = ?4)
             ORDER BY id DESC"
        ),
        vals![depot.0, state, branch, owner],
        shelf_row,
    )
}

/// What a create or replace writes.
pub(crate) struct NewShelf<'a> {
    pub depot: DepotId,
    pub branch_id: i64,
    pub base_rev: u64,
    pub owner_id: i64,
    pub description: &'a str,
    pub provenance: Option<String>,
    pub merged_from: Option<String>,
    pub changes: &'a [Change],
    pub now: Millis,
}

pub(crate) fn insert_shelf(conn: &dyn Sql, s: &NewShelf<'_>) -> Result<i64> {
    let id: i64 = conn.value(
        "INSERT INTO mod_shelves_shelf (depot_id, branch_id, base_rev, owner_id, description,
             state, provenance, version, merged_from, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, 1, ?7, ?8, ?8) RETURNING id",
        vals![
            s.depot.0,
            s.branch_id,
            s.base_rev as i64,
            s.owner_id,
            s.description,
            &s.provenance,
            &s.merged_from,
            s.now
        ],
    )?;
    insert_changes(conn, id, s.changes)?;
    Ok(id)
}

/// Replaces an open shelf's content and raises its version. False if it is not open.
pub(crate) fn replace_shelf(conn: &dyn Sql, id: i64, s: &NewShelf<'_>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE mod_shelves_shelf
         SET base_rev = ?2, description = ?3, provenance = ?4, merged_from = ?5,
             version = version + 1, updated_at = ?6
         WHERE id = ?1 AND state = 'open'",
        vals![
            id,
            s.base_rev as i64,
            s.description,
            &s.provenance,
            &s.merged_from,
            s.now
        ],
    )?;
    if n == 0 {
        return Ok(false);
    }
    delete_changes(conn, id)?;
    insert_changes(conn, id, s.changes)?;
    Ok(true)
}

/// Marks an open shelf at `version` submitted as `rev`. False if it is not open at that
/// version.
pub(crate) fn mark_submitted(
    conn: &dyn Sql,
    depot: DepotId,
    id: i64,
    version: i64,
    rev: u64,
    now: Millis,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE mod_shelves_shelf
         SET state = 'submitted', submitted_rev = ?4, updated_at = ?5
         WHERE depot_id = ?1 AND id = ?2 AND version = ?3 AND state = 'open'",
        vals![depot.0, id, version, rev as i64, now],
    )?;
    Ok(n > 0)
}

/// Marks an open shelf deleted and drops its changes, so their blobs can be collected.
pub(crate) fn delete_shelf(conn: &dyn Sql, id: i64, now: Millis) -> Result<bool> {
    let n = conn.execute(
        "UPDATE mod_shelves_shelf SET state = 'deleted', updated_at = ?2
         WHERE id = ?1 AND state = 'open'",
        vals![id, now],
    )?;
    if n > 0 {
        delete_changes(conn, id)?;
    }
    Ok(n > 0)
}

fn delete_changes(conn: &dyn Sql, id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM mod_shelves_change WHERE shelf_id = ?1",
        vals![id],
    )?;
    Ok(())
}

fn action_text(a: ChangeAction) -> &'static str {
    match a {
        ChangeAction::Add => "add",
        ChangeAction::Modify => "modify",
        ChangeAction::Delete => "delete",
    }
}

fn kind_text(k: EntryKind) -> &'static str {
    match k {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
    }
}

fn insert_changes(conn: &dyn Sql, shelf: i64, changes: &[Change]) -> Result<()> {
    for c in changes {
        conn.execute(
            "INSERT INTO mod_shelves_change (shelf_id, path, action, kind, base_id, new_id, size,
                 flags, copy_from_path, copy_from_rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            vals![
                shelf,
                c.path.as_str(),
                action_text(c.action),
                c.kind.map(kind_text),
                c.base_id.map(|i| i.as_bytes().to_vec()),
                c.new_id.map(|i| i.as_bytes().to_vec()),
                c.size.map(|s| s as i64),
                i64::from(if c.executable { FLAG_EXECUTABLE } else { 0 }),
                c.copy_from.as_ref().map(|cf| cf.path.as_str().to_owned()),
                c.copy_from.as_ref().map(|cf| cf.rev as i64),
            ],
        )?;
    }
    Ok(())
}

fn bad(i: usize, what: &str) -> MetaError {
    MetaError::BadValue(format!("column {i}: bad stored {what}"))
}

fn change(r: &Row) -> Result<Change> {
    let path: String = r.get(0)?;
    let action: String = r.get(1)?;
    let kind: Option<String> = r.get(2)?;
    let flags: i64 = r.get(6)?;
    let cf_path: Option<String> = r.get(7)?;
    let cf_rev: Option<i64> = r.get(8)?;
    Ok(Change {
        path: DepotPath::parse(&path).map_err(|_| bad(0, "path"))?,
        action: match action.as_str() {
            "add" => ChangeAction::Add,
            "modify" => ChangeAction::Modify,
            "delete" => ChangeAction::Delete,
            _ => return Err(bad(1, "action")),
        },
        base_id: r.get::<Option<ObjectId>>(3)?,
        new_id: r.get::<Option<ObjectId>>(4)?,
        kind: match kind.as_deref() {
            None => None,
            Some("file") => Some(EntryKind::File),
            Some("symlink") => Some(EntryKind::Symlink),
            Some("dir") => Some(EntryKind::Dir),
            Some(_) => return Err(bad(2, "kind")),
        },
        executable: flags & i64::from(FLAG_EXECUTABLE) != 0,
        size: r.get::<Option<i64>>(5)?.map(|s| s as u64),
        copy_from: match (cf_path, cf_rev) {
            (Some(p), Some(rev)) => Some(CopyFrom {
                path: DepotPath::parse(&p).map_err(|_| bad(7, "copy path"))?,
                rev: rev as u64,
            }),
            _ => None,
        },
    })
}

/// The changes of a shelf, by path.
pub(crate) fn changes(conn: &dyn Sql, shelf: i64) -> Result<Vec<Change>> {
    conn.query(
        "SELECT path, action, kind, base_id, new_id, size, flags, copy_from_path, copy_from_rev
         FROM mod_shelves_change WHERE shelf_id = ?1 ORDER BY path",
        vals![shelf],
        change,
    )
}

/// The changed paths of a shelf.
pub(crate) fn paths(conn: &dyn Sql, shelf: i64) -> Result<Vec<String>> {
    conn.query(
        "SELECT path FROM mod_shelves_change WHERE shelf_id = ?1",
        vals![shelf],
        |r| r.get(0),
    )
}

/// Every new blob of the open shelves of `depot`.
pub(crate) fn open_blobs(conn: &dyn Sql, depot: DepotId) -> Result<Vec<ObjectId>> {
    conn.query(
        "SELECT DISTINCT c.new_id FROM mod_shelves_change c
         JOIN mod_shelves_shelf s ON s.id = c.shelf_id
         WHERE s.depot_id = ?1 AND s.state = 'open' AND c.new_id IS NOT NULL",
        vals![depot.0],
        |r| r.get(0),
    )
}
