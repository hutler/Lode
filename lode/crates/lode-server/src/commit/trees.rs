//! Loading tree objects through a bounded cache, and path lookups in a tree.

use std::sync::Arc;

use async_trait::async_trait;
use lode_core::{DepotId, DepotPath, EntryKind, ObjectId, ObjectKind, Tree, TreeEntry};
use lode_store::{BlobStore, StoreError};
use tokio::io::AsyncReadExt;

use crate::lru::Lru;

/// Largest tree object loaded into memory (a directory of roughly a million entries).
const MAX_TREE_BYTES: u64 = 256 * 1024 * 1024;

/// Why a tree could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum TreeLoadError {
    /// The object does not exist.
    #[error("tree {0} is missing")]
    Missing(ObjectId),
    /// The object exists but is not a valid tree.
    #[error("tree {0} is corrupt: {1}")]
    Corrupt(ObjectId, String),
    /// Storage failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Read failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Something that yields decoded trees by id.
#[async_trait]
pub trait TreeSource: Send + Sync {
    /// Loads one tree of `depot`.
    async fn tree(&self, depot: DepotId, id: &ObjectId) -> Result<Arc<Tree>, TreeLoadError>;
}

/// Trees from the blob store, cached by id. Ids are content hashes, so one cache serves every
/// depot.
pub struct TreeLoader {
    blobs: Arc<dyn BlobStore>,
    cache: Lru<ObjectId, Arc<Tree>>,
}

impl TreeLoader {
    /// A loader keeping up to `capacity` trees in memory.
    pub fn new(blobs: Arc<dyn BlobStore>, capacity: usize) -> Self {
        Self {
            blobs,
            cache: Lru::new(capacity),
        }
    }
}

#[async_trait]
impl TreeSource for TreeLoader {
    async fn tree(&self, depot: DepotId, id: &ObjectId) -> Result<Arc<Tree>, TreeLoadError> {
        if let Some(t) = self.cache.get(id) {
            return Ok(t);
        }
        let obj = match self.blobs.get(depot, id).await {
            Ok(o) => o,
            Err(StoreError::NotFound(_)) => return Err(TreeLoadError::Missing(*id)),
            Err(e) => return Err(e.into()),
        };
        if obj.kind != ObjectKind::Tree {
            return Err(TreeLoadError::Corrupt(*id, "not a tree object".into()));
        }
        if obj.raw_len > MAX_TREE_BYTES {
            return Err(TreeLoadError::Corrupt(
                *id,
                "tree object is too large".into(),
            ));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(obj.raw_len).unwrap_or(0));
        obj.reader
            .take(MAX_TREE_BYTES)
            .read_to_end(&mut bytes)
            .await?;
        let tree = Tree::decode(&bytes).map_err(|e| TreeLoadError::Corrupt(*id, e.to_string()))?;
        let tree = Arc::new(tree);
        self.cache.insert(*id, tree.clone());
        Ok(tree)
    }
}

/// The entry at `path` in the tree rooted at `root`, or `None` when absent (including when
/// an ancestor is a file).
pub async fn lookup(
    src: &dyn TreeSource,
    depot: DepotId,
    root: &ObjectId,
    path: &DepotPath,
) -> Result<Option<TreeEntry>, TreeLoadError> {
    let mut dir = src.tree(depot, root).await?;
    let segments: Vec<&str> = path.segments().collect();
    for (i, seg) in segments.iter().enumerate() {
        let Some(e) = dir.get(seg).cloned() else {
            return Ok(None);
        };
        if i + 1 == segments.len() {
            return Ok(Some(e));
        }
        if e.kind != EntryKind::Dir {
            return Ok(None);
        }
        dir = src.tree(depot, &e.id).await?;
    }
    Ok(None)
}
