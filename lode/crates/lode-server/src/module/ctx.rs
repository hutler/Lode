//! What a module may use of the server: the public services, nothing else.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use lode_core::{DepotId, DepotPath, ObjectId, Tree, TreeEntry};
use lode_meta::{DepotRow, MetaStore, PrincipalRow, Reads};
use lode_proto::commit::{CommitRequest, CommitResponse};
use lode_proto::depot::{BranchList, DepotList};
use lode_proto::read::{RevisionList, RevisionListQuery, RevisionView, TreeEntryView};
use lode_store::BlobStore;

use super::ToolRegistry;
use crate::auth::Caller;
use crate::clock::SharedClock;
use crate::commit::{self, CommitOptions, Origin, TreeLoadError, TreeSource};
use crate::error::{ApiError, ApiResult};
use crate::perm::DepotAccess;
use crate::read::{self, FileChange, FileContent, ReadableRev, View, Visibility};
use crate::state::AppState;

/// The context handed to a module: its name and the server's public services.
#[derive(Clone)]
pub struct ModuleCtx {
    state: AppState,
    module: &'static str,
}

impl ModuleCtx {
    pub(crate) fn new(state: AppState, module: &'static str) -> Self {
        Self { state, module }
    }

    /// The module this context belongs to.
    pub fn module_name(&self) -> &'static str {
        self.module
    }

    /// Authentication and permissions.
    pub fn auth(&self) -> Auth<'_> {
        Auth { state: &self.state }
    }

    /// Reading depots: trees, revisions, history.
    pub fn depots(&self) -> Depots<'_> {
        Depots { state: &self.state }
    }

    /// The commit engine.
    pub fn commits(&self) -> Commits<'_> {
        Commits {
            state: &self.state,
            module: self.module,
        }
    }

    /// Content storage.
    pub fn blobs(&self) -> Blobs<'_> {
        Blobs { state: &self.state }
    }

    /// The metadata database: the module's own tables, `rev_meta` and core reads.
    pub fn meta(&self) -> Meta<'_> {
        Meta { state: &self.state }
    }

    /// Tools of every enabled module.
    pub fn tools(&self) -> &ToolRegistry {
        self.state.tools()
    }

    /// The server configuration (`loded.toml`), for a module's own section such as
    /// `[webhooks]`.
    pub fn config(&self) -> &crate::config::ServerConfig {
        self.state.config()
    }

    /// Size and count limits from `[limits]`.
    pub fn limits(&self) -> &crate::config::Limits {
        &self.state.config().limits
    }

    /// The server's time source.
    pub fn clock(&self) -> &SharedClock {
        self.state.clock()
    }

    /// Hold this shared while making stored objects referenced by the module's own tables
    /// (as a shelf does), so garbage collection cannot delete them in between. Do not hold
    /// it while calling [`Commits`], which takes it itself.
    pub fn gc_fence(&self) -> &tokio::sync::RwLock<()> {
        self.state.gc_fence()
    }

    /// Wakes the event dispatcher after the module wrote to the outbox.
    pub fn notify_events(&self) {
        self.state.outbox_notify().notify_one();
    }
}

impl FromRequestParts<ModuleCtx> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, ctx: &ModuleCtx) -> Result<Self, ApiError> {
        <Caller as FromRequestParts<AppState>>::from_request_parts(parts, &ctx.state).await
    }
}

/// Authentication and permission checks.
pub struct Auth<'a> {
    state: &'a AppState,
}

impl Auth<'_> {
    /// Resolves a bearer token (without the `Bearer ` prefix).
    pub async fn authenticate(&self, token: &str) -> ApiResult<Caller> {
        crate::auth::authenticate(self.state, token).await
    }

    /// The caller's rules in one depot.
    pub fn depot_access(&self, caller: &Caller, depot: DepotId) -> DepotAccess {
        self.state.permissions().depot(caller, depot)
    }

    /// Whether the caller is a server administrator.
    pub fn is_server_admin(&self, caller: &Caller) -> bool {
        self.state.permissions().is_server_admin(caller)
    }
}

/// Reading depots.
pub struct Depots<'a> {
    state: &'a AppState,
}

