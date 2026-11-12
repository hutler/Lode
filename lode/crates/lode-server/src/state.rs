//! State shared by every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lode_core::{DepotId, LodeConfig, ObjectId};

use lode_meta::MetaStore;
use lode_store::BlobStore;

use crate::auth::{LoginLimiter, ScopeRegistry};
use crate::clock::SharedClock;
use crate::commit::TreeLoader;
use crate::config::ServerConfig;
use crate::lru::Lru;
use crate::module::{Module, ModuleCtx, Modules, ToolRegistry};
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
    modules: Modules,
    module_names: Vec<String>,
    tools: std::sync::OnceLock<ToolRegistry>,
    scopes: ScopeRegistry,
    login_limiter: LoginLimiter,
    permissions: Permissions,
    trees: TreeLoader,
    config_cache: Lru<ObjectId, Arc<LodeConfig>>,
    branch_locks: Mutex<HashMap<(DepotId, String), BranchLock>>,
    outbox_notify: Arc<tokio::sync::Notify>,
    gc_fence: tokio::sync::RwLock<()>,
    gc_running: tokio::sync::Mutex<()>,
}

/// Serializes commits to one branch.
pub type BranchLock = Arc<tokio::sync::Mutex<()>>;

/// Parsed trees kept in memory.
const TREE_CACHE_ENTRIES: usize = 10_000;
/// Parsed `.lodeconfig` files kept in memory.
const CONFIG_CACHE_ENTRIES: usize = 256;

impl AppState {
    /// Assembles the state with the enabled modules, in registration order. Their scopes
    /// join the registry and their tools are collected.
    pub fn new(
        meta: MetaStore,
        blobs: Arc<dyn BlobStore>,
        config: ServerConfig,
        clock: SharedClock,
        modules: Modules,
    ) -> Self {
        let mut scopes = ScopeRegistry::core();
        for m in &modules {
            scopes.extend_defs(m.scopes());
        }
        let module_names = modules.iter().map(|m| m.name().to_owned()).collect();
        let state = Self {
            inner: Arc::new(Inner {
                meta,
                trees: TreeLoader::new(blobs.clone(), TREE_CACHE_ENTRIES),
                blobs,
                config,
                clock,
                modules,
                module_names,
                tools: std::sync::OnceLock::new(),
                scopes,
                login_limiter: LoginLimiter::default(),
                permissions: Permissions::default(),
                config_cache: Lru::new(CONFIG_CACHE_ENTRIES),
                branch_locks: Mutex::new(HashMap::new()),
                outbox_notify: Arc::new(tokio::sync::Notify::new()),
                gc_fence: tokio::sync::RwLock::new(()),
                gc_running: tokio::sync::Mutex::new(()),
            }),
        };
        let mut tools = ToolRegistry::default();
        for m in state.modules() {
            tools.extend(m.mcp_tools(ModuleCtx::new(state.clone(), m.name())));
        }
        // Freshly created, so the cell is empty and this cannot fail.
        let _ = state.inner.tools.set(tools);
        state
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
    pub fn modules(&self) -> &[Arc<dyn Module>] {
        &self.inner.modules
    }

    /// Names of the enabled modules.
    pub fn module_names(&self) -> &[String] {
        &self.inner.module_names
    }

    /// Tools of the enabled modules.
    pub fn tools(&self) -> &ToolRegistry {
        static EMPTY: std::sync::OnceLock<ToolRegistry> = std::sync::OnceLock::new();
        self.inner
            .tools
            .get()
            .unwrap_or_else(|| EMPTY.get_or_init(ToolRegistry::default))
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

    /// Woken after a transaction appends to the outbox.
    pub fn outbox_notify(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_notify.clone()
    }

    /// Held shared by anything that makes existing objects referenced (a commit from its
    /// blob checks to its transaction); garbage collection holds it exclusively while it
    /// deletes. Never hold it across a call that takes it again.
    pub fn gc_fence(&self) -> &tokio::sync::RwLock<()> {
        &self.inner.gc_fence
    }

    /// Held while a garbage collection runs; one at a time.
    pub(crate) fn gc_running(&self) -> &tokio::sync::Mutex<()> {
        &self.inner.gc_running
    }
}
