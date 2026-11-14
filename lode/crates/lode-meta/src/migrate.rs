//! Embedded, numbered, transactional migrations, tracked per namespace.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{MetaError, Result};

/// One schema step.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Position in its set, starting at 1 and without gaps.
    pub version: u32,
    /// Short name for error messages.
    pub name: &'static str,
    /// SQL run in one transaction.
    pub sql: &'static str,
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
        },
        Migration {
            version: 2,
            name: "access_epoch",
            sql: include_str!("../migrations/core/0002_access_epoch.sql"),
        },
        Migration {
            version: 3,
            name: "obliterated",
            sql: include_str!("../migrations/core/0003_obliterated.sql"),
        },
    ],
};

const VERSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_version (
    namespace TEXT PRIMARY KEY,
    version   INTEGER NOT NULL
)";

/// Brings one namespace up to date. Each migration runs in its own transaction together
/// with the version bump, so a failure leaves the database at the previous version.
pub(crate) fn run(conn: &mut Connection, set: &MigrationSet) -> Result<()> {
    conn.execute_batch(VERSION_TABLE)?;
    for (i, m) in set.migrations.iter().enumerate() {
        if m.version as usize != i + 1 {
            return Err(MetaError::Setup(format!(
                "migrations of {} are not numbered 1, 2, 3, …",
                set.namespace
            )));
        }
    }
    let known = set.migrations.len() as u32;
    let current: u32 = conn
        .query_row(
            "SELECT version FROM schema_version WHERE namespace = ?1",
            [set.namespace],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if current > known {
        return Err(MetaError::SchemaTooNew {
            namespace: set.namespace.to_owned(),
            found: current,
            known,
        });
    }
    for m in &set.migrations[current as usize..] {
        let wrap = |source| MetaError::Migration {
            namespace: set.namespace.to_owned(),
            version: m.version,
            name: m.name.to_owned(),
            source,
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql).map_err(wrap)?;
        tx.execute(
            "INSERT INTO schema_version (namespace, version) VALUES (?1, ?2)
             ON CONFLICT (namespace) DO UPDATE SET version = excluded.version",
            params![set.namespace, m.version],
        )
        .map_err(wrap)?;
        tx.commit().map_err(wrap)?;
    }
    Ok(())
}

/// The recorded version of a namespace, or 0.
pub fn schema_version(conn: &Connection, namespace: &str) -> Result<u32> {
    Ok(conn
        .query_row(
            "SELECT version FROM schema_version WHERE namespace = ?1",
            [namespace],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}
