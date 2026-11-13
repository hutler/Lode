//! Lode module: shelves (ARCHITECTURE §9.2, SPEC §6.2).
//!
//! A shelf is a change list stored on the server, exactly as a commit would send it, against
//! a base revision of a branch. Blobs are uploaded through the blob API first. Every update
//! replaces the whole content and raises `version`, so one version names one exact state.

#![forbid(unsafe_code)]

mod db;
mod mcp;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use lode_core::{Change, ChangeAction, DepotId, DepotPath, EntryKind, ErrorCode, ObjectId};
use lode_meta::{
    BranchId, DepotRow, MetaError, Migration, PrincipalId, PrincipalKind, Reads, WriteTx,
};
use lode_proto::commit::{CommitRequest, CommitResponse, MergedFrom, Provenance};
use lode_proto::consts::paths;
use lode_proto::shelves::{
    CreateShelfRequest, ShelfFileDiff, ShelfList, ShelfState, ShelfView, ShelvesQuery,
    SubmitShelfRequest, UpdateShelfRequest,
};
use lode_proto::PrincipalRef;
use lode_server::auth::SCOPE_WRITE;
use lode_server::commit::{CommittedCtx, Origin};
use lode_server::module::{Module, ModuleCtx, ScopeDef, ToolDef};
use lode_server::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult, Caller, DepotAccess, Level};
use serde_json::json;
use tokio::io::AsyncReadExt;

use db::{NewShelf, ShelfRow, DELETED, OPEN, SUBMITTED};

/// Module name, as in `[modules]`.
pub const NAME: &str = "shelves";
/// Scope needed to create, update and delete one's own shelves.
pub const SCOPE_SHELVE: &str = "shelve";
/// Longest description, in bytes; the same limit as a commit message.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Largest file a shelf diff shows, in bytes.
pub const MAX_DIFF_FILE: u64 = 1024 * 1024;
/// Largest total of patches in one shelf diff, in bytes.
pub const MAX_DIFF_TOTAL: usize = 4 * 1024 * 1024;
/// `rev_meta` key naming the shelf a revision was submitted from.
pub const REV_META_SHELF: &str = "shelves.shelf_id";
/// Origin attributes of a submit commit.
const ATTR_ID: &str = "shelf_id";
const ATTR_VERSION: &str = "shelf_version";

const SCOPES: &[ScopeDef] = &[ScopeDef {
    name: SCOPE_SHELVE,
    agent_default: true,
}];

/// The shelves module.
#[derive(Debug, Default, Clone, Copy)]
pub struct Shelves;

impl Shelves {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self)
    }
}

fn sql_error(e: rusqlite::Error) -> MetaError {
    MetaError::from(e)
}

fn ms_to_time(ms: lode_meta::Millis) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

fn state_of(s: &str) -> ShelfState {
    match s {
        SUBMITTED => ShelfState::Submitted,
        DELETED => ShelfState::Deleted,
        _ => ShelfState::Open,
    }
}

fn state_text(s: ShelfState) -> &'static str {
    match s {
        ShelfState::Open => OPEN,
        ShelfState::Submitted => SUBMITTED,
        ShelfState::Deleted => DELETED,
    }
}

/// A shelf with what a view needs: branch name, owner and, when asked for, the changes.
struct Loaded {
    row: ShelfRow,
    branch: String,
    owner: PrincipalRef,
    owner_id: PrincipalId,
    agent_owner: Option<String>,
    paths: Vec<String>,
    changes: Option<Vec<Change>>,
}

