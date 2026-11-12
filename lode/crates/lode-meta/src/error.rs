/// Errors from the metadata store.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// SQLite reported an error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a newer build.
    #[error("database schema {namespace} is at version {found}, but this build knows only up to {known}; upgrade Lode")]
    SchemaTooNew {
        /// `core` or a module name.
        namespace: String,
        /// Version in the file.
        found: u32,
        /// Newest version this build has.
        known: u32,
    },
    /// A migration failed; the database is unchanged by it.
    #[error("migration {namespace} {version} ({name}) failed: {source}")]
    Migration {
        /// `core` or a module name.
        namespace: String,
        /// Migration number.
        version: u32,
        /// Migration name.
        name: String,
        /// SQLite's error.
        source: rusqlite::Error,
    },
    /// A unique constraint was violated, such as a duplicate name.
    #[error("already exists: {0}")]
    AlreadyExists(String),
    /// A row the caller expected is missing.
    #[error("not found: {0}")]
    NotFound(String),
    /// A stored value is malformed.
    #[error("bad stored value: {0}")]
    BadValue(String),
    /// Opening or configuring the database failed.
    #[error("database setup: {0}")]
    Setup(String),
    /// A lock was poisoned by a panic in another thread.
    #[error("database connection lock poisoned")]
    Poisoned,
}

/// Result type of this crate.
pub type Result<T, E = MetaError> = std::result::Result<T, E>;

/// Maps a unique-constraint violation to [`MetaError::AlreadyExists`].
pub(crate) fn unique(what: &str) -> impl FnOnce(rusqlite::Error) -> MetaError + '_ {
    move |e| match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
        {
            MetaError::AlreadyExists(what.to_owned())
        }
        _ => MetaError::Sqlite(e),
    }
}
