//! The workspace lock: one mutating `lode` process at a time.
//!
//! The lock is an exclusive SQLite lock on `.lode/lock`, held by an open transaction. SQLite
//! takes it with the platform's file locks (`fcntl` on Unix, `LockFileEx` on Windows), which
//! the operating system drops when the process exits, so a killed `lode` never leaves a stale
//! lock behind, and no `unsafe` platform call is needed here. Commands that only read (such
//! as `status`) do not take it and may run alongside.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode};

use crate::error::{Result, WorkspaceError};

/// Held while a command changes the workspace; released on drop.
#[derive(Debug)]
pub struct WorkspaceLock {
    // Dropping the connection rolls the transaction back and releases the file lock.
    _conn: Connection,
}

fn busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

impl WorkspaceLock {
    /// Takes the lock at `path`, failing at once with [`WorkspaceError::InUse`] if another
    /// process (or another lock in this one) holds it.
    pub fn acquire(path: &Path) -> Result<Self> {
        let map = |e: rusqlite::Error| {
            if busy(&e) {
                WorkspaceError::InUse
            } else {
                WorkspaceError::Db(e)
            }
        };
        let conn = Connection::open(path).map_err(map)?;
        conn.busy_timeout(Duration::ZERO).map_err(map)?;
        // A rollback journal, so `BEGIN EXCLUSIVE` really takes the exclusive file lock (in
        // WAL mode it would not keep readers out, which does not matter here, but it also
        // would not keep a second writer from starting until its first write).
        conn.pragma_update(None, "journal_mode", "DELETE")
            .map_err(map)?;
        conn.execute_batch("BEGIN EXCLUSIVE").map_err(map)?;
        Ok(Self { _conn: conn })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn second_holder_fails_fast_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = WorkspaceLock::acquire(&path).unwrap();
        let start = std::time::Instant::now();
        assert!(matches!(
            WorkspaceLock::acquire(&path),
            Err(WorkspaceError::InUse)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(first);
        let _again = WorkspaceLock::acquire(&path).unwrap();
    }
}
