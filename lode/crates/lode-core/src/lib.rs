//! Core types of Lode: object ids, depot paths, trees, stored-object headers, changes,
//! error codes, glob matching and `.lodeconfig` parsing.
//!
//! This crate does no I/O beyond reading from a caller-supplied `std::io::Read` when
//! hashing, and has no async code. Every other crate builds on it.

#![forbid(unsafe_code)]

pub mod change;
pub mod chunk;
pub mod config;
pub mod error;
pub mod glob;
pub mod id;
pub mod object;
pub mod path;
pub mod tree;
mod varint;

pub use change::{validate_changes, Change, ChangeAction, ChangeSetError, CopyFrom};
pub use chunk::{
    ChunkList, ChunkListError, ChunkRef, Chunked, Chunker, ChunkingHasher, CHUNK_ALGORITHM,
    CHUNK_AVG, CHUNK_MAX, CHUNK_MIN, CHUNK_THRESHOLD,
};
pub use config::{
    valid_validate_name, CheckRequirement, ConfigError, LodeConfig, MergeToolRule, ValidateConfig,
    ValidateScript, DEFAULT_JSON_MERGE,
};
pub use error::ErrorCode;
pub use glob::{GlobError, PathGlobSet};
pub use id::{blob_id, tree_id, DepotId, IdParseError, ObjectId, TREE_ID_CONTEXT};
pub use object::{Codec, HeaderError, ObjectHeader, ObjectKind};
pub use path::{DepotPath, PathError};
pub use tree::{EntryKind, Tree, TreeEntry, TreeError};
