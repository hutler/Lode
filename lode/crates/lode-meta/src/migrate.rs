//! Embedded, numbered, transactional migrations, tracked per namespace.

use rusqlite::{Connection, TransactionBehavior};

use crate::pg::PgConn;
use crate::sql::Sql;
use crate::vals;

use crate::{MetaError, Result};

/// One schema step.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Position in its set, starting at 1 and without gaps.
    pub version: u32,
    /// Short name for error messages.
    pub name: &'static str,
    /// SQLite SQL, run in one transaction.
    pub sql: &'static str,
    /// The same step for Postgres (T153): `BIGINT` for integers, `BYTEA` for blobs, identity
    /// columns instead of `INTEGER PRIMARY KEY`, `TEXT COLLATE "C"` where the order of text
    /// matters. Empty for a module that does not support Postgres yet; opening a Postgres
    /// database with it enabled then fails.
    pub postgres: &'static str,
}

/// A namespace and its migrations: `core` for the server, the module name for a module,
/// whose tables must be prefixed `mod_<name>_`.
#[derive(Debug, Clone, Copy)]
pub struct MigrationSet {
    /// `core` or a module name.
    pub namespace: &'static str,
    /// Migrations in ascending version order.
    pub migrations: &'static [Migration],
}

/// The core schema.
pub const CORE_MIGRATIONS: MigrationSet = MigrationSet {
    namespace: "core",
    migrations: &[
        Migration {
            version: 1,
            name: "init",
            sql: include_str!("../migrations/core/0001_init.sql"),
            postgres: include_str!("../migrations/core/0001_init.postgres.sql"),
        },
        Migration {
            version: 2,
            name: "access_epoch",
            sql: include_str!("../migrations/core/0002_access_epoch.sql"),
            postgres: include_str!("../migrations/core/0002_access_epoch.postgres.sql"),
        },
        Migration {
            version: 3,
            name: "obliterated",
            sql: include_str!("../migrations/core/0003_obliterated.sql"),
            postgres: include_str!("../migrations/core/0003_obliterated.postgres.sql"),
        },
        Migration {
            version: 4,
            name: "sso",
            sql: include_str!("../migrations/core/0004_sso.sql"),
            postgres: include_str!("../migrations/core/0004_sso.postgres.sql"),
        },
        Migration {
            version: 5,
            name: "maintenance_runs",
            sql: include_str!("../migrations/core/0005_maintenance_runs.sql"),
            postgres: include_str!("../migrations/core/0005_maintenance_runs.postgres.sql"),
        },
        Migration {
            version: 6,
            name: "deleted",
            sql: include_str!("../migrations/core/0006_deleted.sql"),
            postgres: include_str!("../migrations/core/0006_deleted.postgres.sql"),
        },
        Migration {
            version: 7,
            name: "streams",
            sql: include_str!("../migrations/core/0007_streams.sql"),
            postgres: include_str!("../migrations/core/0007_streams.postgres.sql"),
        },
    ],
};

const VERSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_version (
    namespace TEXT PRIMARY KEY,
    version   INTEGER NOT NULL
)";

fn check_numbering(set: &MigrationSet) -> Result<()> {
    for (i, m) in set.migrations.iter().enumerate() {
        if m.version as usize != i + 1 {
            return Err(MetaError::Setup(format!(
                "migrations of {} are not numbered 1, 2, 3, …",
                set.namespace
            )));
        }
    }
    Ok(())
}

/// [`run`] for Postgres. Concurrent servers starting on one database take turns through an
/// advisory lock held for each step's transaction.
pub(crate) fn run_pg(conn: &mut PgConn, set: &MigrationSet) -> Result<()> {
    check_numbering(set)?;
    {
        let tx = conn.begin_write()?;
        tx.batch("SELECT pg_advisory_xact_lock(hashtextextended('lode-migrate', 0))")?;
        tx.batch(VERSION_TABLE)?;
        tx.commit()?;
    }
    let known = set.migrations.len() as u32;
    loop {
        let tx = conn.begin_write()?;
        tx.batch("SELECT pg_advisory_xact_lock(hashtextextended('lode-migrate', 0))")?;
        let current = schema_version(&tx, set.namespace)?;
        if current > known {
            return Err(MetaError::SchemaTooNew {
                namespace: set.namespace.to_owned(),
                found: current,
                known,
            });
        }
        let Some(m) = set.migrations.get(current as usize) else {
            return Ok(());
        };
        let wrap = |source: MetaError| MetaError::Migration {
            namespace: set.namespace.to_owned(),
            version: m.version,
            name: m.name.to_owned(),
            source: Box::new(source),
        };
        if m.postgres.trim().is_empty() {
            return Err(wrap(MetaError::Setup(format!(
                "module {} has no Postgres schema; disable it or use SQLite",
                set.namespace
            ))));
        }
        tx.batch(m.postgres).map_err(wrap)?;
        let db: &dyn Sql = &tx;
        db.execute(
            "INSERT INTO schema_version (namespace, version) VALUES (?1, ?2)
             ON CONFLICT (namespace) DO UPDATE SET version = excluded.version",
            vals![set.namespace, m.version],
        )
        .map_err(wrap)?;
        tx.commit().map_err(wrap)?;
    }
}

/// Brings one namespace up to date. Each migration runs in its own transaction together
/// with the version bump, so a failure leaves the database at the previous version.
pub(crate) fn run(conn: &mut Connection, set: &MigrationSet) -> Result<()> {
    conn.execute_batch(VERSION_TABLE)?;
    check_numbering(set)?;
    let known = set.migrations.len() as u32;
    let current = schema_version(conn, set.namespace)?;
    if current > known {
        return Err(MetaError::SchemaTooNew {
            namespace: set.namespace.to_owned(),
            found: current,
            known,
        });
    }
    for m in &set.migrations[current as usize..] {
        let wrap = |source: MetaError| MetaError::Migration {
            namespace: set.namespace.to_owned(),
            version: m.version,
            name: m.name.to_owned(),
            source: Box::new(source),
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let db: &dyn Sql = &*tx;
        db.batch(m.sql).map_err(wrap)?;
        db.execute(
            "INSERT INTO schema_version (namespace, version) VALUES (?1, ?2)
             ON CONFLICT (namespace) DO UPDATE SET version = excluded.version",
            vals![set.namespace, m.version],
        )
        .map_err(wrap)?;
        tx.commit().map_err(|e| wrap(e.into()))?;
    }
    Ok(())
}

/// The recorded version of a namespace, or 0.
pub fn schema_version(conn: &dyn Sql, namespace: &str) -> Result<u32> {
    Ok(conn
        .query_opt(
            "SELECT version FROM schema_version WHERE namespace = ?1",
            vals![namespace],
            |r| r.get(0),
        )?
        .unwrap_or(0))
}
