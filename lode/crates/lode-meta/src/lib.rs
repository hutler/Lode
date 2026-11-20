//! Metadata store for the Lode server (ARCHITECTURE §4), in SQLite or Postgres.
//!
//! One writer connection behind a mutex serializes every write, which is what makes commits
//! atomic. Reads use a small pool of read-only connections and run concurrently with the
//! writer (WAL mode in SQLite, snapshot transactions in Postgres). The async methods run their closure on the blocking thread
//! pool; the closure gets a transaction and must not do network or blob I/O.

#![forbid(unsafe_code)]

mod copy;
mod error;
mod lock;
mod migrate;
mod pg;
mod queries;
mod rows;
mod sql;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use tokio::sync::Semaphore;

use pg::{PgConn, PgTarget, PgTx};

pub use copy::CopyReport;
pub use error::{MetaError, Result};
pub use lock::{LockAttempt, ServerLock};
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

struct SqliteBackend {
    writer: Mutex<Connection>,
    readers: ReadPool,
}

struct PgBackend {
    target: PgTarget,
    writer: Mutex<Option<PgConn>>,
    idle: Mutex<Vec<PgConn>>,
}

impl PgBackend {
    fn take(&self) -> Result<PgConn> {
        loop {
            let idle = self.idle.lock().map_err(|_| MetaError::Poisoned)?.pop();
            match idle {
                Some(c) if c.is_closed() => continue,
                Some(c) => return Ok(c),
                None => return self.target.connect(),
            }
        }
    }

    fn give_back(&self, c: PgConn) {
        if c.is_closed() {
            return;
        }
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(c);
        }
    }
}

enum Backend {
    Sqlite(SqliteBackend),
    Postgres(Box<PgBackend>),
}

/// Where the metadata lives.
#[derive(Clone, PartialEq, Eq)]
pub enum Database {
    /// An SQLite file.
    Sqlite(PathBuf),
    /// A Postgres database, by `postgres://` URL.
    Postgres(String),
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Database::Sqlite(p) => write!(f, "{}", p.display()),
            Database::Postgres(url) => f.write_str(&pg::hide_password(url)),
        }
    }
}

/// What the admin overview shows about the database.
#[derive(Debug, Clone)]
pub struct DatabaseInfo {
    /// `SQLite` or `Postgres`.
    pub kind: &'static str,
    /// The database software's version.
    pub version: String,
    /// The file or URL, without any password.
    pub location: String,
    /// Size of the database file, for SQLite.
    pub file_size: Option<u64>,
}

