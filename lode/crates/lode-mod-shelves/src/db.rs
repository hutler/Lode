//! The module's tables: `mod_shelves_shelf` and `mod_shelves_change`.

use lode_core::tree::FLAG_EXECUTABLE;
use lode_core::{Change, ChangeAction, CopyFrom, DepotId, DepotPath, EntryKind, ObjectId};
use lode_meta::{Migration, Millis};
use rusqlite::{params, Connection, OptionalExtension, Row};

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

fn shelf_row(r: &Row<'_>) -> rusqlite::Result<ShelfRow> {
    Ok(ShelfRow {
        id: r.get(0)?,
        branch_id: r.get(1)?,
        base_rev: r.get::<_, i64>(2)? as u64,
        owner_id: r.get(3)?,
        description: r.get(4)?,
        state: r.get(5)?,
        submitted_rev: r.get::<_, Option<i64>>(6)?.map(|n| n as u64),
        provenance: r.get(7)?,
        version: r.get::<_, i64>(8)? as u64,
        merged_from: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// One shelf of `depot`.
pub(crate) fn shelf(
    conn: &Connection,
    depot: DepotId,
    id: i64,
) -> rusqlite::Result<Option<ShelfRow>> {
    conn.prepare_cached(&format!(
        "SELECT {SHELF_COLUMNS} FROM mod_shelves_shelf WHERE depot_id = ?1 AND id = ?2"
    ))?
    .query_row(params![depot.0, id], shelf_row)
    .optional()
}

/// Shelves of `depot`, newest first, filtered.
pub(crate) fn shelves(
    conn: &Connection,
    depot: DepotId,
    branch: Option<i64>,
    owner: Option<i64>,
    state: &str,
) -> rusqlite::Result<Vec<ShelfRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SHELF_COLUMNS} FROM mod_shelves_shelf
         WHERE depot_id = ?1 AND state = ?2
           AND (?3 IS NULL OR branch_id = ?3)
           AND (?4 IS NULL OR owner_id = ?4)
         ORDER BY id DESC"
    ))?;
    let rows = stmt.query_map(params![depot.0, state, branch, owner], shelf_row)?;
    rows.collect()
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

pub(crate) fn insert_shelf(conn: &Connection, s: &NewShelf<'_>) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO mod_shelves_shelf (depot_id, branch_id, base_rev, owner_id, description,
             state, provenance, version, merged_from, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, 1, ?7, ?8, ?8)",
    )?
    .execute(params![
        s.depot.0,
        s.branch_id,
        s.base_rev as i64,
        s.owner_id,
        s.description,
        s.provenance,
        s.merged_from,
        s.now
    ])?;
    let id = conn.last_insert_rowid();
    insert_changes(conn, id, s.changes)?;
    Ok(id)
}

/// Replaces an open shelf's content and raises its version. False if it is not open.
pub(crate) fn replace_shelf(
    conn: &Connection,
    id: i64,
    s: &NewShelf<'_>,
) -> rusqlite::Result<bool> {
    let n = conn
        .prepare_cached(
            "UPDATE mod_shelves_shelf
             SET base_rev = ?2, description = ?3, provenance = ?4, merged_from = ?5,
                 version = version + 1, updated_at = ?6
             WHERE id = ?1 AND state = 'open'",
        )?
        .execute(params![
            id,
            s.base_rev as i64,
            s.description,
            s.provenance,
            s.merged_from,
            s.now
        ])?;
    if n == 0 {
        return Ok(false);
    }
    delete_changes(conn, id)?;
    insert_changes(conn, id, s.changes)?;
    Ok(true)
}

/// Marks an open shelf deleted and drops its changes, so their blobs can be collected.
pub(crate) fn delete_shelf(conn: &Connection, id: i64, now: Millis) -> rusqlite::Result<bool> {
    let n = conn
        .prepare_cached(
            "UPDATE mod_shelves_shelf SET state = 'deleted', updated_at = ?2
             WHERE id = ?1 AND state = 'open'",
        )?
        .execute(params![id, now])?;
    if n > 0 {
        delete_changes(conn, id)?;
    }
    Ok(n > 0)
}

fn delete_changes(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("DELETE FROM mod_shelves_change WHERE shelf_id = ?1")?
        .execute([id])?;
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

fn insert_changes(conn: &Connection, shelf: i64, changes: &[Change]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO mod_shelves_change (shelf_id, path, action, kind, base_id, new_id, size,
             flags, copy_from_path, copy_from_rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for c in changes {
        stmt.execute(params![
            shelf,
            c.path.as_str(),
            action_text(c.action),
            c.kind.map(kind_text),
            c.base_id.map(|i| i.as_bytes().to_vec()),
            c.new_id.map(|i| i.as_bytes().to_vec()),
            c.size.map(|s| s as i64),
            if c.executable { FLAG_EXECUTABLE } else { 0 },
            c.copy_from.as_ref().map(|cf| cf.path.as_str().to_owned()),
            c.copy_from.as_ref().map(|cf| cf.rev as i64),
        ])?;
    }
    Ok(())
}

fn bad(i: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        i,
        rusqlite::types::Type::Text,
        format!("bad stored {what}").into(),
    )
}

fn get_id(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<ObjectId>> {
    let v: Option<Vec<u8>> = r.get(i)?;
    v.map(|b| {
        <[u8; 32]>::try_from(b.as_slice())
            .map(ObjectId::from_bytes)
            .map_err(|_| bad(i, "id"))
    })
    .transpose()
}

fn change(r: &Row<'_>) -> rusqlite::Result<Change> {
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
        base_id: get_id(r, 3)?,
        new_id: get_id(r, 4)?,
        kind: match kind.as_deref() {
            None => None,
            Some("file") => Some(EntryKind::File),
            Some("symlink") => Some(EntryKind::Symlink),
            Some("dir") => Some(EntryKind::Dir),
            Some(_) => return Err(bad(2, "kind")),
        },
        executable: flags & i64::from(FLAG_EXECUTABLE) != 0,
        size: r.get::<_, Option<i64>>(5)?.map(|s| s as u64),
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
pub(crate) fn changes(conn: &Connection, shelf: i64) -> rusqlite::Result<Vec<Change>> {
    let mut stmt = conn.prepare_cached(
        "SELECT path, action, kind, base_id, new_id, size, flags, copy_from_path, copy_from_rev
         FROM mod_shelves_change WHERE shelf_id = ?1 ORDER BY path",
    )?;
    let rows = stmt.query_map([shelf], change)?;
    rows.collect()
}

/// The changed paths of a shelf.
pub(crate) fn paths(conn: &Connection, shelf: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT path FROM mod_shelves_change WHERE shelf_id = ?1")?;
    let rows = stmt.query_map([shelf], |r| r.get(0))?;
    rows.collect()
}

/// Every new blob of the open shelves of `depot`.
pub(crate) fn open_blobs(conn: &Connection, depot: DepotId) -> rusqlite::Result<Vec<ObjectId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT c.new_id FROM mod_shelves_change c
         JOIN mod_shelves_shelf s ON s.id = c.shelf_id
         WHERE s.depot_id = ?1 AND s.state = 'open' AND c.new_id IS NOT NULL",
    )?;
    let rows = stmt.query_map([depot.0], |r| get_id(r, 0))?;
    let mut out = Vec::new();
    for id in rows {
        if let Some(id) = id? {
            out.push(id);
        }
    }
    Ok(out)
}
