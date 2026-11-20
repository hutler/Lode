//! The module framework (ARCHITECTURE §8): the `Module` trait, the context modules get, tool
//! definitions and the event envelope.
//!
//! Modules are compiled into `loded` and enabled in `[modules]`. They see the server only
//! through [`ModuleCtx`] and the arguments of their trait methods.

mod ctx;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use lode_core::{DepotId, ObjectId};
use lode_meta::{MetaError, Migration, Reads, WriteTx};

pub use ctx::{Auth, Blobs, Commits, Depots, Meta, ModuleCtx};

use crate::auth::Caller;
use crate::commit::{CommitCtx, CommittedCtx};
use crate::config::ModulesConfig;
use crate::error::ApiError;

/// A boxed future, as returned by object-safe async trait methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An error from a module's event handler; the dispatcher logs it and retries later.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tells a module's background worker that the server is shutting down.
#[derive(Debug, Clone)]
pub struct Stop(tokio::sync::watch::Receiver<bool>);

impl Stop {
    /// A stop signal and the sender that fires it.
    pub fn new() -> (tokio::sync::watch::Sender<bool>, Self) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (tx, Self(rx))
    }

    /// Whether shutdown has begun.
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes when shutdown begins, or when the sender is dropped.
    pub async fn wait(&mut self) {
        while !*self.0.borrow_and_update() {
            if self.0.changed().await.is_err() {
                return;
            }
        }
    }
}

/// A scope a module adds to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeDef {
    /// Scope name, such as `lock`.
    pub name: &'static str,
    /// Whether agents get it by default.
    pub agent_default: bool,
}

/// One outbox event as delivered to modules (ARCHITECTURE §8.1).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Event {
    /// Outbox id; increases with every event.
    pub id: i64,
    /// Event name, such as `revision.created`.
    pub event: String,
    /// Depot name, for depot events.
    pub depot: Option<String>,
    /// When the event was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// Identifiers only; never paths or content.
    pub data: serde_json::Value,
}

/// The handler of a tool: the caller and the JSON input, to a JSON result.
pub type ToolHandler = Arc<
    dyn Fn(Caller, serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, ApiError>>
        + Send
        + Sync,
>;

/// A transport-neutral tool description that the MCP module serves.
#[derive(Clone)]
pub struct ToolDef {
    /// Tool name, unique across modules.
    pub name: String,
    /// What the tool does, for the agent.
    pub description: String,
    /// JSON Schema of the input.
    pub input_schema: serde_json::Value,
    /// Scope the caller's token needs.
    pub scope: String,
    /// The implementation.
    pub handler: ToolHandler,
}

impl std::fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDef")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Tools of every enabled module, by name. Hosted by the MCP module.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolDef>,
}

impl ToolRegistry {
    /// Adds tools; a later tool with a name already taken is dropped with an error log.
    pub fn extend(&mut self, tools: impl IntoIterator<Item = ToolDef>) {
        for t in tools {
            if self.tools.contains_key(&t.name) {
                tracing::error!(tool = %t.name, "two modules offer a tool with this name");
                continue;
            }
            self.tools.insert(t.name.clone(), t);
        }
    }

    /// One tool.
    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    /// Every tool, by name.
    pub fn all(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.values()
    }

    /// Number of tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// An extension compiled into the server.
pub trait Module: Send + Sync + 'static {
    /// Name, as in `[modules]`; also the prefix of its tables (`mod_<name>_`) and its
    /// outbox subscriber name.
    fn name(&self) -> &'static str;

    /// Scopes this module adds.
    fn scopes(&self) -> &'static [ScopeDef] {
        &[]
    }

    /// Schema of the module's own tables.
    fn migrations(&self) -> &'static [Migration] {
        &[]
    }

    /// Routes, mounted next to the core routes with the same middleware.
    fn routes(&self, _ctx: ModuleCtx) -> Option<Router> {
        None
    }

    /// May reject a commit before it is written. Runs inside the commit transaction, so it
    /// must be fast: no network, no blob I/O. Read-only.
    fn pre_commit(&self, _tx: &dyn Reads, _ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
        Ok(())
    }

    /// Writes the module's own rows for a new revision, in the commit transaction. An error
    /// rolls the whole commit back.
    fn in_commit(&self, _tx: &WriteTx<'_>, _ctx: &CommittedCtx<'_>) -> Result<(), ApiError> {
        Ok(())
    }

    /// Handles one event after it was committed, at least once and in order. Return
    /// quickly; slow work belongs in the module's own queue. `ctx` gives access to the
    /// module's own tables.
    fn on_event<'a>(
        &'a self,
        _ctx: &'a ModuleCtx,
        _event: &'a Event,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async { Ok(()) })
    }

    /// Objects the module keeps alive in `depot`, for garbage collection.
    fn gc_roots(
        &self,
        _tx: &dyn Reads,
        _depot: DepotId,
        _out: &mut dyn FnMut(ObjectId),
    ) -> Result<(), MetaError> {
        Ok(())
    }

    /// Removes the module's rows for `depot`, which is being purged for good. Runs in the
    /// purge transaction, before the core rows go; an error rolls the purge back and it is
    /// tried again later.
    fn purge_depot(&self, _tx: &WriteTx<'_>, _depot: DepotId) -> Result<(), MetaError> {
        Ok(())
    }

    /// Tidies up after a principal was deleted, such as releasing its locks. Runs in the
    /// transaction that deletes it.
    fn principal_deleted(
        &self,
        _tx: &WriteTx<'_>,
        _principal: lode_meta::PrincipalId,
        _now: lode_meta::Millis,
    ) -> Result<(), MetaError> {
        Ok(())
    }

    /// Gauges for `/metrics` (T181), read from the module's own tables, such as open shelves
    /// per depot. Only asked when metrics are enabled; keep it to a few cheap counts.
    fn metrics(&self, _tx: &dyn Reads) -> Result<Vec<crate::metrics::Sample>, MetaError> {
        Ok(Vec::new())
    }

    /// Tools this module offers to agents.
    fn mcp_tools(&self, _ctx: ModuleCtx) -> Vec<ToolDef> {
        Vec::new()
    }

    /// A background worker that runs while the server does, such as webhook delivery. It
    /// must return soon after `stop` fires. Started by [`spawn_workers`].
    fn worker(&self, _ctx: ModuleCtx, _stop: Stop) -> Option<BoxFuture<'static, ()>> {
        None
    }
}

/// Starts the background worker of every enabled module that has one. Fire the sender of
/// `stop` and await the handles to shut them down.
pub fn spawn_workers(
    state: &crate::state::AppState,
    stop: &Stop,
) -> Vec<tokio::task::JoinHandle<()>> {
    state
        .modules()
        .iter()
        .filter_map(|m| m.worker(state.module_ctx(m.name()), stop.clone()))
        .map(tokio::spawn)
        .collect()
}

/// The enabled modules, in registration order.
pub type Modules = Vec<Arc<dyn Module>>;

/// Keeps the modules of `available` that `[modules]` enables.
pub fn select_enabled(config: &ModulesConfig, available: Modules) -> Modules {
    available
        .into_iter()
        .filter(|m| config.is_enabled(m.name()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str);
    impl Module for Named {
        fn name(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn config_selects_modules() {
        let available: Modules = vec![
            Arc::new(Named("locks")),
            Arc::new(Named("webhooks")),
            Arc::new(Named("unknown")),
        ];
        let names: Vec<&str> = select_enabled(&ModulesConfig::default(), available)
            .iter()
            .map(|m| m.name())
            .collect();
        assert_eq!(names, vec!["locks"]);
    }
}
