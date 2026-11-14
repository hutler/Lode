//! SQLite metadata store for the Lode server (ARCHITECTURE §4).
//!
//! One writer connection behind a mutex serializes every write, which is what makes commits
//! atomic. Reads use a small pool of read-only connections and run concurrently with the
//! writer thanks to WAL mode. The async methods run their closure on the blocking thread
//! pool; the closure gets a transaction and must not do network or blob I/O.

#![forbid(unsafe_code)]

mod error;
mod migrate;
mod queries;
mod rows;
mod sql;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use tokio::sync::Semaphore;

pub use error::{MetaError, Result};
pub use migrate::{schema_version, Migration, MigrationSet, CORE_MIGRATIONS};
pub use queries::{in_chain, Reads};
pub use rows::*;
pub use sql::{to_postgres_params, Cell, Dialect, FromCell, Row, Sql, Val};

/// Settings for [`MetaStore::open`].
#[derive(Debug, Clone)]
pub struct MetaConfig {
    /// How long a connection waits on a locked database before failing.
    pub busy_timeout: Duration,
    /// Maximum number of read connections.
    pub read_connections: usize,
}

impl Default for MetaConfig {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(10),
            read_connections: 8,
        }
    }
}

struct ReadPool {
    path: PathBuf,
    busy_timeout: Duration,
    idle: Mutex<Vec<Connection>>,
    permits: Semaphore,
}

impl ReadPool {
    fn take(&self) -> Result<Connection> {
        let idle = self.idle.lock().map_err(|_| MetaError::Poisoned)?.pop();
        match idle {
            Some(c) => Ok(c),
            None => open_reader(&self.path, self.busy_timeout),
        }
    }

    fn give_back(&self, c: Connection) {
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(c);
        }
    }
}

/// The metadata database.
#[derive(Clone)]
pub struct MetaStore {
    writer: Arc<Mutex<Connection>>,
    readers: Arc<ReadPool>,
}

fn open_writer(path: &Path, busy_timeout: Duration) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(busy_timeout)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(MetaError::Setup(format!(
            "journal_mode is {mode}, expected wal"
        )));
    }
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")?;
    Ok(conn)
}

fn open_reader(path: &Path, busy_timeout: Duration) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
    Ok(conn)
}

impl MetaStore {
    /// Opens or creates the database at `path` and brings the core schema and every given
    /// module migration set up to date. Fails if the file has a newer schema than this build
    /// knows, for the core or for any given module.
    pub fn open(
        path: impl Into<PathBuf>,
        modules: &[MigrationSet],
        config: MetaConfig,
    ) -> Result<Self> {
        let path = path.into();
        let mut writer = open_writer(&path, config.busy_timeout)?;
        migrate::run(&mut writer, &CORE_MIGRATIONS)?;
        for set in modules {
            migrate::run(&mut writer, set)?;
        }
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            readers: Arc::new(ReadPool {
                path,
                busy_timeout: config.busy_timeout,
                idle: Mutex::new(Vec::new()),
                permits: Semaphore::new(config.read_connections.max(1)),
            }),
        })
    }

    /// Runs `f` in a read transaction that sees one consistent snapshot.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTx<'_>) -> Result<T> + Send + 'static,
    {
        let _permit = self
            .readers
            .permits
            .acquire()
            .await
            .map_err(|_| MetaError::Poisoned)?;
        let pool = self.readers.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.take()?;
            let out = {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
                let rtx = ReadTx { tx };
                let out = f(&rtx);
                // Ending a read transaction cannot lose data; ignore rollback errors.
                let _ = rtx.tx.rollback();
                out
            };
            pool.give_back(conn);
            out
        })
        .await
        .map_err(|e| MetaError::Setup(format!("read task failed: {e}")))?
    }

    /// Runs `f` in a write transaction, committing if it returns `Ok` and rolling back
    /// otherwise. Writes are serialized.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTx<'_>) -> Result<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.write_blocking(f))
            .await
            .map_err(|e| MetaError::Setup(format!("write task failed: {e}")))?
    }

    /// [`MetaStore::write`] on the calling thread: runs `f` in a write transaction,
    /// committing if it returns `Ok` and rolling back otherwise.
    pub fn write_blocking<T>(&self, f: impl FnOnce(&WriteTx<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self.writer.lock().map_err(|_| MetaError::Poisoned)?;
        let tx = WriteTx::begin(&mut guard)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Gives `f` the writer connection on the calling thread, for code that manages its own
    /// transaction (the commit engine, tests). Blocks other writers until `f` returns.
    pub fn with_writer<T>(&self, f: impl FnOnce(&dyn Sql) -> Result<T>) -> Result<T> {
        let guard = self.writer.lock().map_err(|_| MetaError::Poisoned)?;
        f(&*guard)
    }

    /// Copies the live database to `dest` with SQLite's online backup API. Readers and the
    /// writer keep working during the copy.
    pub async fn backup_to(&self, dest: impl Into<PathBuf>) -> Result<()> {
        let dest = dest.into();
        let pool = self.readers.clone();
        tokio::task::spawn_blocking(move || {
            let src = pool.take()?;
            let mut dst = Connection::open(&dest)?;
            {
                let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
                backup.run_to_completion(256, Duration::from_millis(10), None)?;
            }
            pool.give_back(src);
            Ok(())
        })
        .await
        .map_err(|e| MetaError::Setup(format!("backup task failed: {e}")))?
    }
}

/// A read transaction.
pub struct ReadTx<'c> {
    tx: rusqlite::Transaction<'c>,
}

/// A write transaction. Dropping it without [`WriteTx::commit`] rolls everything back.
pub struct WriteTx<'c> {
    tx: rusqlite::Transaction<'c>,
}

impl<'c> WriteTx<'c> {
    /// Starts an immediate (write-locking) transaction on the writer connection.
    pub(crate) fn begin(conn: &'c mut Connection) -> Result<Self> {
        Ok(Self {
            tx: conn.transaction_with_behavior(TransactionBehavior::Immediate)?,
        })
    }

    /// Commits.
    pub fn commit(self) -> Result<()> {
        Ok(self.tx.commit()?)
    }

    /// Raw connection access for module tables (`mod_<name>_*`). Core tables must be changed
    /// only through the typed methods.
    pub fn conn(&self) -> &dyn Sql {
        &*self.tx
    }
}

impl ReadTx<'_> {
    /// Raw connection access for module tables.
    pub fn conn(&self) -> &dyn Sql {
        &*self.tx
    }
}
