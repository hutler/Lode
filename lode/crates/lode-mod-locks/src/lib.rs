//! Lode module: exclusive file locks (ARCHITECTURE §9.1, SPEC §6.1).
//!
//! A lock is an exclusive claim by one principal, from one workspace, on one path of one
//! branch. The module refuses commits that touch a path locked by someone other than the
//! commit's author, and commits that modify or delete a path matching `[locks].required`
//! without the author holding its lock. A commit releases the author's locks on the paths it
//! changed, unless the client sends the extension `locks.keep`.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::{ChangeAction, DepotId, DepotPath, EntryKind, ErrorCode};
use lode_meta::{vals, Row, Sql};
use lode_meta::{
    BranchId, BranchState, MetaError, Migration, Millis, PrincipalId, PrincipalKind, Reads, WriteTx,
};
use lode_proto::consts::paths;
use lode_proto::locks::{
    LockList, LockRequest, LockView, LocksQuery, UnlockRequest, UnlockResponse,
};
use lode_proto::PrincipalRef;
use lode_server::commit::{CommitCtx, CommittedCtx};
use lode_server::module::{Module, ModuleCtx, ScopeDef, ToolDef};
use lode_server::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult, Caller, DepotAccess, Level};
use serde_json::json;

/// Module name, as in `[modules]`.
pub const NAME: &str = "locks";
/// Scope needed to take and release locks.
pub const SCOPE_LOCK: &str = "lock";
/// Commit extension that keeps the author's locks after the commit.
pub const KEEP_EXTENSION: &str = "locks.keep";
/// Most paths in one lock or unlock request.
pub const MAX_PATHS: usize = 10_000;
/// Longest workspace id.
pub const MAX_WORKSPACE_LEN: usize = 200;
/// Longest comment, in bytes.
pub const MAX_COMMENT_LEN: usize = 1024;

/// How often a lock request looks the paths up again when commits keep moving the branch.
const HEAD_RETRIES: usize = 5;

const SCOPES: &[ScopeDef] = &[ScopeDef {
    name: SCOPE_LOCK,
    agent_default: true,
}];

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_locks_lock (
              depot_id   INTEGER NOT NULL,
              branch_id  INTEGER NOT NULL,
              path       TEXT NOT NULL,
              owner_id   INTEGER NOT NULL,
              workspace  TEXT NOT NULL,
              comment    TEXT,
              created_at INTEGER NOT NULL,
              PRIMARY KEY (depot_id, branch_id, path)
          ) WITHOUT ROWID;
          CREATE INDEX mod_locks_lock_owner ON mod_locks_lock (owner_id);",
    postgres: "CREATE TABLE mod_locks_lock (
              depot_id   BIGINT NOT NULL,
              branch_id  BIGINT NOT NULL,
              path       TEXT COLLATE \"C\" NOT NULL,
              owner_id   BIGINT NOT NULL,
              workspace  TEXT COLLATE \"C\" NOT NULL,
              comment    TEXT COLLATE \"C\",
              created_at BIGINT NOT NULL,
              PRIMARY KEY (depot_id, branch_id, path)
          );
          CREATE INDEX mod_locks_lock_owner ON mod_locks_lock (owner_id);",
}];

/// The locks module.
#[derive(Debug, Default, Clone, Copy)]
pub struct Locks;

impl Locks {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self)
    }
}

/// One row of `mod_locks_lock`.
#[derive(Debug, Clone)]
struct LockRow {
    branch_id: i64,
    path: String,
    owner_id: i64,
    workspace: String,
    comment: Option<String>,
    created_at: Millis,
}

const COLUMNS: &str = "branch_id, path, owner_id, workspace, comment, created_at";

fn lock_row(r: &Row) -> lode_meta::Result<LockRow> {
    Ok(LockRow {
        branch_id: r.get(0)?,
        path: r.get(1)?,
        owner_id: r.get(2)?,
        workspace: r.get(3)?,
        comment: r.get(4)?,
        created_at: r.get(5)?,
    })
}

