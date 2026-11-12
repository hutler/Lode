//! State shared by every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lode_core::{DepotId, LodeConfig, ObjectId};

use lode_meta::MetaStore;
use lode_store::BlobStore;

use crate::auth::{LoginLimiter, ScopeRegistry};
use crate::clock::SharedClock;
use crate::commit::{Hooks, TreeLoader};
use crate::config::ServerConfig;
use crate::lru::Lru;
use crate::perm::Permissions;

/// Cheap to clone; everything lives behind one `Arc`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    meta: MetaStore,
    blobs: Arc<dyn BlobStore>,
    config: ServerConfig,
    clock: SharedClock,
    modules: Vec<String>,
    scopes: ScopeRegistry,
    login_limiter: LoginLimiter,
    permissions: Permissions,
    trees: TreeLoader,
    config_cache: Lru<ObjectId, Arc<LodeConfig>>,
    branch_locks: Mutex<HashMap<(DepotId, String), BranchLock>>,
    hooks: Hooks,
    outbox_notify: Arc<tokio::sync::Notify>,
}

/// Serializes commits to one branch.
pub type BranchLock = Arc<tokio::sync::Mutex<()>>;

/// Parsed trees kept in memory.
const TREE_CACHE_ENTRIES: usize = 10_000;
/// Parsed `.lodeconfig` files kept in memory.
const CONFIG_CACHE_ENTRIES: usize = 256;

impl AppState {
    /// Assembles the state. `modules` lists the names of the modules that are enabled and
    /// registered, in the order `/info` reports them.
    pub fn new(
        meta: MetaStore,
        blobs: Arc<dyn BlobStore>,
        config: ServerConfig,
        clock: SharedClock,
        modules: Vec<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                meta,
                trees: TreeLoader::new(blobs.clone(), TREE_CACHE_ENTRIES),
                blobs,
                config,
                clock,
                modules,
                scopes: ScopeRegistry::core(),
                login_limiter: LoginLimiter::default(),
                permissions: Permissions::default(),
                config_cache: Lru::new(CONFIG_CACHE_ENTRIES),
                branch_locks: Mutex::new(HashMap::new()),
                hooks: Hooks::new(),
                outbox_notify: Arc::new(tokio::sync::Notify::new()),
            }),
        }
    }

    /// Metadata store.
    pub fn meta(&self) -> &MetaStore {
        &self.inner.meta
    }

    /// Blob store.
    pub fn blobs(&self) -> &Arc<dyn BlobStore> {
        &self.inner.blobs
    }

    /// Configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// Time source.
    pub fn clock(&self) -> &SharedClock {
        &self.inner.clock
    }

    /// Registered scopes.
    pub fn scopes(&self) -> &ScopeRegistry {
        &self.inner.scopes
    }

    /// Failed-login tracking.
    pub fn login_limiter(&self) -> &LoginLimiter {
        &self.inner.login_limiter
    }

    /// Access rules. Empty until [`Permissions::reload`] runs, which `open_state` does.
    pub fn permissions(&self) -> &Permissions {
        &self.inner.permissions
    }

    /// Enabled modules.
    pub fn modules(&self) -> &[String] {
        &self.inner.modules
    }

    /// Tree loader with its cache.
    pub fn trees(&self) -> &TreeLoader {
        &self.inner.trees
    }

    /// Parsed branch configurations by blob id.
    pub fn config_cache(&self) -> &Lru<ObjectId, Arc<LodeConfig>> {
        &self.inner.config_cache
    }

    /// The lock that serializes commits to one branch.
    pub fn branch_lock(&self, depot: DepotId, branch: &str) -> BranchLock {
        let mut map = match self.inner.branch_locks.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        // Drop locks nobody holds so the map does not grow with every branch ever used.
        if map.len() > 1024 {
            map.retain(|_, l| Arc::strong_count(l) > 1);
        }
        map.entry((depot, branch.to_owned()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Commit hooks of the enabled modules.
    pub fn hooks(&self) -> &Hooks {
        &self.inner.hooks
    }

    /// Woken after a transaction appends to the outbox.
    pub fn outbox_notify(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_notify.clone()
    }
}
