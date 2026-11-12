//! State shared by every request.

use std::sync::Arc;

use lode_meta::MetaStore;
use lode_store::BlobStore;

use crate::auth::{LoginLimiter, ScopeRegistry};
use crate::clock::SharedClock;
use crate::config::ServerConfig;

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
}

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
                blobs,
                config,
                clock,
                modules,
                scopes: ScopeRegistry::core(),
                login_limiter: LoginLimiter::default(),
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

    /// Enabled modules.
    pub fn modules(&self) -> &[String] {
        &self.inner.modules
    }
}