fn get_lock(
    conn: &dyn Sql,
    depot: DepotId,
    branch: i64,
    path: &str,
) -> lode_meta::Result<Option<LockRow>> {
    conn.query_opt(
        &format!(
            "SELECT {COLUMNS} FROM mod_locks_lock WHERE depot_id = ?1 AND branch_id = ?2 AND path = ?3"
        ),
        vals![depot.0, branch, path],
        lock_row,
    )
}

/// Every lock of one branch, by path.
fn branch_locks(
    conn: &dyn Sql,
    depot: DepotId,
    branch: i64,
) -> lode_meta::Result<HashMap<String, LockRow>> {
    let rows = conn.query(
        &format!("SELECT {COLUMNS} FROM mod_locks_lock WHERE depot_id = ?1 AND branch_id = ?2"),
        vals![depot.0, branch],
        lock_row,
    )?;
    Ok(rows.into_iter().map(|l| (l.path.clone(), l)).collect())
}

fn count_locks(conn: &dyn Sql, depot: DepotId, branch: i64) -> lode_meta::Result<usize> {
    conn.value::<i64>(
        "SELECT count(*) FROM mod_locks_lock WHERE depot_id = ?1 AND branch_id = ?2",
        vals![depot.0, branch],
    )
    .map(|n| n.max(0) as usize)
}

fn ms_to_time(ms: Millis) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

fn sql_error(e: MetaError) -> ApiError {
    ApiError::from(e)
}

/// Turns rows into views, looking up owner names once each.
struct Viewer<'a> {
    tx: &'a dyn Reads,
    branches: HashMap<i64, String>,
    owners: HashMap<i64, PrincipalRef>,
}

impl<'a> Viewer<'a> {
    fn new(tx: &'a dyn Reads) -> Self {
        Self {
            tx,
            branches: HashMap::new(),
            owners: HashMap::new(),
        }
    }

    fn with_branch(mut self, id: i64, name: &str) -> Self {
        self.branches.insert(id, name.to_owned());
        self
    }

    fn owner(&mut self, id: i64) -> Result<PrincipalRef, MetaError> {
        if let Some(o) = self.owners.get(&id) {
            return Ok(o.clone());
        }
        let p = self.tx.principal(PrincipalId(id))?;
        let r = match p {
            Some(p) => {
                let owner = match (p.kind, p.owner_id) {
                    (PrincipalKind::Agent, Some(o)) => self.tx.principal(o)?.map(|o| o.name),
                    _ => None,
                };
                PrincipalRef {
                    name: p.name,
                    kind: match p.kind {
                        PrincipalKind::User => lode_proto::PrincipalKind::User,
                        PrincipalKind::Agent => lode_proto::PrincipalKind::Agent,
                    },
                    owner,
                }
            }
            None => PrincipalRef {
                name: format!("#{id}"),
                kind: lode_proto::PrincipalKind::User,
                owner: None,
            },
        };
        self.owners.insert(id, r.clone());
        Ok(r)
    }

    fn branch(&mut self, id: i64) -> Result<String, MetaError> {
        if let Some(b) = self.branches.get(&id) {
            return Ok(b.clone());
        }
        let name = self
            .tx
            .branch_by_id(BranchId(id))?
            .map(|b| b.name)
            .unwrap_or_default();
        self.branches.insert(id, name.clone());
        Ok(name)
    }

    fn view(&mut self, row: &LockRow) -> Result<LockView, MetaError> {
        Ok(LockView {
            branch: self.branch(row.branch_id)?,
            path: DepotPath::parse(&row.path)
                .map_err(|e| MetaError::BadValue(format!("lock path {}: {e}", row.path)))?,
            owner: self.owner(row.owner_id)?,
            workspace: row.workspace.clone(),
            comment: row.comment.clone(),
            created_at: ms_to_time(row.created_at),
        })
    }
}