impl Loaded {
    fn view(&self) -> ShelfView {
        ShelfView {
            id: self.row.id as u64,
            version: self.row.version,
            branch: self.branch.clone(),
            base_rev: self.row.base_rev,
            owner: self.owner.clone(),
            agent_owner: self.agent_owner.clone(),
            description: self.row.description.clone(),
            state: state_of(&self.row.state),
            submitted_rev: self.row.submitted_rev,
            created_at: ms_to_time(self.row.created_at),
            updated_at: ms_to_time(self.row.updated_at),
            changes: self.changes.clone(),
            merged_from: self
                .row
                .merged_from
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok())
                .unwrap_or_default(),
            provenance: self
                .row
                .provenance
                .as_deref()
                .and_then(|p| serde_json::from_str(p).ok()),
            diff: None,
        }
    }

    /// Readable only with `read` on the branch and on every path of the shelf.
    fn visible_to(&self, access: &DepotAccess) -> bool {
        let b = access.branch(&self.branch);
        if !b.reads_anything() {
            return false;
        }
        self.paths.iter().all(|p| {
            DepotPath::parse(p)
                .map(|p| b.allows(&p, Level::Read))
                .unwrap_or(false)
        })
    }
}

/// Looks up names once each while loading several shelves.
struct Loader<'a> {
    tx: &'a dyn Reads,
    branches: HashMap<i64, String>,
    owners: HashMap<i64, (PrincipalRef, Option<String>)>,
}

impl<'a> Loader<'a> {
    fn new(tx: &'a dyn Reads) -> Self {
        Self {
            tx,
            branches: HashMap::new(),
            owners: HashMap::new(),
        }
    }

    fn load(&mut self, row: ShelfRow, with_changes: bool) -> Result<Loaded, MetaError> {
        let branch = match self.branches.get(&row.branch_id) {
            Some(b) => b.clone(),
            None => {
                let name = self
                    .tx
                    .branch_by_id(BranchId(row.branch_id))?
                    .map(|b| b.name)
                    .unwrap_or_default();
                self.branches.insert(row.branch_id, name.clone());
                name
            }
        };
        let (owner, agent_owner) = match self.owners.get(&row.owner_id) {
            Some(o) => o.clone(),
            None => {
                let o = match self.tx.principal(PrincipalId(row.owner_id))? {
                    Some(p) => {
                        let agent_owner = match (p.kind, p.owner_id) {
                            (PrincipalKind::Agent, Some(o)) => {
                                self.tx.principal(o)?.map(|o| o.name)
                            }
                            _ => None,
                        };
                        let kind = match p.kind {
                            PrincipalKind::User => lode_proto::PrincipalKind::User,
                            PrincipalKind::Agent => lode_proto::PrincipalKind::Agent,
                        };
                        (
                            PrincipalRef {
                                name: p.name,
                                kind,
                                owner: agent_owner.clone(),
                            },
                            agent_owner,
                        )
                    }
                    None => (
                        PrincipalRef {
                            name: format!("#{}", row.owner_id),
                            kind: lode_proto::PrincipalKind::User,
                            owner: None,
                        },
                        None,
                    ),
                };
                self.owners.insert(row.owner_id, o.clone());
                o
            }
        };
        let conn = self.tx.db();
        let changes = if with_changes {
            Some(db::changes(conn, row.id).map_err(sql_error)?)
        } else {
            None
        };
        let paths = match &changes {
            Some(c) => c.iter().map(|c| c.path.as_str().to_owned()).collect(),
            None => db::paths(conn, row.id).map_err(sql_error)?,
        };
        Ok(Loaded {
            owner_id: PrincipalId(row.owner_id),
            row,
            branch,
            owner,
            agent_owner,
            paths,
            changes,
        })
    }
}

/// The data of a `shelf.*` event (ARCHITECTURE §8.1): identifiers only.
fn event_data(s: &Loaded, rev: Option<u64>) -> String {
    let mut data = json!({
        "shelf_id": s.row.id,
        "version": s.row.version,
        "branch": s.branch,
        "base_rev": s.row.base_rev,
        "owner": s.owner.name,
        "owner_kind": match s.owner.kind {
            lode_proto::PrincipalKind::User => "user",
            lode_proto::PrincipalKind::Agent => "agent",
        },
    });
    if let Some(rev) = rev {
        data["rev"] = json!(rev);
    }
    data.to_string()
}