/// The metadata database.
#[derive(Clone)]
pub struct MetaStore {
    backend: Arc<Backend>,
    permits: Arc<Semaphore>,
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
    /// Opens or creates the SQLite database at `path` and brings the core schema and every
    /// given module migration set up to date. Fails if the file has a newer schema than this
    /// build knows, for the core or for any given module.
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
            backend: Arc::new(Backend::Sqlite(SqliteBackend {
                writer: Mutex::new(writer),
                readers: ReadPool {
                    path,
                    busy_timeout: config.busy_timeout,
                    idle: Mutex::new(Vec::new()),
                },
            })),
            permits: Arc::new(Semaphore::new(config.read_connections.max(1))),
        })
    }

    /// Connects to a Postgres database by URL and brings its schema up to date, like
    /// [`MetaStore::open`].
    pub fn open_postgres(url: &str, modules: &[MigrationSet], config: MetaConfig) -> Result<Self> {
        let target = PgTarget::parse(url)?;
        let mut writer = target.connect()?;
        migrate::run_pg(&mut writer, &CORE_MIGRATIONS)?;
        for set in modules {
            migrate::run_pg(&mut writer, set)?;
        }
        Ok(Self {
            backend: Arc::new(Backend::Postgres(Box::new(PgBackend {
                target,
                writer: Mutex::new(Some(writer)),
                idle: Mutex::new(Vec::new()),
            }))),
            permits: Arc::new(Semaphore::new(config.read_connections.max(1))),
        })
    }

    /// Opens either kind of database.
    pub fn open_database(
        db: &Database,
        modules: &[MigrationSet],
        config: MetaConfig,
    ) -> Result<Self> {
        match db {
            Database::Sqlite(path) => Self::open(path, modules, config),
            Database::Postgres(url) => Self::open_postgres(url, modules, config),
        }
    }

    /// Which database this is.
    pub fn dialect(&self) -> Dialect {
        match &*self.backend {
            Backend::Sqlite(_) => Dialect::Sqlite,
            Backend::Postgres(_) => Dialect::Postgres,
        }
    }

    /// Kind, version and location of the database.
    pub async fn info(&self) -> Result<DatabaseInfo> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || match &*store.backend {
            Backend::Sqlite(b) => {
                let conn = b.readers.take()?;
                let version: String =
                    conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
                let path = b.readers.path.clone();
                b.readers.give_back(conn);
                Ok(DatabaseInfo {
                    kind: "SQLite",
                    version,
                    location: path.display().to_string(),
                    file_size: std::fs::metadata(&path).ok().map(|m| m.len()),
                })
            }
            Backend::Postgres(b) => {
                let mut conn = b.take()?;
                let version = pg::wait(conn.client()?.query_one("SHOW server_version", &[]))?
                    .try_get::<_, String>(0)?;
                b.give_back(conn);
                Ok(DatabaseInfo {
                    kind: "Postgres",
                    version,
                    location: b.target.shown.clone(),
                    file_size: None,
                })
            }
        })
        .await
        .map_err(|e| MetaError::Setup(format!("info task failed: {e}")))?
    }

    /// Runs `f` in a read transaction that sees one consistent snapshot.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTx<'_>) -> Result<T> + Send + 'static,
    {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| MetaError::Poisoned)?;
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || match &*backend {
            Backend::Sqlite(b) => {
                let mut conn = b.readers.take()?;
                let out = {
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
                    let rtx = ReadTx { tx: Tx::Sqlite(tx) };
                    let out = f(&rtx);
                    // Ending a read transaction cannot lose data; ignore rollback errors.
                    if let Tx::Sqlite(tx) = rtx.tx {
                        let _ = tx.rollback();
                    }
                    out
                };
                b.readers.give_back(conn);
                out
            }
            Backend::Postgres(b) => {
                let mut conn = b.take()?;
                let out = {
                    let rtx = ReadTx {
                        tx: Tx::Pg(conn.begin_read()?),
                    };
                    let out = f(&rtx);
                    if let Tx::Pg(tx) = rtx.tx {
                        // Read-only: committing ends it without effect.
                        let _ = tx.commit();
                    }
                    out
                };
                b.give_back(conn);
                out
            }
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
        match &*self.backend {
            Backend::Sqlite(b) => {
                let mut guard = b.writer.lock().map_err(|_| MetaError::Poisoned)?;
                let tx = WriteTx {
                    tx: Tx::Sqlite(
                        guard.transaction_with_behavior(TransactionBehavior::Immediate)?,
                    ),
                };
                let out = f(&tx)?;
                tx.commit()?;
                Ok(out)
            }
            Backend::Postgres(b) => {
                let mut guard = b.writer.lock().map_err(|_| MetaError::Poisoned)?;
                if guard.as_ref().is_none_or(PgConn::is_closed) {
                    *guard = Some(b.target.connect()?);
                }
                let conn = guard
                    .as_mut()
                    .ok_or_else(|| MetaError::Setup("no database connection".into()))?;
                let tx = WriteTx {
                    tx: Tx::Pg(conn.begin_write()?),
                };
                let out = f(&tx)?;
                tx.commit()?;
                Ok(out)
            }
        }
    }

    /// Gives `f` the writer connection on the calling thread, inside a transaction that
    /// commits when `f` returns `Ok`, for code that needs raw statements (tests, tools).
    /// Blocks other writers until `f` returns.
    pub fn with_writer<T>(&self, f: impl FnOnce(&dyn Sql) -> Result<T>) -> Result<T> {
        self.write_blocking(|tx| f(tx.conn()))
    }

    /// Copies the live SQLite database to `dest` with SQLite's online backup API. Readers
    /// and the writer keep working during the copy. A Postgres database is backed up with
    /// `pg_dump` instead; this returns an error for one.
    pub async fn backup_to(&self, dest: impl Into<PathBuf>) -> Result<()> {
        let dest = dest.into();
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || {
            let Backend::Sqlite(b) = &*backend else {
                return Err(MetaError::Setup(
                    "a Postgres database is backed up with pg_dump".into(),
                ));
            };
            let src = b.readers.take()?;
            let mut dst = Connection::open(&dest)?;
            {
                let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
                backup.run_to_completion(256, Duration::from_millis(10), None)?;
            }
            b.readers.give_back(src);
            Ok(())
        })
        .await
        .map_err(|e| MetaError::Setup(format!("backup task failed: {e}")))?
    }
}

enum Tx<'c> {
    Sqlite(rusqlite::Transaction<'c>),
    Pg(PgTx<'c>),
}

impl Tx<'_> {
    fn sql(&self) -> &dyn Sql {
        match self {
            Tx::Sqlite(t) => &**t,
            Tx::Pg(t) => t,
        }
    }
}

/// A read transaction.
pub struct ReadTx<'c> {
    tx: Tx<'c>,
}

/// A write transaction. Dropping it without [`WriteTx::commit`] rolls everything back.
pub struct WriteTx<'c> {
    tx: Tx<'c>,
}

impl WriteTx<'_> {
    /// Commits.
    pub fn commit(self) -> Result<()> {
        match self.tx {
            Tx::Sqlite(t) => Ok(t.commit()?),
            Tx::Pg(t) => t.commit(),
        }
    }

    /// Raw connection access for module tables (`mod_<name>_*`). Core tables must be changed
    /// only through the typed methods.
    pub fn conn(&self) -> &dyn Sql {
        self.tx.sql()
    }
}

impl ReadTx<'_> {
    /// Raw connection access for module tables.
    pub fn conn(&self) -> &dyn Sql {
        self.tx.sql()
    }
}

/// Test support: a fresh, empty Postgres schema on the server named by the
/// `LODE_TEST_POSTGRES` URL, returned as a URL whose sessions use that schema. `None` when the
/// variable is not set, so tests that need Postgres skip and suites run on SQLite.
pub fn test_postgres_url() -> Option<Result<String>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::var("LODE_TEST_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let schema = format!(
        "lode_t_{}_{}_{}",
        std::process::id(),
        stamp,
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    let made = (|| {
        let mut conn = PgTarget::parse(&base)?.connect()?;
        pg::wait(
            conn.client()?
                .batch_execute(&format!("CREATE SCHEMA {schema}")),
        )?;
        let sep = if base.contains('?') { '&' } else { '?' };
        Ok(format!("{base}{sep}options=-csearch_path%3D{schema}"))
    })();
    Some(made)
}