impl Depots<'_> {
    /// A depot the caller can see; `not_found` otherwise.
    pub async fn resolve(&self, caller: &Caller, name: &str) -> ApiResult<(DepotRow, DepotAccess)> {
        crate::depots::resolve(self.state, caller, name).await
    }

    /// A revision on a branch the caller can read; `not_found` otherwise.
    pub async fn readable_rev(
        &self,
        caller: &Caller,
        depot: DepotId,
        rev: u64,
    ) -> ApiResult<ReadableRev> {
        read::readable_rev(self.state, caller, depot, rev).await
    }

    /// One tree object.
    pub async fn tree(&self, depot: DepotId, id: &ObjectId) -> Result<Arc<Tree>, TreeLoadError> {
        self.state.trees().tree(depot, id).await
    }

    /// The entry at `path` below `root`.
    pub async fn lookup(
        &self,
        depot: DepotId,
        root: &ObjectId,
        path: &DepotPath,
    ) -> Result<Option<TreeEntry>, TreeLoadError> {
        commit::lookup(self.state.trees(), depot, root, path).await
    }

    /// The revision list, as `GET /revisions` returns it to `caller`.
    pub async fn revisions(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        q: RevisionListQuery,
    ) -> ApiResult<RevisionList> {
        read::list_revisions(self.state, caller, depot, q).await
    }

    /// The depots `caller` can see, as `GET /depots` returns them.
    pub async fn list(&self, caller: &Caller) -> ApiResult<DepotList> {
        crate::depots::list_depots(self.state, caller).await
    }

    /// The branches of `depot` that `caller` can read, as `GET /branches` returns them.
    pub async fn branches(
        &self,
        caller: &Caller,
        depot: &str,
        include_archived: bool,
    ) -> ApiResult<BranchList> {
        crate::branches::list_branches(self.state, caller, depot, include_archived).await
    }

    /// The visible entries of a directory at revision `rev` (the root when `path` is
    /// `None`), as `GET /tree` returns them.
    pub async fn list_dir(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        rev: u64,
        path: Option<&DepotPath>,
    ) -> ApiResult<Vec<TreeEntryView>> {
        let target = read::readable_rev(self.state, caller, depot.id, rev).await?;
        let vis = Visibility::new(target.access, View::default());
        read::list_tree(self.state, depot.id, target.rev.root_tree, path, &vis).await
    }

    /// The entry at `path` in revision `rev`: `None` when nothing is there, `not_found` when
    /// `caller` may not read the path (whether or not something is there).
    pub async fn entry(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        rev: u64,
        path: &DepotPath,
    ) -> ApiResult<Option<TreeEntry>> {
        let target = read::readable_rev(self.state, caller, depot.id, rev).await?;
        let vis = Visibility::new(target.access, View::default());
        if !vis.shows(path) {
            return Err(ApiError::not_found(format!("no file {}", path.as_str())));
        }
        commit::lookup(self.state.trees(), depot.id, &target.rev.root_tree, path)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "cannot load a tree");
                ApiError::internal()
            })
    }

    /// A file at revision `rev`, as `GET /files` returns it; `not_found` when hidden.
    pub async fn open_file(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        rev: u64,
        path: &DepotPath,
    ) -> ApiResult<FileContent> {
        let target = read::readable_rev(self.state, caller, depot.id, rev).await?;
        let vis = Visibility::new(target.access, View::default());
        read::open_file(self.state, depot.id, target.rev.root_tree, path, &vis).await
    }

    /// The files that differ between revisions `from` and `to` that `caller` can read, below
    /// `under` when given, sorted by path; at most `limit`, with whether more were left.
    pub async fn changed_files(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        from: u64,
        to: u64,
        under: Option<&DepotPath>,
        limit: usize,
    ) -> ApiResult<(Vec<FileChange>, bool)> {
        let old = read::readable_rev(self.state, caller, depot.id, from).await?;
        let new = read::readable_rev(self.state, caller, depot.id, to).await?;
        let same_rules = old.branch.id == new.branch.id
            || (old.access.reads_everything() && new.access.reads_everything());
        let old_root = old.rev.root_tree;
        let new_root = new.rev.root_tree;
        let old_vis = Visibility::new(old.access, View::default());
        let new_vis = Visibility::new(new.access, View::default());
        read::changed_files(
            self.state, depot.id, old_root, new_root, &old_vis, &new_vis, same_rules, under, limit,
        )
        .await
    }

    /// The blame of a text file at revision `rev`, as `GET /blame` returns it to `caller`.
    pub async fn blame(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        rev: u64,
        path: &DepotPath,
    ) -> ApiResult<lode_proto::read::BlameView> {
        read::blame(self.state, caller, depot, rev, path).await
    }

    /// One revision, as `GET /revisions/{n}` returns it to `caller`.
    pub async fn revision(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        n: u64,
    ) -> ApiResult<RevisionView> {
        read::revision_detail(self.state, caller, depot, n).await
    }
}

