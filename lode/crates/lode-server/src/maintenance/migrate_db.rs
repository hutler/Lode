//! `loded migrate-db --to postgres` (T153): copies the SQLite metadata into an empty Postgres
//! database in one transaction, checks the row counts, then points `loded.toml` at Postgres,
//! keeping the old file as `loded.toml.before-postgres`. Run it with the server stopped;
//! `meta.db` is left in place.

use std::io;
use std::path::{Path, PathBuf};

use lode_meta::{CopyReport, Database, MetaConfig, MetaStore, MigrationSet};

use crate::config::{DatabaseKind, ServerConfig, CONFIG_FILE_NAME};
use crate::module::Modules;
use crate::server::{database_of, ServerError};

/// What `migrate-db` did.
#[derive(Debug, Clone, Default)]
pub struct MigrateDbReport {
    /// Tables and rows copied.
    pub copy: CopyReport,
    /// The configuration file, now pointing at Postgres.
    pub switched: PathBuf,
}

/// Why `migrate-db` stopped.
#[derive(Debug, thiserror::Error)]
pub enum MigrateDbError {
    /// The server already uses Postgres.
    #[error("the server already keeps its metadata in Postgres ([database] kind = \"postgres\")")]
    AlreadyPostgres,
    /// The URL file is unusable.
    #[error("{0}")]
    UrlFile(String),
    /// Opening or copying failed.
    #[error(transparent)]
    Server(#[from] ServerError),
    /// The database refused.
    #[error("{0}")]
    Meta(#[from] lode_meta::MetaError),
    /// The configuration could not be switched.
    #[error("{0}")]
    Config(String),
    /// A file could not be read or written.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        source: io::Error,
    },
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> MigrateDbError {
    let context = context.into();
    move |source| MigrateDbError::Io { context, source }
}

/// Copies the metadata of the server configured at `config_path` into the Postgres database
/// whose URL is the first line of `url_file`, then switches the configuration. `modules` are
/// the modules compiled into this build; the enabled ones' tables are copied.
pub async fn migrate_db_to_postgres(
    config_path: &Path,
    url_file: &Path,
    modules: Modules,
) -> Result<MigrateDbReport, MigrateDbError> {
    let config =
        ServerConfig::load(config_path).map_err(|e| MigrateDbError::Config(e.to_string()))?;
    if config.database.kind == DatabaseKind::Postgres {
        return Err(MigrateDbError::AlreadyPostgres);
    }
    let Database::Sqlite(db_file) = database_of(&config)? else {
        return Err(MigrateDbError::AlreadyPostgres);
    };
    if !db_file.is_file() {
        return Err(ServerError::NotInitialized(config.data_dir.clone()).into());
    }
    let url_file = std::path::absolute(url_file)
        .map_err(io_err(format!("cannot resolve {}", url_file.display())))?;
    let url = crate::config::DatabaseConfig::postgres_file(&url_file)
        .postgres_url()
        .map_err(MigrateDbError::UrlFile)?;
    let enabled = crate::module::select_enabled(&config.modules, modules);
    let sets: Vec<MigrationSet> = enabled
        .iter()
        .map(|m| MigrationSet {
            namespace: m.name(),
            migrations: m.migrations(),
        })
        .collect();
    let copy = tokio::task::spawn_blocking(move || {
        let src = MetaStore::open(&db_file, &sets, MetaConfig::default())?;
        let dst = MetaStore::open_postgres(&url, &sets, MetaConfig::default())?;
        src.copy_into(&dst)
    })
    .await
    .map_err(|e| MigrateDbError::Config(format!("copy task failed: {e}")))??;
    switch_config(config_path, &url_file)?;
    Ok(MigrateDbReport {
        copy,
        switched: config_path.to_owned(),
    })
}

/// Writes `[database] kind = "postgres"` with `url_file` into `loded.toml`, keeping the old
/// file as `loded.toml.before-postgres`.
fn switch_config(path: &Path, url_file: &Path) -> Result<(), MigrateDbError> {
    let text =
        std::fs::read_to_string(path).map_err(io_err(format!("cannot read {}", path.display())))?;
    let new = with_postgres(&text, url_file);
    let base = path.parent().unwrap_or(Path::new("."));
    let check =
        ServerConfig::parse(&new, base, path).map_err(|e| MigrateDbError::Config(e.to_string()))?;
    if check.database.kind != DatabaseKind::Postgres {
        return Err(MigrateDbError::Config(format!(
            "could not switch {}; set kind = \"postgres\" and url_file in [database] yourself",
            path.display()
        )));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    let saved = path.with_file_name(format!("{name}.before-postgres"));
    std::fs::copy(path, &saved).map_err(io_err(format!("cannot write {}", saved.display())))?;
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, new).map_err(io_err(format!("cannot write {}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(io_err(format!("cannot replace {}", path.display())))?;
    Ok(())
}

/// The text with a `[database]` table for Postgres: an existing table loses its `kind`, `url`
/// and `url_file` lines and gets new ones; otherwise the table is added at the end.
fn with_postgres(text: &str, url_file: &Path) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let file = toml_string(&url_file.to_string_lossy());
    let body = [
        "kind = \"postgres\"".to_owned(),
        format!("url_file = {file}"),
    ];
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_header = |l: &str| l.trim().starts_with('[');
    let table_name = |l: &str| -> Option<String> {
        let t = l.trim().split('#').next()?.trim();
        Some(t.strip_prefix('[')?.strip_suffix(']')?.trim().to_owned())
    };
    let Some(start) = lines
        .iter()
        .position(|l| is_header(l) && table_name(l).as_deref() == Some("database"))
    else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(newline);
        out.push_str("[database]");
        out.push_str(newline);
        for l in &body {
            out.push_str(l);
            out.push_str(newline);
        }
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_header(l))
        .map_or(lines.len(), |i| start + 1 + i);
    let key_of = |l: &str| l.split('=').next().map(|k| k.trim().to_owned());
    let mut out: Vec<String> = lines[..=start].iter().map(|l| (*l).to_owned()).collect();
    if !out.last().is_some_and(|l| l.ends_with('\n')) {
        if let Some(l) = out.last_mut() {
            l.push_str(newline);
        }
    }
    for l in &body {
        out.push(format!("{l}{newline}"));
    }
    for l in &lines[start + 1..end] {
        let drop = !l.trim_start().starts_with('#')
            && key_of(l).is_some_and(|k| k == "kind" || k == "url" || k == "url_file");
        if !drop {
            out.push((*l).to_owned());
        }
    }
    out.extend(lines[end..].iter().map(|l| (*l).to_owned()));
    out.concat()
}

fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_table_is_added_or_rewritten() {
        let added = with_postgres("data_dir = \"/srv\"\n", Path::new("/etc/lode/db.txt"));
        assert_eq!(
            added,
            "data_dir = \"/srv\"\n\n[database]\nkind = \"postgres\"\nurl_file = \"/etc/lode/db.txt\"\n"
        );
        let text = "a = 1\n[database]\nkind = \"sqlite\"\nconnections = 4\n[gc]\nx = 1\n";
        let out = with_postgres(text, Path::new("C:\\lode\\db.txt"));
        assert_eq!(
            out,
            "a = 1\n[database]\nkind = \"postgres\"\nurl_file = \"C:\\\\lode\\\\db.txt\"\nconnections = 4\n[gc]\nx = 1\n"
        );
    }
}