/// `locked`, naming the holders. Every path in it was named by the caller and is visible to
/// them, so the details leak nothing.
fn locked_error(held: &[LockView], mine: &[bool]) -> ApiError {
    let first = &held[0];
    let who = if mine.first().copied().unwrap_or(false) {
        format!("you in another workspace ({})", first.workspace)
    } else {
        format!("{} (workspace {})", first.owner.name, first.workspace)
    };
    let more = match held.len() {
        1 => String::new(),
        n => format!(" and {} more path(s) are locked", n - 1),
    };
    let paths: Vec<&str> = held.iter().map(|l| l.path.as_str()).collect();
    ApiError::new(
        ErrorCode::Locked,
        format!("{} is locked by {who}{more}", first.path),
    )
    .with_details(json!({ "paths": paths, "locks": held }))
}

/// Releases every lock `owner` holds, with one `lock.released` event per branch.
fn release_all_of(tx: &WriteTx<'_>, owner: PrincipalId, now: Millis) -> Result<(), MetaError> {
    let held: Vec<(i64, i64, i64)> = tx.conn().query(
        "SELECT depot_id, branch_id, count(*) FROM mod_locks_lock WHERE owner_id = ?1 \
         GROUP BY depot_id, branch_id",
        vals![owner.0],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if held.is_empty() {
        return Ok(());
    }
    let name = tx
        .principal(owner)?
        .map(|p| p.name)
        .unwrap_or_else(|| format!("#{}", owner.0));
    tx.conn().execute(
        "DELETE FROM mod_locks_lock WHERE owner_id = ?1",
        vals![owner.0],
    )?;
    for (depot, branch, count) in held {
        let Some(b) = tx.branch_by_id(BranchId(branch))? else {
            continue;
        };
        let data = event_data(&b.name, &name, usize::try_from(count).unwrap_or(0));
        tx.append_outbox(Some(DepotId(depot)), "lock.released", &data, now)?;
    }
    Ok(())
}

fn event_data(branch: &str, owner: &str, count: usize) -> String {
    json!({ "branch": branch, "owner": owner, "count": count }).to_string()
}

/// Checks the size of a path list and that no path appears twice.
fn check_paths<'a>(paths: impl ExactSizeIterator<Item = &'a DepotPath>) -> ApiResult<()> {
    let n = paths.len();
    if n == 0 || n > MAX_PATHS {
        return Err(ApiError::invalid_request(format!(
            "a request names 1 to {MAX_PATHS} paths"
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(n);
    for p in paths {
        if !seen.insert(p.as_str()) {
            return Err(ApiError::invalid_request(format!(
                "path {p} appears more than once"
            )));
        }
    }
    Ok(())
}

/// A branch the caller can read something of, or `not_found`.
async fn visible_branch(
    ctx: &ModuleCtx,
    access: &DepotAccess,
    depot: DepotId,
    name: &str,
) -> ApiResult<lode_meta::BranchRow> {
    let not_found = || ApiError::not_found(format!("no branch `{name}`"));
    if !access.branch(name).reads_anything() {
        return Err(not_found());
    }
    let owned = name.to_owned();
    ctx.meta()
        .read(move |tx| tx.branch(depot, &owned))
        .await?
        .ok_or_else(not_found)
}

enum Taken {
    /// A commit moved the branch since the paths were looked up.
    Moved,
    /// Some paths are held by others (or by the caller in another workspace).
    Held(Vec<LockView>, Vec<bool>),
    /// Every path is locked by the caller; the views of the requested paths.
    Done(Vec<LockView>, usize),
}

async fn lock(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<LockRequest>,
) -> ApiResult<Json<LockList>> {
    caller.require_scope(SCOPE_LOCK)?;
    let (d, access) = ctx.depots().resolve(&caller, &depot).await?;
    if req.workspace.trim().is_empty() || req.workspace.len() > MAX_WORKSPACE_LEN {
        return Err(ApiError::invalid_request(format!(
            "workspace must have 1 to {MAX_WORKSPACE_LEN} characters"
        )));
    }
    if req
        .comment
        .as_ref()
        .is_some_and(|c| c.len() > MAX_COMMENT_LEN)
    {
        return Err(ApiError::invalid_request(format!(
            "comment is longer than {MAX_COMMENT_LEN} bytes"
        )));
    }
    check_paths(req.paths.iter().map(|p| &p.path))?;
    let branch = visible_branch(&ctx, &access, d.id, &req.branch).await?;
    if branch.state == BranchState::Archived {
        return Err(ApiError::new(
            ErrorCode::BranchArchived,
            format!("branch `{}` is archived", branch.name),
        ));
    }

    // Paths the caller cannot read do not exist for them; read-only ones are refused.
    let levels = access
        .branch(&req.branch)
        .levels(req.paths.iter().map(|p| &p.path));
    let hidden: Vec<&str> = req
        .paths
        .iter()
        .zip(&levels)
        .filter(|(_, l)| **l < Level::Read)
        .map(|(p, _)| p.path.as_str())
        .collect();
    if !hidden.is_empty() {
        return Err(not_at_head(&hidden));
    }
    let denied: Vec<&str> = req
        .paths
        .iter()
        .zip(&levels)
        .filter(|(_, l)| **l < Level::Write)
        .map(|(p, _)| p.path.as_str())
        .collect();
    if !denied.is_empty() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!("no write access to {} path(s)", denied.len()),
        )
        .with_details(json!({ "paths": denied })));
    }

    let me = caller.principal.clone();
    let comment = req.comment.filter(|c| !c.trim().is_empty());
    for _ in 0..HEAD_RETRIES {
        let name = req.branch.clone();
        let depot_id = d.id;
        let head = ctx
            .meta()
            .read(move |tx| {
                let Some(b) = tx.branch(depot_id, &name)? else {
                    return Ok(None);
                };
                Ok(tx.revision(depot_id, b.head_rev)?.map(|r| (b, r.root_tree)))
            })
            .await?;
        let Some((b, root)) = head else {
            return Err(ApiError::not_found(format!("no branch `{}`", req.branch)));
        };

        let mut missing = Vec::new();
        let mut stale = Vec::new();
        for p in &req.paths {
            let entry = ctx
                .depots()
                .lookup(d.id, &root, &p.path)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "cannot read a tree while locking");
                    ApiError::internal()
                })?;
            match entry {
                Some(e) if e.kind != EntryKind::Dir => {
                    if e.id != p.base_id {
                        stale.push(p.path.as_str());
                    }
                }
                _ => missing.push(p.path.as_str()),
            }
        }
        if !missing.is_empty() {
            return Err(not_at_head(&missing));
        }
        if !stale.is_empty() {
            return Err(ApiError::new(
                ErrorCode::OutOfDate,
                format!(
                    "{} path(s) changed on the branch since your version; update first",
                    stale.len()
                ),
            )
            .with_details(json!({ "paths": stale })));
        }

        let now = ctx.clock().now_ms();
        let paths: Vec<String> = req
            .paths
            .iter()
            .map(|p| p.path.as_str().to_owned())
            .collect();
        let workspace = req.workspace.clone();
        let comment = comment.clone();
        let me = me.clone();
        let (branch_id, branch_name, expected) = (b.id.0, b.name.clone(), b.head_rev);
        let taken = ctx
            .meta()
            .store()
            .write(move |tx| {
                let now_head = tx.branch_by_id(BranchId(branch_id))?.map(|b| b.head_rev);
                if now_head != Some(expected) {
                    return Ok(Taken::Moved);
                }
                let mut viewer = Viewer::new(tx).with_branch(branch_id, &branch_name);
                let mut held = Vec::new();
                let mut mine = Vec::new();
                let mut fresh = Vec::new();
                for p in &paths {
                    match get_lock(tx.conn(), depot_id, branch_id, p)? {
                        Some(l) if l.owner_id == me.id.0 && l.workspace == workspace => {}
                        Some(l) => {
                            mine.push(l.owner_id == me.id.0);
                            held.push(viewer.view(&l)?);
                        }
                        None => fresh.push(p),
                    }
                }
                if !held.is_empty() {
                    return Ok(Taken::Held(held, mine));
                }
                {
                    let ins = format!(
                        "INSERT INTO mod_locks_lock (depot_id, {COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                    );
                    for p in &fresh {
                        tx.conn().execute(
                            &ins,
                            vals![depot_id.0, branch_id, *p, me.id.0, &workspace, &comment, now],
                        )?;
                    }
                }
                if !fresh.is_empty() {
                    let data = event_data(&branch_name, &me.name, fresh.len());
                    tx.append_outbox(Some(depot_id), "lock.acquired", &data, now)?;
                }
                let mut views = Vec::with_capacity(paths.len());
                for p in &paths {
                    if let Some(l) = get_lock(tx.conn(), depot_id, branch_id, p)? {
                        views.push(viewer.view(&l)?);
                    }
                }
                Ok(Taken::Done(views, fresh.len()))
            })
            .await?;
        match taken {
            Taken::Moved => continue,
            Taken::Held(held, mine) => return Err(locked_error(&held, &mine)),
            Taken::Done(locks, fresh) => {
                if fresh > 0 {
                    ctx.notify_events();
                }
                return Ok(Json(LockList { locks }));
            }
        }
    }
    Err(ApiError::new(
        ErrorCode::Conflict,
        "the branch kept changing while the locks were taken; try again",
    ))
}

