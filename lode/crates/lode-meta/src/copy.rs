//! Copying an SQLite database into an empty Postgres one (`loded migrate-db`, T153).

use crate::sql::{Cell, Sql, Val};
use crate::{vals, Backend, MetaError, MetaStore, Result};

/// Core tables in an order that satisfies their foreign keys. Module tables follow, by name.
const CORE_ORDER: &[&str] = &[
    "depot",
    "principal",
    "grp",
    "grp_member",
    "token",
    "acl",
    "branch",
    "revision",
    "rev_change",
    "rev_merge",
    "rev_meta",
    "outbox",
    "outbox_cursor",
    "counter",
    "obliterated",
    "maintenance_run",
];

/// What [`MetaStore::copy_into`] copied.
#[derive(Debug, Clone, Default)]
pub struct CopyReport {
    /// Each table and its row count, in copy order.
    pub tables: Vec<(String, u64)>,
}

impl CopyReport {
    /// Rows copied in all.
    pub fn rows(&self) -> u64 {
        self.tables.iter().map(|(_, n)| n).sum()
    }
}

fn cell_val(c: Cell) -> Val<'static> {
    match c {
        Cell::Null => Val::Null,
        Cell::Int(v) => Val::Int(v),
        // No core or module table stores floating-point numbers.
        Cell::Real(v) => Val::Text(v.to_string().into()),
        Cell::Text(t) => Val::Text(t.into()),
        Cell::Blob(b) => Val::Blob(b.into()),
    }
}

impl MetaStore {
    /// Copies every table of this SQLite store into `dest`, an empty Postgres store opened
    /// with the same modules, in one Postgres transaction, and checks the row counts. Both
    /// must be idle: run it with the server stopped.
    pub fn copy_into(&self, dest: &MetaStore) -> Result<CopyReport> {
        let Backend::Sqlite(src) = &*self.backend else {
            return Err(MetaError::Setup(
                "the source database must be SQLite".into(),
            ));
        };
        if !matches!(&*dest.backend, Backend::Postgres(_)) {
            return Err(MetaError::Setup(
                "the destination database must be Postgres".into(),
            ));
        }
        let reader = src.readers.take()?;
        let source: &dyn Sql = &reader;
        let mut tables: Vec<String> = source.query(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND name != 'schema_version' ORDER BY name",
            vals![],
            |r| r.get(0),
        )?;
        tables.sort_by_key(|t| {
            (
                CORE_ORDER
                    .iter()
                    .position(|c| c == t)
                    .unwrap_or(CORE_ORDER.len()),
                t.clone(),
            )
        });
        let report = dest.write_blocking(|tx| {
            let db = tx.conn();
            for t in &tables {
                let exists: i64 = db.value(
                    "SELECT count(*) FROM information_schema.tables \
                     WHERE table_schema = current_schema() AND table_name = ?1",
                    vals![t.as_str()],
                )?;
                if exists == 0 {
                    return Err(MetaError::Setup(format!(
                        "table {t} is missing in Postgres; enable the same modules as on the \
                         SQLite server"
                    )));
                }
            }
            for t in ["depot", "principal", "revision"] {
                let n: i64 = db.value(&format!("SELECT count(*) FROM {t}"), vals![])?;
                if n > 0 {
                    return Err(MetaError::Setup(
                        "the Postgres database already holds Lode data; use an empty one".into(),
                    ));
                }
            }
            // Seed rows written by migrations (such as the access epoch) are replaced.
            for t in tables.iter().rev() {
                db.execute(&format!("DELETE FROM {t}"), vals![])?;
            }
            let mut report = CopyReport::default();
            for t in &tables {
                let mut cols: Vec<String> = source.query(
                    &format!("SELECT name FROM pragma_table_info('{t}') ORDER BY cid"),
                    vals![],
                    |r| r.get(0),
                )?;
                let dest_cols: Vec<String> = db.query(
                    "SELECT column_name::text FROM information_schema.columns \
                     WHERE table_schema = current_schema() AND table_name = ?1",
                    vals![t.as_str()],
                    |r| r.get(0),
                )?;
                // rev_merge keeps SQLite's implicit row order in an explicit column.
                if dest_cols.iter().any(|c| c == "rowid") && !cols.iter().any(|c| c == "rowid") {
                    cols.insert(0, "rowid".into());
                }
                let list = cols.join(", ");
                let marks = (1..=cols.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let insert = format!("INSERT INTO {t} ({list}) VALUES ({marks})");
                let rows = source.rows(&format!("SELECT {list} FROM {t}"), vals![])?;
                for row in rows {
                    let args: Vec<Val<'static>> = (0..row.len())
                        .map(|i| row.cell(i).cloned().map(cell_val).unwrap_or(Val::Null))
                        .collect();
                    db.execute(&insert, &args)?;
                }
                let copied: i64 = db.value(&format!("SELECT count(*) FROM {t}"), vals![])?;
                let expected: i64 = source.value(&format!("SELECT count(*) FROM {t}"), vals![])?;
                if copied != expected {
                    return Err(MetaError::Setup(format!(
                        "table {t}: copied {copied} rows of {expected}"
                    )));
                }
                report.tables.push((t.clone(), copied as u64));
            }
            // Identity columns continue after the copied ids.
            let identities: Vec<(String, String)> = db.query(
                "SELECT table_name::text, column_name::text FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND is_identity = 'YES'",
                vals![],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            for (t, c) in identities {
                db.value::<i64>(
                    &format!(
                        "SELECT setval(pg_get_serial_sequence('{t}', '{c}'), \
                         coalesce((SELECT max({c}) FROM {t}), 0) + 1, false)"
                    ),
                    vals![],
                )?;
            }
            Ok(report)
        });
        src.readers.give_back(reader);
        report
    }
}