fn parse_id(id: &str) -> ApiResult<i64> {
    id.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::not_found(format!("no shelf {id}")))
}

/// One shelf the caller can see, or `not_found`.
async fn visible_shelf(
    ctx: &ModuleCtx,
    access: &DepotAccess,
    depot: DepotId,
    id: i64,
    with_changes: bool,
) -> ApiResult<Loaded> {
    let loaded = ctx
        .meta()
        .read(move |tx| {
            let Some(row) = db::shelf(tx.db(), depot, id).map_err(sql_error)? else {
                return Ok(None);
            };
            Loader::new(tx).load(row, with_changes).map(Some)
        })
        .await?;
    loaded
        .filter(|s| s.visible_to(access))
        .ok_or_else(|| ApiError::not_found(format!("no shelf {id}")))
}

/// What create and replace have in common: description limit and the change-set check.
#[allow(clippy::too_many_arguments)]
async fn check_content(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &DepotRow,
    branch: &str,
    base_rev: u64,
    description: &str,
    changes: &[Change],
    merged_from: &[MergedFrom],
    provenance: Option<&Provenance>,
) -> ApiResult<lode_meta::BranchRow> {
    if description.len() > MAX_DESCRIPTION {
        return Err(ApiError::invalid_request("description is too long"));
    }
    // A shelf proposes changes; it needs read access, not write.
    ctx.commits()
        .check(
            caller,
            depot,
            branch,
            base_rev,
            changes,
            merged_from,
            provenance,
            Level::Read,
        )
        .await
}

fn json_text<T: serde::Serialize>(v: &T) -> ApiResult<String> {
    serde_json::to_string(v).map_err(|_| ApiError::internal())
}

async fn create(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<CreateShelfRequest>,
) -> ApiResult<(StatusCode, Json<ShelfView>)> {
    caller.require_scope(SCOPE_SHELVE)?;
    Ok((
        StatusCode::CREATED,
        Json(create_shelf(&ctx, &caller, &depot, req).await?),
    ))
}

/// The provenance a shelf stores: the client's, with `via` set by the server.
fn shelf_provenance(caller: &Caller, given: Option<Provenance>) -> Provenance {
    let mut p = given.unwrap_or_default();
    p.via = Some(caller.via);
    p.submitted_shelf = None;
    p
}

/// Creates a shelf owned by `caller` (see `POST …/shelves`). The caller must have passed
/// the scope check.
pub async fn create_shelf(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    mut req: CreateShelfRequest,
) -> ApiResult<ShelfView> {
    req.provenance = Some(shelf_provenance(caller, req.provenance.take()));
    let (d, _) = ctx.depots().resolve(caller, depot).await?;
    // Garbage collection must not delete the blobs between the check and the write.
    let _fence = ctx.gc_fence().read().await;
    let branch = check_content(
        ctx,
        caller,
        &d,
        &req.branch,
        req.base_rev,
        &req.description,
        &req.changes,
        &req.merged_from,
        req.provenance.as_ref(),
    )
    .await?;
    let merged = (!req.merged_from.is_empty())
        .then(|| json_text(&req.merged_from))
        .transpose()?;
    let provenance = req.provenance.as_ref().map(json_text).transpose()?;
    let now = ctx.clock().now_ms();
    let owner = caller.principal.id.0;
    let depot_id = d.id;
    let loaded = ctx
        .meta()
        .store()
        .write(move |tx| {
            let id = db::insert_shelf(
                tx.conn(),
                &NewShelf {
                    depot: depot_id,
                    branch_id: branch.id.0,
                    base_rev: req.base_rev,
                    owner_id: owner,
                    description: &req.description,
                    provenance,
                    merged_from: merged,
                    changes: &req.changes,
                    now,
                },
            )
            .map_err(sql_error)?;
            let row = db::shelf(tx.conn(), depot_id, id)
                .map_err(sql_error)?
                .ok_or_else(|| MetaError::NotFound("new shelf".into()))?;
            let loaded = Loader::new(tx).load(row, true)?;
            tx.append_outbox(
                Some(depot_id),
                "shelf.created",
                &event_data(&loaded, None),
                now,
            )?;
            Ok(loaded)
        })
        .await?;
    ctx.notify_events();
    Ok(loaded.view())
}

