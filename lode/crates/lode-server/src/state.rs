//! State shared by every request.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lode_core::{DepotId, LodeConfig, ObjectId};

use lode_meta::MetaStore;
use lode_store::{BlobStore, ChunkedStore};

use crate::auth::{LoginLimiter, ScopeRegistry, Sso};
use crate::clock::SharedClock;
use crate::commit::TreeLoader;
use crate::config::ServerConfig;
use crate::failpoint::Failpoints;
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
    chunked: ChunkedStore,
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
    blame_cache: Lru<crate::read::BlameKey, Arc<crate::read::BlameData>>,
    branch_locks: Mutex<HashMap<(DepotId, String), BranchLock>>,
    outbox_notify: Arc<tokio::sync::Notify>,
    gc_fence: tokio::sync::RwLock<()>,
    gc_running: tokio::sync::Mutex<()>,
    obliterated: Mutex<HashMap<DepotId, Arc<HashSet<ObjectId>>>>,
    failpoints: Arc<Failpoints>,
    sso: Option<Sso>,
    started_at: i64,
    last_backup: Mutex<Option<BackupRun>>,
    listening: std::sync::OnceLock<std::net::SocketAddr>,
    sizes: crate::sizes::SizeCache,
}

/// The outcome of the last scheduled backup this server made (T140).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRun {
    /// When it finished, in milliseconds.
    pub at: i64,
    /// The bytes it copied, or why it failed.
    pub outcome: Result<u64, String>,
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
        let started_at = crate::clock::Clock::now_ms(&*clock);
        // Every reader sees a chunked file as its whole content (ARCHITECTURE §3.5).
        let chunked = ChunkedStore::new(blobs, config.limits.max_blob_size);
        let blobs: Arc<dyn BlobStore> = Arc::new(chunked.clone());
        let state = Self {
            inner: Arc::new(Inner {
                meta,
                trees: TreeLoader::new(blobs.clone(), TREE_CACHE_ENTRIES),
                sso: config.oidc.clone().map(Sso::new),
                blobs,
                chunked,
                config,
                clock,
                modules,
                module_names,
                tools: std::sync::OnceLock::new(),
                scopes,
                login_limiter: LoginLimiter::default(),
                permissions: Permissions::default(),
                config_cache: Lru::new(CONFIG_CACHE_ENTRIES),
                blame_cache: Lru::new(crate::read::BLAME_CACHE_ENTRIES),
                branch_locks: Mutex::new(HashMap::new()),
                outbox_notify: Arc::new(tokio::sync::Notify::new()),
                gc_fence: tokio::sync::RwLock::new(()),
                gc_running: tokio::sync::Mutex::new(()),
                obliterated: Mutex::new(HashMap::new()),
                failpoints: Arc::default(),
                started_at,
                last_backup: Mutex::new(None),
                sizes: crate::sizes::SizeCache::default(),
                listening: std::sync::OnceLock::new(),
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

    /// The blob store's chunk list operations (the same store as [`AppState::blobs`]).
    pub fn chunked(&self) -> &ChunkedStore {
        &self.inner.chunked
    }

    /// Configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// Time source.
    pub fn clock(&self) -> &SharedClock {
        &self.inner.clock
    }

    /// Single sign-on, when `[oidc]` is configured.
    pub fn sso(&self) -> Option<&Sso> {
        self.inner.sso.as_ref()
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

    /// The context a module gets, for running a module's own functions outside a request
    /// (tests, embedding).
    pub fn module_ctx(&self, module: &'static str) -> ModuleCtx {
        ModuleCtx::new(self.clone(), module)
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

    /// Computed blames (see [`crate::read::blame`]).
    pub fn blame_cache(&self) -> &Lru<crate::read::BlameKey, Arc<crate::read::BlameData>> {
        &self.inner.blame_cache
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

    /// The ids obliterated from `depot` (T120), loaded once and kept until the next
    /// obliterate of that depot.
    pub async fn obliterated(
        &self,
        depot: DepotId,
    ) -> crate::error::ApiResult<Arc<HashSet<ObjectId>>> {
        if let Some(set) = self.cached_obliterated(depot) {
            return Ok(set);
        }
        let ids = self
            .meta()
            .read(move |tx| lode_meta::Reads::obliterated_ids(tx, depot))
            .await?;
        let set = Arc::new(ids.into_iter().collect::<HashSet<_>>());
        if let Ok(mut map) = self.inner.obliterated.lock() {
            map.insert(depot, set.clone());
        }
        Ok(set)
    }

    fn cached_obliterated(&self, depot: DepotId) -> Option<Arc<HashSet<ObjectId>>> {
        self.inner
            .obliterated
            .lock()
            .ok()
            .and_then(|m| m.get(&depot).cloned())
    }

    /// Whether `id` was obliterated from `depot`.
    pub async fn is_obliterated(
        &self,
        depot: DepotId,
        id: &ObjectId,
    ) -> crate::error::ApiResult<bool> {
        Ok(self.obliterated(depot).await?.contains(id))
    }

    /// Drops the cached obliterated ids of `depot`, after they changed.
    pub(crate) fn forget_obliterated(&self, depot: DepotId) {
        if let Ok(mut map) = self.inner.obliterated.lock() {
            map.remove(&depot);
        }
    }

    /// When this state was built, which is when the server started, in milliseconds.
    pub fn started_at(&self) -> i64 {
        self.inner.started_at
    }

    /// Records the address the server is bound to, once it is (the configured one can have
    /// port 0).
    pub fn set_listening(&self, addr: std::net::SocketAddr) {
        let _ = self.inner.listening.set(addr);
    }

    /// Measured directory sizes (depots, store, database), shared by the admin pages and
    /// `GET /depots`.
    pub fn sizes(&self) -> &crate::sizes::SizeCache {
        &self.inner.sizes
    }

    /// The address the server listens on: the bound one when known, else the configured one.
    pub fn listening(&self) -> std::net::SocketAddr {
        self.inner
            .listening
            .get()
            .copied()
            .unwrap_or(self.inner.config.listen)
    }

    /// The last scheduled backup since the server started.
    pub fn last_backup(&self) -> Option<BackupRun> {
        self.inner
            .last_backup
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Records a scheduled backup's outcome.
    pub fn record_backup(&self, run: BackupRun) {
        *self
            .inner
            .last_backup
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(run);
    }

    /// Failpoints tests can arm (see [`crate::failpoint`]).
    pub fn failpoints(&self) -> &Arc<Failpoints> {
        &self.inner.failpoints
    }
}
