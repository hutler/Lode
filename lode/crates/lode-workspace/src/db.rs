//! `.lode/workspace.db`: the schema of ARCHITECTURE §10.1, its migrations and typed access.

use std::path::Path;
use std::time::Duration;

use lode_core::{CopyFrom, DepotPath, EntryKind, ObjectId};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::error::{Result, WorkspaceError};

/// Schema versions, applied in order. `PRAGMA user_version` records how many ran.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema (ARCHITECTURE §10.1).
    "CREATE TABLE config (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
     ) WITHOUT ROWID;
     CREATE TABLE changelist (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL UNIQUE,
        description TEXT NOT NULL DEFAULT ''
     );
     CREATE TABLE file (
        path           TEXT PRIMARY KEY,
        base_id        BLOB,
        kind           INTEGER NOT NULL,
        flags          INTEGER NOT NULL DEFAULT 0,
        size           INTEGER NOT NULL DEFAULT 0,
        mtime_ns       INTEGER NOT NULL DEFAULT 0,
        state          TEXT NOT NULL DEFAULT 'normal'
                       CHECK (state IN ('normal', 'added', 'deleted', 'conflicted')),
        copy_from_path TEXT,
        copy_from_rev  INTEGER,
        changelist_id  INTEGER REFERENCES changelist(id) ON DELETE SET NULL,
        lock_held      INTEGER NOT NULL DEFAULT 0
     ) WITHOUT ROWID;
     CREATE TABLE conflict (
        path       TEXT PRIMARY KEY,
        base_id    BLOB,
        theirs_id  BLOB,
        theirs_rev INTEGER
     ) WITHOUT ROWID;",
    // 2: the local content before a text merge wrote conflict markers (`resolve --mine`).
    "ALTER TABLE conflict ADD COLUMN mine_id BLOB;",
];

/// Newest schema version this build knows.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// `flags` bit: the file is executable.
const FLAG_EXECUTABLE: i64 = 1;

/// Well-known `config` keys.
pub mod keys {
    /// Server URL the workspace talks to.
    pub const SERVER_URL: &str = "server_url";
    /// Depot name.
    pub const DEPOT: &str = "depot";
    /// Branch the workspace follows.
    pub const BRANCH: &str = "branch";
    /// Random id of this workspace.
    pub const WORKSPACE_ID: &str = "workspace_id";
    /// Revision the files were last synced to.
    pub const SYNCED_REV: &str = "synced_rev";
    /// Access epoch seen at the last sync (ARCHITECTURE §6).
    pub const ACCESS_EPOCH: &str = "access_epoch";
    /// The view, as JSON.
    pub const VIEW: &str = "view";
}

/// Tracking state of a file row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileState {
    /// Tracked, at `base_id` unless the disk says otherwise.
    Normal,
    /// Added locally (`lode add`), not committed yet.
    Added,
    /// Marked for deletion (`lode rm`).
    Deleted,
    /// An update or merge left a conflict to resolve.
    Conflicted,
}

impl FileState {
    /// The stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::Normal => "normal",
            FileState::Added => "added",
            FileState::Deleted => "deleted",
            FileState::Conflicted => "conflicted",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "normal" => FileState::Normal,
            "added" => FileState::Added,
            "deleted" => FileState::Deleted,
            "conflicted" => FileState::Conflicted,
            other => return Err(WorkspaceError::Corrupt(format!("file state {other:?}"))),
        })
    }
}

/// One tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Depot path.
    pub path: DepotPath,
    /// Content id at the synced revision; `None` for an add.
    pub base_id: Option<ObjectId>,
    /// `File` or `Symlink`.
    pub kind: EntryKind,
    /// Executable bit at base.
    pub executable: bool,
    /// Size on disk when last known unchanged.
    pub size: u64,
    /// Modification time on disk (ns since the epoch) when last known unchanged; 0 forces a
    /// re-hash on the next scan.
    pub mtime_ns: i64,
    /// Tracking state.
    pub state: FileState,
    /// Copy or rename origin recorded by `mv`/`cp`.
    pub copy_from: Option<CopyFrom>,
    /// Changelist, `None` for the default one.
    pub changelist_id: Option<i64>,
    /// Whether this workspace holds the file's lock.
    pub lock_held: bool,
}