enum Changed {
    Done(Box<Loaded>),
    NotOpen(String),
    Gone,
}

async fn replace(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    ApiJson(req): ApiJson<UpdateShelfRequest>,
) -> ApiResult<Json<ShelfView>> {
    caller.require_scope(SCOPE_SHELVE)?;
    let id = parse_id(&id)?;
    Ok(Json(update_shelf(&ctx, &caller, &depot, id, req).await?))
}

/// Replaces the content of shelf `id`, which `caller` must own (see `PUT …/shelves/{id}`).
/// The caller must have passed the scope check.
pub async fn update_shelf(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    id: i64,
    mut req: UpdateShelfRequest,
) -> ApiResult<ShelfView> {
    req.provenance = Some(shelf_provenance(caller, req.provenance.take()));
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let current = visible_shelf(ctx, &access, d.id, id, false).await?;
    if current.owner_id != caller.principal.id {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "only the owner can change a shelf",
        ));
    }
    if current.row.state != OPEN {
        return Err(not_open(&current.row.state));
    }
    let _fence = ctx.gc_fence().read().await;
    check_content(
        ctx,
        caller,
        &d,
        &current.branch,
        req.base_rev,
        &req.description,
        &req.changes,
        &req.merged_from,
        req.provenance.as_ref(),
    )
    .await?;
    let merged = (!req.merged_from.is_empty())
        .then(|| json_text(&req.merged_from))
        .transpose()?;
    let provenance = req.provenance.as_ref().map(json_text).transpose()?;
    let now = ctx.clock().now_ms();
    let depot_id = d.id;
    let branch_id = current.row.branch_id;
    let owner = current.row.owner_id;
    let out = ctx
        .meta()
        .store()
        .write(move |tx| {
            let replaced = db::replace_shelf(
                tx.conn(),
                id,
                &NewShelf {
                    depot: depot_id,
                    branch_id,
                    base_rev: req.base_rev,
                    owner_id: owner,
                    description: &req.description,
                    provenance,
                    merged_from: merged,
                    changes: &req.changes,
                    now,
                },
            )
            .map_err(sql_error)?;
            let Some(row) = db::shelf(tx.conn(), depot_id, id).map_err(sql_error)? else {
                return Ok(Changed::Gone);
            };
            if !replaced {
                return Ok(Changed::NotOpen(row.state));
            }
            let loaded = Loader::new(tx).load(row, true)?;
            tx.append_outbox(
                Some(depot_id),
                "shelf.updated",
                &event_data(&loaded, None),
                now,
            )?;
            Ok(Changed::Done(Box::new(loaded)))
        })
        .await?;
    match out {
        Changed::Done(loaded) => {
            ctx.notify_events();
            Ok(loaded.view())
        }
        Changed::NotOpen(state) => Err(not_open(&state)),
        Changed::Gone => Err(ApiError::not_found(format!("no shelf {id}"))),
    }
}

/// The answer for a shelf that `expected` names: `Some` when it was already submitted at that
/// version, `None` when it is open at that version, `conflict` otherwise.
fn submitted_already(s: &Loaded, expected: u64) -> ApiResult<Option<CommitResponse>> {
    let version = s.row.version;
    match s.row.state.as_str() {
        SUBMITTED if version == expected => Ok(Some(CommitResponse {
            rev: s.row.submitted_rev.unwrap_or(0),
            branch: s.branch.clone(),
            replayed: true,
        })),
        OPEN if version == expected => Ok(None),
        OPEN => Err(ApiError::new(
            ErrorCode::Conflict,
            format!("the shelf is at version {version}, not {expected}; review it again"),
        )
        .with_details(json!({ "version": version }))),
        state => Err(not_open(state)),
    }
}

