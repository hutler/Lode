use lode_core::{ErrorCode, ObjectId};

/// Errors from a blob store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The content does not hash to the expected id.
    #[error("content hashes to {actual}, expected {expected}")]
    HashMismatch {
        /// The id the caller gave.
        expected: ObjectId,
        /// The id of what was received.
        actual: ObjectId,
    },
    /// The content is longer than the store's cap.
    #[error("object exceeds the size limit of {limit} bytes")]
    TooLarge {
        /// The cap.
        limit: u64,
    },
    /// The stream's length differs from the announced length.
    #[error("received {actual} bytes, expected {expected}")]
    LengthMismatch {
        /// The announced length.
        expected: u64,
        /// The bytes received (or at least received, when longer).
        actual: u64,
    },
    /// The object does not exist.
    #[error("object {0} not found")]
    NotFound(ObjectId),
    /// The kind cannot be stored by this build.
    #[error("object kind {0:?} is not supported")]
    UnsupportedKind(lode_core::ObjectKind),
    /// A stored object is damaged.
    #[error("object {id} is corrupt: {reason}")]
    Corrupt {
        /// The object.
        id: ObjectId,
        /// What is wrong.
        reason: String,
    },
    /// Underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl StoreError {
    /// The API error code for this failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            StoreError::HashMismatch { .. } => ErrorCode::HashMismatch,
            StoreError::TooLarge { .. } => ErrorCode::TooLarge,
            StoreError::LengthMismatch { .. } | StoreError::UnsupportedKind(_) => {
                ErrorCode::InvalidRequest
            }
            StoreError::NotFound(_) => ErrorCode::NotFound,
            StoreError::Corrupt { .. } | StoreError::Io(_) => ErrorCode::Internal,
        }
    }
}

/// Result type of this crate.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;