impl FileRow {
    /// A row for a file just synced from the server.
    pub fn synced(path: DepotPath, id: ObjectId, kind: EntryKind, executable: bool) -> Self {
        Self {
            path,
            base_id: Some(id),
            kind,
            executable,
            size: 0,
            mtime_ns: 0,
            state: FileState::Normal,
            copy_from: None,
            changelist_id: None,
            lock_held: false,
        }
    }
}

/// A conflict left by an update or merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRow {
    /// The conflicted path.
    pub path: DepotPath,
    /// Common ancestor content, if any.
    pub base_id: Option<ObjectId>,
    /// Incoming content, if the other side did not delete it.
    pub theirs_id: Option<ObjectId>,
    /// Revision the incoming content came from.
    pub theirs_rev: u64,
    /// The local content before conflict markers were written into the file, kept in
    /// `.lode/conflicts/`; `None` when the local file was left as it was.
    pub mine_id: Option<ObjectId>,
}

/// A named changelist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelistRow {
    /// Id.
    pub id: i64,
    /// Unique name.
    pub name: String,
    /// Free text.
    pub description: String,
}

fn kind_code(k: EntryKind) -> i64 {
    match k {
        EntryKind::File => 1,
        EntryKind::Symlink => 2,
        EntryKind::Dir => 3,
    }
}

fn kind_from(code: i64) -> Result<EntryKind> {
    Ok(match code {
        1 => EntryKind::File,
        2 => EntryKind::Symlink,
        3 => EntryKind::Dir,
        other => return Err(WorkspaceError::Corrupt(format!("file kind {other}"))),
    })
}

fn id_from(blob: Option<Vec<u8>>) -> Result<Option<ObjectId>> {
    blob.map(|b| {
        let bytes: [u8; 32] = b
            .try_into()
            .map_err(|_| WorkspaceError::Corrupt("object id is not 32 bytes".into()))?;
        Ok(ObjectId::from_bytes(bytes))
    })
    .transpose()
}

fn path_from(s: String) -> Result<DepotPath> {
    DepotPath::parse(&s).map_err(|e| WorkspaceError::Corrupt(format!("path {s:?}: {e}")))
}

/// Raw columns of a `file` row, converted after the statement so errors keep their type.
struct RawFile {
    path: String,
    base_id: Option<Vec<u8>>,
    kind: i64,
    flags: i64,
    size: i64,
    mtime_ns: i64,
    state: String,
    copy_from_path: Option<String>,
    copy_from_rev: Option<i64>,
    changelist_id: Option<i64>,
    lock_held: bool,
}

const FILE_COLUMNS: &str = "path, base_id, kind, flags, size, mtime_ns, state, copy_from_path, \
                            copy_from_rev, changelist_id, lock_held";

impl RawFile {
    fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path: r.get(0)?,
            base_id: r.get(1)?,
            kind: r.get(2)?,
            flags: r.get(3)?,
            size: r.get(4)?,
            mtime_ns: r.get(5)?,
            state: r.get(6)?,
            copy_from_path: r.get(7)?,
            copy_from_rev: r.get(8)?,
            changelist_id: r.get(9)?,
            lock_held: r.get(10)?,
        })
    }

    fn convert(self) -> Result<FileRow> {
        let copy_from = match (self.copy_from_path, self.copy_from_rev) {
            (Some(p), Some(rev)) => Some(CopyFrom {
                path: path_from(p)?,
                rev: u64::try_from(rev)
                    .map_err(|_| WorkspaceError::Corrupt("negative copy_from_rev".into()))?,
            }),
            _ => None,
        };
        Ok(FileRow {
            path: path_from(self.path)?,
            base_id: id_from(self.base_id)?,
            kind: kind_from(self.kind)?,
            executable: self.flags & FLAG_EXECUTABLE != 0,
            size: u64::try_from(self.size).unwrap_or(0),
            mtime_ns: self.mtime_ns,
            state: FileState::parse(&self.state)?,
            copy_from,
            changelist_id: self.changelist_id,
            lock_held: self.lock_held,
        })
    }
}