/// `POST …/shelves/{id}/submit`: commits the shelf as its owner, with the caller as submitter.
async fn submit(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    ApiJson(req): ApiJson<SubmitShelfRequest>,
) -> ApiResult<Json<CommitResponse>> {
    caller.require_scope(SCOPE_WRITE)?;
    let id = parse_id(&id)?;
    Ok(Json(submit_shelf(&ctx, &caller, &depot, id, req).await?))
}

/// Submits shelf `id` of `depot` (see [`submit`]). Submitting a submitted shelf again answers
/// its revision.
pub async fn submit_shelf(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    id: i64,
    req: SubmitShelfRequest,
) -> ApiResult<CommitResponse> {
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let shelf = visible_shelf(ctx, &access, d.id, id, true).await?;
    if let Some(done) = submitted_already(&shelf, req.expected_version)? {
        return Ok(done);
    }
    let message = req
        .message
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| shelf.row.description.clone());
    let mut provenance: Provenance = shelf
        .row
        .provenance
        .as_deref()
        .and_then(|p| serde_json::from_str(p).ok())
        .unwrap_or_default();
    provenance.submitted_shelf = Some(id as u64);
    let merged_from: Vec<MergedFrom> = shelf
        .row
        .merged_from
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or_default();
    let owner_id = shelf.owner_id;
    let author = ctx
        .meta()
        .read(move |tx| tx.principal(owner_id))
        .await?
        .ok_or_else(|| ApiError::not_found("the shelf's owner no longer exists"))?;
    let version = shelf.row.version;
    let request = CommitRequest {
        // Unique per attempt: whether this shelf version was submitted is the shelf's state,
        // not a request id a client could also send.
        request_id: format!(
            "shelves:{id}:v{version}:{}:{}",
            caller.principal.id.0,
            ctx.clock().now_ms()
        ),
        message,
        changes: shelf.changes.clone().unwrap_or_default(),
        merged_from,
        provenance: Some(provenance),
        extensions: BTreeMap::new(),
    };
    let attrs: BTreeMap<String, String> = [
        (ATTR_ID.to_owned(), id.to_string()),
        (ATTR_VERSION.to_owned(), version.to_string()),
    ]
    .into();
    match ctx
        .commits()
        .commit(caller, &d, &shelf.branch, request, Some(author), attrs)
        .await
    {
        Ok(r) => Ok(r),
        Err(e) => {
            // Another submit of the same version may have won the race.
            let again = visible_shelf(ctx, &access, d.id, id, false).await?;
            match submitted_already(&again, req.expected_version) {
                Ok(Some(done)) => Ok(done),
                _ => Err(e),
            }
        }
    }
}

fn not_open(state: &str) -> ApiError {
    ApiError::new(ErrorCode::Conflict, format!("the shelf is {state}"))
        .with_details(json!({ "state": state }))
}