/// The commit engine. Commits made here carry `origin = Module { name, attrs }` with this
/// module's name; no request can set that.
pub struct Commits<'a> {
    state: &'a AppState,
    module: &'static str,
}

impl Commits<'_> {
    /// Commits `req` to `branch` with `caller` as submitter. `author` overrides the author
    /// (a shelf submit commits as the shelf owner); `attrs` describe the origin.
    pub async fn commit(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        branch: &str,
        req: CommitRequest,
        author: Option<PrincipalRow>,
        attrs: BTreeMap<String, String>,
    ) -> ApiResult<CommitResponse> {
        let opts = CommitOptions {
            author,
            origin: Origin::Module {
                name: self.module.to_owned(),
                attrs,
            },
            via: caller.via,
            client: None,
        };
        commit::commit(self.state, caller, depot, branch, req, opts).await
    }

    /// Checks a change set the way a commit would, against `base_rev` of `branch` instead of
    /// the head, with `need` on every changed path (a shelf needs `Read`). Returns the
    /// branch. See [`commit::check_change_set`].
    #[allow(clippy::too_many_arguments)]
    pub async fn check(
        &self,
        caller: &Caller,
        depot: &DepotRow,
        branch: &str,
        base_rev: u64,
        changes: &[lode_core::Change],
        merged_from: &[lode_proto::commit::MergedFrom],
        provenance: Option<&lode_proto::commit::Provenance>,
        need: crate::perm::Level,
    ) -> ApiResult<lode_meta::BranchRow> {
        commit::check_change_set(
            self.state,
            caller,
            depot,
            branch,
            base_rev,
            changes,
            merged_from,
            provenance,
            need,
        )
        .await
    }
}

/// Content storage.
pub struct Blobs<'a> {
    state: &'a AppState,
}

impl Blobs<'_> {
    /// The blob store.
    pub fn store(&self) -> &Arc<dyn BlobStore> {
        self.state.blobs()
    }

    /// Whether `id` was obliterated from `depot` (T120). Such content must not be read or
    /// stored again; [`Blobs::check_storable`] gives the error to answer with.
    pub async fn is_obliterated(&self, depot: DepotId, id: &ObjectId) -> ApiResult<bool> {
        self.state.is_obliterated(depot, id).await
    }

    /// Refuses content that was obliterated from `depot`, with code `obliterated`.
    pub async fn check_storable(&self, depot: DepotId, id: &ObjectId) -> ApiResult<()> {
        if self.is_obliterated(depot, id).await? {
            return Err(ApiError::new(
                lode_core::ErrorCode::Obliterated,
                format!("content {id} was obliterated from this depot and cannot be stored again"),
            ));
        }
        Ok(())
    }
}

/// The metadata database.
pub struct Meta<'a> {
    state: &'a AppState,
}

impl Meta<'_> {
    /// The store. Modules write only their own `mod_<name>_` tables, `rev_meta` and the
    /// outbox; core tables are read through [`Reads`].
    pub fn store(&self) -> &MetaStore {
        self.state.meta()
    }

    /// Runs `f` in a read transaction.
    pub async fn read<T, F>(&self, f: F) -> Result<T, lode_meta::MetaError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn Reads) -> Result<T, lode_meta::MetaError> + Send + 'static,
    {
        self.state.meta().read(move |tx| f(tx)).await
    }
}