fn size_i64(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

/// The workspace database. One connection; not shared between threads.
#[derive(Debug)]
pub struct WorkspaceDb {
    conn: Connection,
}

impl WorkspaceDb {
    /// Opens (creating if needed) and migrates the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        // WAL lets `status` read and refresh while another command holds the workspace lock.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(WorkspaceError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as u32 + 1)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Runs `f` in one transaction: all of its writes land, or none.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&DbTx<'_>) -> Result<T>) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&DbTx { tx: &tx })?;
        tx.commit()?;
        Ok(out)
    }

    // ---- reads ----

    /// A config value.
    pub fn config(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM config WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// A config value parsed as a number.
    pub fn config_u64(&self, key: &str) -> Result<Option<u64>> {
        self.config(key)?
            .map(|v| {
                v.parse()
                    .map_err(|_| WorkspaceError::Corrupt(format!("config {key} = {v:?}")))
            })
            .transpose()
    }

    /// Every file row, in path order.
    pub fn files(&self) -> Result<Vec<FileRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {FILE_COLUMNS} FROM file ORDER BY path"))?;
        let raws = stmt
            .query_map([], RawFile::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter().map(RawFile::convert).collect()
    }

    /// One file row.
    pub fn file(&self, path: &DepotPath) -> Result<Option<FileRow>> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {FILE_COLUMNS} FROM file WHERE path = ?1"),
                [path.as_str()],
                RawFile::read,
            )
            .optional()?;
        raw.map(RawFile::convert).transpose()
    }

    /// Every conflict.
    pub fn conflicts(&self) -> Result<Vec<ConflictRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, base_id, theirs_id, theirs_rev, mine_id FROM conflict ORDER BY path",
        )?;
        let raws = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<Vec<u8>>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter()
            .map(|(p, b, t, rev, m)| {
                Ok(ConflictRow {
                    path: path_from(p)?,
                    base_id: id_from(b)?,
                    theirs_id: id_from(t)?,
                    theirs_rev: u64::try_from(rev).unwrap_or(0),
                    mine_id: id_from(m)?,
                })
            })
            .collect()
    }

    /// Every changelist.
    pub fn changelists(&self) -> Result<Vec<ChangelistRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description FROM changelist ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ChangelistRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Writes one change outside an explicit transaction.
    pub fn write<T>(&mut self, f: impl FnOnce(&DbTx<'_>) -> Result<T>) -> Result<T> {
        self.transaction(f)
    }
}

/// Writes inside a transaction.
pub struct DbTx<'a> {
    tx: &'a Transaction<'a>,
}