async fn remove(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
) -> ApiResult<Json<ShelfView>> {
    let id = parse_id(&id)?;
    let (d, access) = ctx.depots().resolve(&caller, &depot).await?;
    let current = visible_shelf(&ctx, &access, d.id, id, false).await?;
    if current.owner_id == caller.principal.id {
        caller.require_scope(SCOPE_SHELVE)?;
    } else if !access.is_depot_admin() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "only the owner or an administrator can delete a shelf",
        ));
    }
    if current.row.state == DELETED {
        return Ok(Json(current.view()));
    }
    if current.row.state != OPEN {
        return Err(not_open(&current.row.state));
    }
    let now = ctx.clock().now_ms();
    let depot_id = d.id;
    let out = ctx
        .meta()
        .store()
        .write(move |tx| {
            let deleted = db::delete_shelf(tx.conn(), id, now).map_err(sql_error)?;
            let Some(row) = db::shelf(tx.conn(), depot_id, id).map_err(sql_error)? else {
                return Ok(Changed::Gone);
            };
            let loaded = Loader::new(tx).load(row, false)?;
            if deleted {
                tx.append_outbox(
                    Some(depot_id),
                    "shelf.deleted",
                    &event_data(&loaded, None),
                    now,
                )?;
            } else if loaded.row.state != DELETED {
                return Ok(Changed::NotOpen(loaded.row.state.clone()));
            }
            Ok(Changed::Done(Box::new(loaded)))
        })
        .await?;
    match out {
        Changed::Done(loaded) => {
            ctx.notify_events();
            Ok(Json(loaded.view()))
        }
        Changed::NotOpen(state) => Err(not_open(&state)),
        Changed::Gone => Err(ApiError::not_found(format!("no shelf {id}"))),
    }
}