fn not_at_head(paths: &[&str]) -> ApiError {
    ApiError::not_found(format!(
        "{} is not a file at the branch head",
        paths.first().copied().unwrap_or_default()
    ))
    .with_details(json!({ "paths": paths }))
}

enum Released {
    Held(Vec<LockView>, Vec<bool>),
    Done(Vec<DepotPath>),
}

async fn unlock(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<UnlockRequest>,
) -> ApiResult<Json<UnlockResponse>> {
    caller.require_scope(SCOPE_LOCK)?;
    let (d, access) = ctx.depots().resolve(&caller, &depot).await?;
    check_paths(req.paths.iter())?;
    if req.force && !access.is_depot_admin() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "breaking locks needs admin on the depot",
        ));
    }
    let b = visible_branch(&ctx, &access, d.id, &req.branch).await?;
    let readable: Vec<bool> = access
        .branch(&req.branch)
        .levels(req.paths.iter())
        .into_iter()
        .map(|l| l >= Level::Read)
        .collect();
    let me = caller.principal.clone();
    let now = ctx.clock().now_ms();
    let paths = req.paths.clone();
    let force = req.force;
    let (depot_id, branch_id, branch_name) = (d.id, b.id.0, b.name.clone());
    let out = ctx
        .meta()
        .store()
        .write(move |tx| {
            let mut viewer = Viewer::new(tx).with_branch(branch_id, &branch_name);
            let mut held = Vec::new();
            let mut mine = Vec::new();
            let mut release: Vec<(DepotPath, i64)> = Vec::new();
            for (p, readable) in paths.iter().zip(readable) {
                match get_lock(tx.conn(), depot_id, branch_id, p.as_str())? {
                    Some(l) if l.owner_id == me.id.0 || force => release.push((p.clone(), l.owner_id)),
                    // Someone else's lock on a path the caller cannot see is left alone
                    // without saying so.
                    Some(l) if readable => {
                        mine.push(false);
                        held.push(viewer.view(&l)?);
                    }
                    _ => {}
                }
            }
            if !held.is_empty() {
                return Ok(Released::Held(held, mine));
            }
            let mut per_owner: BTreeMap<i64, usize> = BTreeMap::new();
            {
                for (p, owner) in &release {
                    tx.conn().execute(
                        "DELETE FROM mod_locks_lock WHERE depot_id = ?1 AND branch_id = ?2 AND path = ?3",
                        vals![depot_id.0, branch_id, p.as_str()],
                    )?;
                    *per_owner.entry(*owner).or_default() += 1;
                }
            }
            for (owner, count) in per_owner {
                let name = viewer.owner(owner)?.name;
                let data = event_data(&branch_name, &name, count);
                tx.append_outbox(Some(depot_id), "lock.released", &data, now)?;
            }
            Ok(Released::Done(release.into_iter().map(|(p, _)| p).collect()))
        })
        .await?;
    match out {
        Released::Held(held, mine) => Err(locked_error(&held, &mine)),
        Released::Done(released) => {
            if !released.is_empty() {
                ctx.notify_events();
            }
            Ok(Json(UnlockResponse { released }))
        }
    }
}