impl DbTx<'_> {
    /// Sets a config value.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.tx.execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Removes a config value.
    pub fn remove_config(&self, key: &str) -> Result<()> {
        self.tx
            .execute("DELETE FROM config WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Inserts or replaces a file row.
    pub fn put_file(&self, row: &FileRow) -> Result<()> {
        let flags = if row.executable { FLAG_EXECUTABLE } else { 0 };
        let (cf_path, cf_rev) = match &row.copy_from {
            Some(c) => (Some(c.path.as_str()), Some(size_i64(c.rev))),
            None => (None, None),
        };
        self.tx.execute(
            &format!(
                "INSERT OR REPLACE INTO file ({FILE_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            ),
            params![
                row.path.as_str(),
                row.base_id.as_ref().map(|id| id.as_bytes().to_vec()),
                kind_code(row.kind),
                flags,
                size_i64(row.size),
                row.mtime_ns,
                row.state.as_str(),
                cf_path,
                cf_rev,
                row.changelist_id,
                row.lock_held,
            ],
        )?;
        Ok(())
    }

    /// Removes a file row. Returns whether there was one.
    pub fn remove_file(&self, path: &DepotPath) -> Result<bool> {
        Ok(self
            .tx
            .execute("DELETE FROM file WHERE path = ?1", [path.as_str()])?
            > 0)
    }

    /// Records the size and mtime a file was last seen unchanged with.
    pub fn set_stat(&self, path: &DepotPath, size: u64, mtime_ns: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE file SET size = ?2, mtime_ns = ?3 WHERE path = ?1",
            params![path.as_str(), size_i64(size), mtime_ns],
        )?;
        Ok(())
    }

    /// Records a conflict.
    pub fn put_conflict(&self, c: &ConflictRow) -> Result<()> {
        self.tx.execute(
            "INSERT OR REPLACE INTO conflict (path, base_id, theirs_id, theirs_rev, mine_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                c.path.as_str(),
                c.base_id.as_ref().map(|id| id.as_bytes().to_vec()),
                c.theirs_id.as_ref().map(|id| id.as_bytes().to_vec()),
                size_i64(c.theirs_rev),
                c.mine_id.as_ref().map(|id| id.as_bytes().to_vec()),
            ],
        )?;
        Ok(())
    }

    /// Clears a conflict.
    pub fn remove_conflict(&self, path: &DepotPath) -> Result<bool> {
        Ok(self
            .tx
            .execute("DELETE FROM conflict WHERE path = ?1", [path.as_str()])?
            > 0)
    }

    /// Creates a changelist and returns its id.
    pub fn create_changelist(&self, name: &str, description: &str) -> Result<i64> {
        self.tx.execute(
            "INSERT INTO changelist (name, description) VALUES (?1, ?2)",
            params![name, description],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// Deletes a changelist; its files fall back to the default one.
    pub fn delete_changelist(&self, id: i64) -> Result<bool> {
        Ok(self
            .tx
            .execute("DELETE FROM changelist WHERE id = ?1", [id])?
            > 0)
    }

    /// Puts a tracked file into a changelist (`None`: the default one). False if the path
    /// is not tracked.
    pub fn set_changelist(&self, path: &DepotPath, id: Option<i64>) -> Result<bool> {
        Ok(self.tx.execute(
            "UPDATE file SET changelist_id = ?2 WHERE path = ?1",
            params![path.as_str(), id],
        )? > 0)
    }

    /// Number of tracked files in a changelist.
    pub fn changelist_size(&self, id: i64) -> Result<u64> {
        let n: i64 = self.tx.query_row(
            "SELECT count(*) FROM file WHERE changelist_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn p(s: &str) -> DepotPath {
        DepotPath::parse(s).unwrap()
    }

    #[test]
    fn rows_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.db");
        let mut db = WorkspaceDb::open(&path).unwrap();
        let id = lode_core::blob_id(&b"x"[..]).unwrap();
        let row = FileRow {
            path: p("a/b.txt"),
            base_id: Some(id),
            kind: EntryKind::Symlink,
            executable: true,
            size: 12,
            mtime_ns: 1_700_000_000_123_456_789,
            state: FileState::Conflicted,
            copy_from: Some(CopyFrom {
                path: p("old.txt"),
                rev: 4,
            }),
            changelist_id: None,
            lock_held: true,
        };
        db.transaction(|tx| {
            tx.put_file(&row)?;
            tx.put_file(&FileRow::synced(p("z.bin"), id, EntryKind::File, false))?;
            tx.set_config(keys::DEPOT, "game")?;
            tx.put_conflict(&ConflictRow {
                path: p("a/b.txt"),
                base_id: None,
                theirs_id: Some(id),
                theirs_rev: 9,
                mine_id: Some(id),
            })
        })
        .unwrap();
        drop(db);
        let mut db = WorkspaceDb::open(&path).unwrap();
        assert_eq!(db.file(&p("a/b.txt")).unwrap().unwrap(), row);
        assert_eq!(db.files().unwrap().len(), 2);
        assert_eq!(db.config(keys::DEPOT).unwrap().as_deref(), Some("game"));
        assert_eq!(db.conflicts().unwrap()[0].theirs_rev, 9);
        db.write(|tx| tx.set_stat(&p("z.bin"), 5, 77)).unwrap();
        let z = db.file(&p("z.bin")).unwrap().unwrap();
        assert_eq!((z.size, z.mtime_ns), (5, 77));

        // Changelists: deleting one returns its files to the default.
        let cl = db.write(|tx| tx.create_changelist("ui", "")).unwrap();
        db.write(|tx| {
            tx.put_file(&FileRow {
                changelist_id: Some(cl),
                ..z.clone()
            })
        })
        .unwrap();
        db.write(|tx| tx.delete_changelist(cl)).unwrap();
        assert_eq!(db.file(&p("z.bin")).unwrap().unwrap().changelist_id, None);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.db");
        drop(WorkspaceDb::open(&path).unwrap());
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(c);
        assert!(matches!(
            WorkspaceDb::open(&path),
            Err(WorkspaceError::SchemaTooNew { .. })
        ));
    }

    #[test]
    fn a_failed_transaction_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = WorkspaceDb::open(&dir.path().join("w.db")).unwrap();
        let r: Result<()> = db.transaction(|tx| {
            tx.set_config("k", "v")?;
            Err(WorkspaceError::InUse)
        });
        assert!(r.is_err());
        assert_eq!(db.config("k").unwrap(), None);
    }
}