async fn list(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<ShelfList>> {
    let state = match q.get("state") {
        None | Some("") => None,
        Some(s) => Some(
            serde_json::from_value::<ShelfState>(json!(s))
                .map_err(|_| ApiError::invalid_request(format!("unknown shelf state `{s}`")))?,
        ),
    };
    let query = ShelvesQuery {
        owner: q.get("owner").map(str::to_owned),
        branch: q.get("branch").map(str::to_owned),
        state,
    };
    Ok(Json(list_shelves(&ctx, &caller, &depot, query).await?))
}

/// The shelves of a depot that `caller` can see, newest first. Without `state`, open ones.
pub async fn list_shelves(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    q: ShelvesQuery,
) -> ApiResult<ShelfList> {
    caller.require_scope("read")?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let depot_id = d.id;
    let branch = q.branch.filter(|b| !b.is_empty());
    if let Some(b) = &branch {
        if !access.branch(b).reads_anything() {
            return Err(ApiError::not_found(format!("no branch `{b}`")));
        }
    }
    let owner = q.owner.filter(|o| !o.is_empty());
    let state = state_text(q.state.unwrap_or(ShelfState::Open));
    let loaded = ctx
        .meta()
        .read(move |tx| {
            let branch_id = match &branch {
                Some(name) => match tx.branch(depot_id, name)? {
                    Some(b) => Some(b.id.0),
                    None => return Ok(None),
                },
                None => None,
            };
            let owner_id = match &owner {
                Some(name) => match tx.principal_by_name(name)? {
                    Some(p) => Some(p.id.0),
                    None => return Ok(Some(Vec::new())),
                },
                None => None,
            };
            let rows =
                db::shelves(tx.db(), depot_id, branch_id, owner_id, state).map_err(sql_error)?;
            let mut loader = Loader::new(tx);
            let mut out = Vec::with_capacity(rows.len());
            for r in rows {
                out.push(loader.load(r, false)?);
            }
            Ok(Some(out))
        })
        .await?
        .ok_or_else(|| ApiError::not_found("no such branch"))?;
    Ok(ShelfList {
        shelves: loaded
            .iter()
            .filter(|s| s.visible_to(&access))
            .map(Loaded::view)
            .collect(),
    })
}

async fn show(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
    q: ApiQuery,
) -> ApiResult<Json<ShelfView>> {
    let id = parse_id(&id)?;
    let diff = q.flag("diff")?;
    Ok(Json(get_shelf(&ctx, &caller, &depot, id, diff).await?))
}

/// One shelf with its changes and, with `diff`, a diff per changed file.
pub async fn get_shelf(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    id: i64,
    diff: bool,
) -> ApiResult<ShelfView> {
    caller.require_scope("read")?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let loaded = visible_shelf(ctx, &access, d.id, id, true).await?;
    let mut view = loaded.view();
    if diff {
        view.diff = Some(diffs(ctx, d.id, loaded.changes.as_deref().unwrap_or_default()).await?);
    }
    Ok(view)
}

enum Content {
    Text(String),
    Skipped(&'static str),
}

/// Reads a blob for a diff: text up to [`MAX_DIFF_FILE`], or why not.
async fn content(ctx: &ModuleCtx, depot: DepotId, id: Option<ObjectId>) -> ApiResult<Content> {
    let Some(id) = id else {
        return Ok(Content::Text(String::new()));
    };
    let obj = match ctx.blobs().store().get(depot, &id).await {
        Ok(o) => o,
        Err(lode_store::StoreError::NotFound(_)) => return Ok(Content::Skipped("missing")),
        Err(e) => return Err(e.into()),
    };
    if obj.raw_len > MAX_DIFF_FILE {
        return Ok(Content::Skipped("too_large"));
    }
    let mut buf = Vec::new();
    obj.reader
        .take(MAX_DIFF_FILE)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ApiError::from(lode_store::StoreError::Io(e)))?;
    if buf[..buf.len().min(8192)].contains(&0) {
        return Ok(Content::Skipped("binary"));
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(Content::Text(s)),
        Err(_) => Ok(Content::Skipped("binary")),
    }
}

async fn diffs(
    ctx: &ModuleCtx,
    depot: DepotId,
    changes: &[Change],
) -> ApiResult<Vec<ShelfFileDiff>> {
    let mut out = Vec::with_capacity(changes.len());
    let mut total = 0usize;
    for c in changes {
        let skip = |why: &str| ShelfFileDiff {
            path: c.path.clone(),
            patch: None,
            skipped: Some(why.to_owned()),
        };
        if c.kind == Some(EntryKind::Symlink) {
            out.push(skip("symlink"));
            continue;
        }
        if total >= MAX_DIFF_TOTAL {
            out.push(skip("too_large"));
            continue;
        }
        let old_id = match c.action {
            ChangeAction::Add => None,
            _ => c.base_id,
        };
        let new_id = match c.action {
            ChangeAction::Delete => None,
            _ => c.new_id,
        };
        let (old, new) = match (
            content(ctx, depot, old_id).await?,
            content(ctx, depot, new_id).await?,
        ) {
            (Content::Text(a), Content::Text(b)) => (a, b),
            (Content::Skipped(why), _) | (_, Content::Skipped(why)) => {
                out.push(skip(why));
                continue;
            }
        };
        let from = match c.action {
            ChangeAction::Add => "/dev/null".to_owned(),
            _ => format!("a/{}", c.path),
        };
        let to = match c.action {
            ChangeAction::Delete => "/dev/null".to_owned(),
            _ => format!("b/{}", c.path),
        };
        let mut opts = diffy::DiffOptions::new();
        opts.set_original_filename(from).set_modified_filename(to);
        let patch = opts.create_patch(&old, &new).to_string();
        total += patch.len();
        out.push(ShelfFileDiff {
            path: c.path.clone(),
            patch: Some(patch),
            skipped: None,
        });
    }
    Ok(out)
}

#[derive(serde::Deserialize)]
struct ListShelvesInput {
    depot: String,
    #[serde(flatten)]
    query: ShelvesQuery,
}

#[derive(serde::Deserialize)]
struct GetShelfInput {
    depot: String,
    id: i64,
    #[serde(default)]
    diff: bool,
}

fn bad_input(e: serde_json::Error) -> ApiError {
    ApiError::invalid_request(format!("bad input: {e}"))
}

impl Module for Shelves {
    fn name(&self) -> &'static str {
        NAME
    }

    fn scopes(&self) -> &'static [ScopeDef] {
        SCOPES
    }

    fn migrations(&self) -> &'static [Migration] {
        db::MIGRATIONS
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        Some(
            Router::new()
                .route(paths::SHELVES, get(list).post(create))
                .route(paths::SHELF, get(show).put(replace).delete(remove))
                .route(paths::SHELF_SUBMIT, post(submit))
                .with_state(ctx),
        )
    }

    /// Marks a submitted shelf in the commit's own transaction, so the revision and the
    /// shelf's state cannot disagree. A shelf no longer open at that version rejects it.
    fn in_commit(&self, tx: &WriteTx<'_>, ctx: &CommittedCtx<'_>) -> Result<(), ApiError> {
        let Origin::Module { name, attrs } = ctx.commit.origin else {
            return Ok(());
        };
        if name != NAME {
            return Ok(());
        }
        let num = |k: &str| attrs.get(k).and_then(|v| v.parse::<i64>().ok());
        let (Some(id), Some(version)) = (num(ATTR_ID), num(ATTR_VERSION)) else {
            return Err(ApiError::internal());
        };
        let depot = ctx.commit.depot;
        let marked = db::mark_submitted(tx.conn(), depot, id, version, ctx.rev, ctx.created_at)
            .map_err(sql_error)?;
        if !marked {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "the shelf changed or was submitted meanwhile",
            ));
        }
        tx.set_rev_meta(depot, ctx.rev, REV_META_SHELF, &id.to_string())?;
        let row = db::shelf(tx.conn(), depot, id)
            .map_err(sql_error)?
            .ok_or_else(ApiError::internal)?;
        let loaded = Loader::new(tx).load(row, false)?;
        tx.append_outbox(
            Some(depot),
            "shelf.submitted",
            &event_data(&loaded, Some(ctx.rev)),
            ctx.created_at,
        )?;
        Ok(())
    }

    fn gc_roots(
        &self,
        tx: &dyn Reads,
        depot: DepotId,
        out: &mut dyn FnMut(ObjectId),
    ) -> Result<(), MetaError> {
        for id in db::open_blobs(tx.db(), depot).map_err(sql_error)? {
            out(id);
        }
        Ok(())
    }

    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> {
        let list_ctx = ctx.clone();
        let mut tools = mcp::tools(&ctx);
        tools.extend([
            ToolDef {
                name: "list_shelves".into(),
                description: "List shelves (proposed change sets stored on the server) of a \
                              depot, newest first. Filter by owner, branch and state (open by \
                              default)."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "depot": { "type": "string", "description": "Depot name" },
                        "owner": { "type": "string", "description": "Owner name" },
                        "branch": { "type": "string", "description": "Target branch" },
                        "state": { "type": "string", "enum": ["open", "submitted", "deleted"] }
                    },
                    "required": ["depot"],
                    "additionalProperties": false
                }),
                scope: "read".into(),
                handler: Arc::new(move |caller, input| {
                    let ctx = list_ctx.clone();
                    Box::pin(async move {
                        let input: ListShelvesInput =
                            serde_json::from_value(input).map_err(bad_input)?;
                        let list = list_shelves(&ctx, &caller, &input.depot, input.query).await?;
                        serde_json::to_value(list).map_err(|_| ApiError::internal())
                    })
                }),
            },
            ToolDef {
                name: "get_shelf".into(),
                description: "Get one shelf with its changes: version, owner, base revision \
                              and the change list. With `diff`, also a unified diff per text \
                              file against the base."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "depot": { "type": "string", "description": "Depot name" },
                        "id": { "type": "integer", "minimum": 1, "description": "Shelf id" },
                        "diff": { "type": "boolean", "description": "Include diffs" }
                    },
                    "required": ["depot", "id"],
                    "additionalProperties": false
                }),
                scope: "read".into(),
                handler: Arc::new(move |caller, input| {
                    let ctx = ctx.clone();
                    Box::pin(async move {
                        let input: GetShelfInput =
                            serde_json::from_value(input).map_err(bad_input)?;
                        let shelf =
                            get_shelf(&ctx, &caller, &input.depot, input.id, input.diff).await?;
                        serde_json::to_value(shelf).map_err(|_| ApiError::internal())
                    })
                }),
            },
        ]);
        tools
    }
}