async fn list(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<LockList>> {
    let query = LocksQuery {
        branch: q.get("branch").map(str::to_owned),
        prefix: q.get("prefix").map(str::to_owned),
        owner: q.get("owner").map(str::to_owned),
    };
    Ok(Json(list_locks(&ctx, &caller, &depot, query).await?))
}

/// The locks of a depot that `caller` can see, filtered by `q`, ordered by branch and path.
/// `prefix` is a plain string prefix of the path (`Content/` for a directory).
pub async fn list_locks(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    q: LocksQuery,
) -> ApiResult<LockList> {
    caller.require_scope("read")?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let branch = match q.branch.as_deref().filter(|b| !b.is_empty()) {
        Some(name) => Some(visible_branch(ctx, &access, d.id, name).await?.id.0),
        None => None,
    };
    let prefix = q.prefix.filter(|p| !p.is_empty());
    let owner = q.owner.filter(|o| !o.is_empty());
    let depot_id = d.id;
    let rows = ctx
        .meta()
        .read(move |tx| {
            let owner_id = match owner {
                Some(name) => match tx.principal_by_name(&name)? {
                    Some(p) => Some(p.id.0),
                    None => return Ok(Vec::new()),
                },
                None => None,
            };
            let rows = tx.db().query(
                &format!(
                    "SELECT {COLUMNS} FROM mod_locks_lock
                     WHERE depot_id = ?1
                       AND (CAST(?2 AS BIGINT) IS NULL OR branch_id = ?2)
                       AND (CAST(?3 AS BIGINT) IS NULL OR owner_id = ?3)
                       AND (CAST(?4 AS TEXT) IS NULL OR substr(path, 1, length(?4)) = ?4)
                     ORDER BY branch_id, path"
                ),
                vals![depot_id.0, branch, owner_id, &prefix],
                lock_row,
            )?;
            let mut viewer = Viewer::new(tx);
            let mut out = Vec::with_capacity(rows.len());
            for r in &rows {
                out.push(viewer.view(r)?);
            }
            Ok(out)
        })
        .await?;
    let mut per_branch: HashMap<String, lode_server::BranchAccess> = HashMap::new();
    let locks = rows
        .into_iter()
        .filter(|l| {
            per_branch
                .entry(l.branch.clone())
                .or_insert_with(|| access.branch(&l.branch))
                .allows(&l.path, Level::Read)
        })
        .collect();
    Ok(LockList { locks })
}

#[derive(serde::Deserialize)]
struct ListLocksInput {
    depot: String,
    #[serde(flatten)]
    query: LocksQuery,
}

impl Module for Locks {
    fn name(&self) -> &'static str {
        NAME
    }

    fn purge_depot(&self, tx: &WriteTx<'_>, depot: DepotId) -> Result<(), MetaError> {
        tx.conn().execute(
            "DELETE FROM mod_locks_lock WHERE depot_id = ?1",
            vals![depot.0],
        )?;
        Ok(())
    }

    /// A deleted principal's locks would block everyone else for good, so they go with it.
    fn principal_deleted(
        &self,
        tx: &WriteTx<'_>,
        principal: PrincipalId,
        now: Millis,
    ) -> Result<(), MetaError> {
        release_all_of(tx, principal, now)
    }

    fn scopes(&self) -> &'static [ScopeDef] {
        SCOPES
    }

    fn migrations(&self) -> &'static [Migration] {
        MIGRATIONS
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        Some(
            Router::new()
                .route(paths::LOCKS, get(list).post(lock).delete(unlock))
                .with_state(ctx),
        )
    }

    fn pre_commit(&self, tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
        if ctx.changes.is_empty() {
            return Ok(());
        }
        let conn = tx.db();
        let branch = ctx.branch.id.0;
        let total = count_locks(conn, ctx.depot, branch).map_err(sql_error)?;
        let required = &ctx.branch_config.lock_required;
        if total == 0 && required.is_empty() {
            return Ok(());
        }
        // Few locks: read them all. Many: look up each changed path.
        let all = if total <= ctx.changes.len() {
            Some(branch_locks(conn, ctx.depot, branch).map_err(sql_error)?)
        } else {
            None
        };
        let author = ctx.author.id.0;
        let mut viewer = Viewer::new(tx).with_branch(branch, &ctx.branch.name);
        let mut held = Vec::new();
        let mut needed = Vec::new();
        for c in ctx.changes {
            let found = if total == 0 {
                None
            } else if let Some(all) = &all {
                all.get(c.path.as_str()).cloned()
            } else {
                get_lock(conn, ctx.depot, branch, c.path.as_str()).map_err(sql_error)?
            };
            match found {
                Some(l) if l.owner_id != author => held.push(viewer.view(&l)?),
                Some(_) => {}
                None if c.action != ChangeAction::Add && required.matches(&c.path) => {
                    needed.push(c.path.as_str())
                }
                None => {}
            }
        }
        if !held.is_empty() {
            let mine = vec![false; held.len()];
            return Err(locked_error(&held, &mine));
        }
        if !needed.is_empty() {
            return Err(ApiError::new(
                ErrorCode::LockRequired,
                format!(
                    "{} needs a lock before it can be changed (`[locks] required` in .lodeconfig){}",
                    needed[0],
                    match needed.len() {
                        1 => String::new(),
                        n => format!(", and {} more path(s)", n - 1),
                    }
                ),
            )
            .with_details(json!({ "paths": needed })));
        }
        Ok(())
    }

    fn in_commit(&self, tx: &WriteTx<'_>, ctx: &CommittedCtx<'_>) -> Result<(), ApiError> {
        let c = ctx.commit;
        let keep = c
            .extensions
            .get(KEEP_EXTENSION)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if keep || c.changes.is_empty() {
            return Ok(());
        }
        let mut released = 0;
        for ch in c.changes {
            released += tx
                .conn()
                .execute(
                    "DELETE FROM mod_locks_lock
                     WHERE depot_id = ?1 AND branch_id = ?2 AND path = ?3 AND owner_id = ?4",
                    vals![c.depot.0, c.branch.id.0, ch.path.as_str(), c.author.id.0],
                )
                .map_err(sql_error)?;
        }
        if released > 0 {
            let data = event_data(&c.branch.name, &c.author.name, released);
            tx.append_outbox(Some(c.depot), "lock.released", &data, ctx.created_at)?;
        }
        Ok(())
    }

    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "list_locks".into(),
            description: "List the file locks of a depot: who holds which path on which branch, \
                          from which workspace, since when and why. Filter by branch, path \
                          prefix (such as `Content/`) and owner name."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "depot": { "type": "string", "description": "Depot name" },
                    "branch": { "type": "string", "description": "Only this branch" },
                    "prefix": { "type": "string", "description": "Only paths starting with this text" },
                    "owner": { "type": "string", "description": "Only locks held by this principal" }
                },
                "required": ["depot"],
                "additionalProperties": false
            }),
            scope: "read".into(),
            handler: Arc::new(move |caller, input| {
                let ctx = ctx.clone();
                Box::pin(async move {
                    let input: ListLocksInput = serde_json::from_value(input)
                        .map_err(|e| ApiError::invalid_request(format!("bad input: {e}")))?;
                    let list = list_locks(&ctx, &caller, &input.depot, input.query).await?;
                    serde_json::to_value(list).map_err(|_| ApiError::internal())
                })
            }),
        }]
    }
}
