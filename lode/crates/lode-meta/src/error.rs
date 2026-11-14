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
        /// The database's error.
        source: Box<MetaError>,
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
pub fn unique(what: &str) -> impl FnOnce(MetaError) -> MetaError + '_ {
    move |e| {
        if e.is_unique_violation() {
            MetaError::AlreadyExists(what.to_owned())
        } else {
            e
        }
    }
}

impl MetaError {
    /// Whether this is a unique or primary-key constraint violation.
    pub fn is_unique_violation(&self) -> bool {
        match self {
            MetaError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => {
                f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
            }
            MetaError::Migration { source, .. } => source.is_unique_violation(),
            _ => false,
        }
    }
}
