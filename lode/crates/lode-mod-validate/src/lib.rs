//! Lode module: validation (SPEC §6.8, ARCHITECTURE §9.7). Off by default.
//!
//! On every new shelf version, runs the Python scripts that the target branch head's
//! `.lodeconfig` lists under `[[validate.script]]` (those with `server = true`) on the
//! shelf's changed files, and reports each as the check `validate/<name>` from the service
//! user `lode-validate`. On branches in `[checks].branches`, a shelf submit needs every such
//! script to have passed on that shelf version.
//!
//! The scripts, their `support` files and the rule list always come from the branch head,
//! never from the shelf, so a shelf cannot change what it is checked with. This is not a
//! sandbox: the scripts run as the server's own user.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::{
    ChangeAction, DepotId, DepotPath, EntryKind, ErrorCode, LodeConfig, ObjectId, PathGlobSet,
    ValidateScript,
};
use lode_meta::{
    vals, MetaError, Migration, NewPrincipal, PrincipalKind, PrincipalRow, Reads, WriteTx,
};
use lode_mod_checks::{ATTR_SHELF_ID, ATTR_SHELF_VERSION, SHELVES_ORIGIN};
use lode_proto::checks::{CheckState, CheckTarget, CheckTargetKind, ReportCheckRequest};
use lode_proto::shelves::{ShelfState, ShelfView};
use lode_server::commit::{CommitCtx, Origin};
use lode_server::module::{BoxError, BoxFuture, Event, Module, ModuleCtx, Stop};
use lode_server::{ApiError, ApiPath, ApiResult, Caller};
use lode_validate::{input_files, matching, Input, Outcome, Python, ScriptResult};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::{Notify, Semaphore};

/// Module name, as in `[modules]`.
pub const NAME: &str = "validate";
/// The service user that reports the checks. Disabled, so it can never sign in.
pub const SERVICE_USER: &str = "lode-validate";
/// Prefix of the checks this module reports.
pub const CHECK_PREFIX: &str = "validate/";
/// `GET` the full results of one shelf version.
pub const RESULTS_PATH: &str = "/api/v1/depots/{d}/validate/{shelf}/{version}";
/// How often the worker looks for queued runs without being woken.
const IDLE_POLL: Duration = Duration::from_secs(30);
/// Longest check summary (the checks module's limit).
const SUMMARY_MAX: usize = 1024;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_validate_run (
              depot_id      INTEGER NOT NULL,
              shelf_id      INTEGER NOT NULL,
              shelf_version INTEGER NOT NULL,
              state         TEXT NOT NULL,
              results       TEXT,
              created_at    INTEGER NOT NULL,
              updated_at    INTEGER NOT NULL,
              PRIMARY KEY (depot_id, shelf_id, shelf_version)
          ) WITHOUT ROWID;",
    postgres: "CREATE TABLE mod_validate_run (
              depot_id      BIGINT NOT NULL,
              shelf_id      BIGINT NOT NULL,
              shelf_version BIGINT NOT NULL,
              state         TEXT COLLATE \"C\" NOT NULL,
              results       TEXT COLLATE \"C\",
              created_at    BIGINT NOT NULL,
              updated_at    BIGINT NOT NULL,
              PRIMARY KEY (depot_id, shelf_id, shelf_version)
          );",
}];

/// Run states in `mod_validate_run.state`.
mod state {
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const SUPERSEDED: &str = "superseded";
}

#[derive(Default)]
struct Inner {
    wake: Notify,
    /// Runs in progress by (depot, shelf), with the version and a handle to stop them.
    running: Mutex<HashMap<(i64, i64), (i64, tokio::task::AbortHandle)>>,
}

/// The validation module.
#[derive(Clone, Default)]
pub struct Validate {
    inner: Arc<Inner>,
}

impl Validate {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self::default())
    }
}

fn sql_error(e: MetaError) -> ApiError {
    ApiError::from(e)
}

/// The shelf id and version of a shelf submit, or `None` for any other commit.
fn submitted_shelf(origin: &Origin) -> Option<(u64, u64)> {
    let Origin::Module { name, attrs } = origin else {
        return None;
    };
    if name != SHELVES_ORIGIN {
        return None;
    }
    let num = |k: &str| attrs.get(k).and_then(|v| v.parse::<u64>().ok());
    Some((num(ATTR_SHELF_ID)?, num(ATTR_SHELF_VERSION)?))
}

/// The scripts the server runs.
fn server_scripts(config: &LodeConfig) -> Vec<&ValidateScript> {
    config
        .validate
        .scripts
        .iter()
        .filter(|s| s.server)
        .collect()
}

/// The submit gate: on `[checks].branches`, every server-side script must have passed (or
/// had nothing to check) on the submitted shelf version.
fn gate(tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
    let config = ctx.branch_config;
    let scripts = server_scripts(config);
    if scripts.is_empty() || !config.checks_branches.matches_str(&ctx.branch.name) {
        return Ok(());
    }
    let Some((shelf, version)) = submitted_shelf(ctx.origin) else {
        if matches!(ctx.origin, Origin::Module { name, .. } if name == SHELVES_ORIGIN) {
            return Err(ApiError::new(
                ErrorCode::ChecksRequired,
                "this shelf submit does not name its shelf version, so its validation cannot be verified",
            ));
        }
        return Ok(());
    };
    let row = tx
        .db()
        .query_opt(
            "SELECT state, results FROM mod_validate_run
             WHERE depot_id = ?1 AND shelf_id = ?2 AND shelf_version = ?3",
            vals![ctx.depot.0, to_i64(shelf), to_i64(version)],
            |r| Ok((r.get::<String>(0)?, r.get::<Option<String>>(1)?)),
        )
        .map_err(sql_error)?;
    let results: Vec<ScriptResult> = match &row {
        Some((s, Some(text))) if s == state::DONE => serde_json::from_str(text).unwrap_or_default(),
        _ => Vec::new(),
    };
    let done = matches!(&row, Some((s, _)) if s == state::DONE);
    let mut missing = Vec::new();
    for s in scripts {
        let name = format!("{CHECK_PREFIX}{}", s.name);
        match results.iter().find(|r| r.name == s.name) {
            Some(r) if matches!(r.outcome, Outcome::Passed | Outcome::Skipped) => {}
            Some(r) => missing.push(json!({
                "name": name, "reason": "not_success", "state": "failure",
                "reporter": SERVICE_USER, "summary": r.summary(SUMMARY_MAX),
            })),
            None if !done && row.is_some() => missing.push(json!({
                "name": name, "reason": "not_success", "state": "pending",
                "reporter": SERVICE_USER,
            })),
            None => missing.push(json!({
                "name": name, "reason": "absent", "reporter": SERVICE_USER,
            })),
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = missing.iter().filter_map(|m| m["name"].as_str()).collect();
    Err(ApiError::new(
        ErrorCode::ChecksRequired,
        format!(
            "shelf {shelf} version {version} has not passed the validation `{}` requires: {}",
            ctx.branch.name,
            names.join(", ")
        ),
    )
    .with_details(json!({ "shelf_id": shelf, "shelf_version": version, "missing": missing })))
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Queues a run for a new shelf version and marks older queued ones superseded.
fn enqueue(
    tx: &WriteTx<'_>,
    depot: i64,
    shelf: i64,
    version: i64,
    now: i64,
) -> Result<(), MetaError> {
    tx.conn().execute(
        "UPDATE mod_validate_run SET state = ?4, updated_at = ?5
         WHERE depot_id = ?1 AND shelf_id = ?2 AND shelf_version < ?3 AND state IN ('queued', 'running')",
        vals![depot, shelf, version, state::SUPERSEDED, now],
    )?;
    tx.conn().execute(
        "INSERT INTO mod_validate_run (depot_id, shelf_id, shelf_version, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT (depot_id, shelf_id, shelf_version) DO NOTHING",
        vals![depot, shelf, version, state::QUEUED, now],
    )?;
    Ok(())
}

impl Module for Validate {
    fn name(&self) -> &'static str {
        NAME
    }

    fn migrations(&self) -> &'static [Migration] {
        MIGRATIONS
    }

    fn purge_depot(&self, tx: &WriteTx<'_>, depot: DepotId) -> Result<(), MetaError> {
        tx.conn().execute(
            "DELETE FROM mod_validate_run WHERE depot_id = ?1",
            vals![depot.0],
        )?;
        Ok(())
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        Some(
            Router::new()
                .route(RESULTS_PATH, get(results))
                .with_state(ctx),
        )
    }

    fn pre_commit(&self, tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
        gate(tx, ctx)
    }

    fn on_event<'a>(
        &'a self,
        ctx: &'a ModuleCtx,
        event: &'a Event,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            if event.event != "shelf.created" && event.event != "shelf.updated" {
                return Ok(());
            }
            let (Some(depot), Some(shelf), Some(version)) = (
                event.depot.clone(),
                event.data.get("shelf_id").and_then(Value::as_i64),
                event.data.get("version").and_then(Value::as_i64),
            ) else {
                return Ok(());
            };
            let now = ctx.clock().now_ms();
            ctx.meta()
                .store()
                .write(move |tx| {
                    let Some(d) = tx.depot_by_name(&depot)? else {
                        return Ok(());
                    };
                    enqueue(tx, d.id.0, shelf, version, now)
                })
                .await?;
            // A run of an older version is no longer worth finishing.
            if let Ok(mut running) = self.inner.running.lock() {
                running.retain(|_, (v, handle)| {
                    if *v < version {
                        handle.abort();
                        false
                    } else {
                        true
                    }
                });
            }
            self.inner.wake.notify_one();
            Ok(())
        })
    }

    fn worker(&self, ctx: ModuleCtx, mut stop: Stop) -> Option<BoxFuture<'static, ()>> {
        let this = self.clone();
        Some(Box::pin(async move {
            let setup = match Setup::new(&ctx).await {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::error!(error = %e, "validation module could not start; shelves are not validated");
                    return;
                }
            };
            loop {
                if stop.is_stopped() {
                    return;
                }
                match this.start_queued(&ctx, &setup).await {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => tracing::error!(error = %e, "validation round failed"),
                }
                tokio::select! {
                    () = stop.wait() => return,
                    () = this.inner.wake.notified() => {}
                    () = tokio::time::sleep(IDLE_POLL) => {}
                }
            }
        }))
    }
}

/// What the worker needs for every run.
struct Setup {
    reporter: PrincipalRow,
    python: Result<Python, String>,
    work_dir: PathBuf,
    max_input: u64,
    slots: Arc<Semaphore>,
}

impl Setup {
    async fn new(ctx: &ModuleCtx) -> Result<Self, BoxError> {
        let now = ctx.clock().now_ms();
        let reporter = ctx
            .meta()
            .store()
            .write(move |tx| {
                let p = match tx.principal_by_name(SERVICE_USER)? {
                    Some(p) => p,
                    None => {
                        let id = tx.create_principal(
                            &NewPrincipal {
                                kind: PrincipalKind::User,
                                name: SERVICE_USER.to_owned(),
                                display_name: Some("Validation scripts".to_owned()),
                                owner_id: None,
                                password_hash: None,
                            },
                            now,
                        )?;
                        tx.principal(id)?
                            .ok_or_else(|| MetaError::NotFound(SERVICE_USER.into()))?
                    }
                };
                if !p.disabled {
                    tx.set_principal_disabled(p.id, true)?;
                }
                // Runs cut short by a restart start over.
                tx.conn().execute(
                    "UPDATE mod_validate_run SET state = ?1 WHERE state = ?2",
                    vals![state::QUEUED, state::RUNNING],
                )?;
                Ok(p)
            })
            .await?;
        let cfg = &ctx.config().validate;
        let python = match &cfg.python {
            None => Err(
                "no Python is set up on the server ([validate] python in loded.toml)".to_owned(),
            ),
            Some(cmd) => {
                let argv = cmd.argv();
                match lode_validate::probe(&argv).await {
                    Some(version) => Ok(Python {
                        command: argv,
                        version,
                    }),
                    None => Err(format!(
                        "{} does not run Python 3 on the server",
                        argv.join(" ")
                    )),
                }
            }
        };
        if let Err(e) = &python {
            tracing::error!("validation module: {e}");
        }
        let work_dir = cfg
            .work_dir
            .clone()
            .unwrap_or_else(|| ctx.config().data_dir.join("validate"));
        std::fs::create_dir_all(&work_dir)?;
        Ok(Self {
            reporter,
            python,
            work_dir,
            max_input: cfg.max_input_mib.saturating_mul(1024 * 1024),
            slots: Arc::new(Semaphore::new(cfg.workers.max(1))),
        })
    }
}

impl Validate {
    /// Starts queued runs while there are free slots. Answers whether it started any.
    async fn start_queued(&self, ctx: &ModuleCtx, setup: &Arc<Setup>) -> Result<bool, BoxError> {
        let mut started = false;
        loop {
            let Ok(permit) = setup.slots.clone().try_acquire_owned() else {
                return Ok(started);
            };
            let now = ctx.clock().now_ms();
            let picked = ctx
                .meta()
                .store()
                .write(move |tx| {
                    let next = tx.conn().query_opt(
                        "SELECT depot_id, shelf_id, shelf_version FROM mod_validate_run
                         WHERE state = ?1 ORDER BY created_at, shelf_id LIMIT 1",
                        vals![state::QUEUED],
                        |r| Ok((r.get::<i64>(0)?, r.get::<i64>(1)?, r.get::<i64>(2)?)),
                    )?;
                    if let Some((d, s, v)) = next {
                        tx.conn().execute(
                            "UPDATE mod_validate_run SET state = ?4, updated_at = ?5
                             WHERE depot_id = ?1 AND shelf_id = ?2 AND shelf_version = ?3",
                            vals![d, s, v, state::RUNNING, now],
                        )?;
                    }
                    Ok(next)
                })
                .await?;
            let Some((d, s, v)) = picked else {
                return Ok(started);
            };
            started = true;
            let ctx2 = ctx.clone();
            let setup2 = setup.clone();
            let inner = self.inner.clone();
            let task = tokio::spawn(async move {
                let _permit = permit;
                let depot = DepotId(d);
                let results = match run_one(&ctx2, &setup2, depot, s, v).await {
                    Ok(results) => results,
                    Err(e) => {
                        tracing::error!(error = %e, depot = d, shelf = s, version = v, "validation run failed");
                        Vec::new()
                    }
                };
                if let Err(e) = mark_done(&ctx2, &setup2.reporter, depot, s, v, &results).await {
                    // Keep the run recoverable by the startup requeue after a storage failure.
                    tracing::error!(error = %e, depot = d, shelf = s, version = v, "could not publish validation results");
                }
                if let Ok(mut r) = inner.running.lock() {
                    if r.get(&(d, s)).is_some_and(|(rv, _)| *rv == v) {
                        r.remove(&(d, s));
                    }
                }
                inner.wake.notify_one();
            });
            if let Ok(mut r) = self.inner.running.lock() {
                r.insert((d, s), (v, task.abort_handle()));
            }
        }
    }
}

/// The branch head's root tree and `.lodeconfig`.
async fn head_config(
    ctx: &ModuleCtx,
    depot: DepotId,
    branch: &str,
) -> Result<Option<(ObjectId, LodeConfig)>, BoxError> {
    let name = branch.to_owned();
    let root = ctx
        .meta()
        .read(move |tx| {
            let Some(b) = tx.branch(depot, &name)? else {
                return Ok(None);
            };
            Ok(tx.revision(depot, b.head_rev)?.map(|r| r.root_tree))
        })
        .await?;
    let Some(root) = root else { return Ok(None) };
    let entry = ctx
        .depots()
        .lookup(depot, &root, &LodeConfig::depot_path())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let config = match entry.filter(|e| e.kind == EntryKind::File) {
        None => LodeConfig::default(),
        Some(e) => {
            let bytes = read_blob(ctx, depot, &e.id, 1024 * 1024).await?;
            LodeConfig::parse(&bytes).unwrap_or_default()
        }
    };
    Ok(Some((root, config)))
}

async fn read_blob(
    ctx: &ModuleCtx,
    depot: DepotId,
    id: &ObjectId,
    max: u64,
) -> Result<Vec<u8>, BoxError> {
    let obj = ctx.blobs().store().get(depot, id).await?;
    let mut buf = Vec::new();
    obj.reader.take(max).read_to_end(&mut buf).await?;
    Ok(buf)
}

/// Writes a blob to `path`, creating directories.
async fn write_blob(
    ctx: &ModuleCtx,
    depot: DepotId,
    id: &ObjectId,
    path: &Path,
) -> Result<(), BoxError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let obj = ctx.blobs().store().get(depot, id).await?;
    let mut reader = obj.reader;
    let mut file = tokio::fs::File::create(path).await?;
    tokio::io::copy(&mut reader, &mut file).await?;
    Ok(())
}

/// The literal leading directories of a pattern, where a walk for it can start.
fn literal_prefix(pattern: &str) -> String {
    let mut out = Vec::new();
    let segs: Vec<&str> = pattern.split('/').collect();
    for (i, seg) in segs.iter().enumerate() {
        if i + 1 == segs.len() || seg.contains(['*', '?', '[', '{']) {
            break;
        }
        out.push(*seg);
    }
    out.join("/")
}

/// Every file below `dir` in the tree at `root` whose path matches `globs`.
async fn walk(
    ctx: &ModuleCtx,
    depot: DepotId,
    root: &ObjectId,
    dir: &str,
    globs: &PathGlobSet,
    out: &mut Vec<(DepotPath, ObjectId, u64)>,
) -> Result<(), BoxError> {
    let start = if dir.is_empty() {
        Some(*root)
    } else {
        let p = DepotPath::parse(dir).map_err(|e| format!("{e:?}"))?;
        match ctx
            .depots()
            .lookup(depot, root, &p)
            .await
            .map_err(|e| format!("{e:?}"))?
        {
            Some(e) if e.kind == EntryKind::Dir => Some(e.id),
            _ => None,
        }
    };
    let Some(start) = start else { return Ok(()) };
    let mut stack = vec![(dir.to_owned(), start)];
    while let Some((prefix, id)) = stack.pop() {
        let tree = ctx
            .depots()
            .tree(depot, &id)
            .await
            .map_err(|e| format!("{e:?}"))?;
        for e in tree.entries() {
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            match e.kind {
                EntryKind::Dir => stack.push((path, e.id)),
                EntryKind::File if globs.matches_str(&path) => {
                    if let Ok(p) = DepotPath::parse(&path) {
                        out.push((p, e.id, e.size));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn shelf_check(
    shelf: u64,
    version: u64,
    name: &str,
    state: CheckState,
    summary: String,
) -> ReportCheckRequest {
    ReportCheckRequest {
        target: CheckTarget {
            kind: CheckTargetKind::Shelf,
            id: shelf,
            version: Some(version),
        },
        name: format!("{CHECK_PREFIX}{name}"),
        state,
        summary: Some(summary),
        url: None,
    }
}

async fn mark_done(
    ctx: &ModuleCtx,
    reporter: &PrincipalRow,
    depot: DepotId,
    shelf: i64,
    version: i64,
    results: &[ScriptResult],
) -> Result<(), BoxError> {
    let text = serde_json::to_string(results)?;
    let mut checks = Vec::new();
    for result in results {
        let state = match result.outcome {
            Outcome::Skipped => continue,
            Outcome::Passed => CheckState::Success,
            Outcome::Failed | Outcome::Error => CheckState::Failure,
        };
        let req = shelf_check(
            u64::try_from(shelf)?,
            u64::try_from(version)?,
            &result.name,
            state,
            result.summary(SUMMARY_MAX),
        );
        checks.push(lode_mod_checks::PreparedCheck::new(req).map_err(|e| format!("{e:?}"))?);
    }
    let reporter = reporter.clone();
    let now = ctx.clock().now_ms();
    ctx.meta()
        .store()
        .write(move |tx| {
            let changed = tx.conn().execute(
                "UPDATE mod_validate_run SET state = ?4, results = ?5, updated_at = ?6
             WHERE depot_id = ?1 AND shelf_id = ?2 AND shelf_version = ?3 AND state = 'running'",
                vals![depot.0, shelf, version, state::DONE, &text, now],
            )?;
            if changed > 0 {
                for check in checks {
                    check.store_in(tx, depot, &reporter, now)?;
                }
            }
            Ok(())
        })
        .await?;
    ctx.notify_events();
    Ok(())
}

/// One run: the shelf version's changed files and the head's scripts in a temporary tree.
async fn run_one(
    ctx: &ModuleCtx,
    setup: &Setup,
    depot: DepotId,
    shelf_id: i64,
    version: i64,
) -> Result<Vec<ScriptResult>, BoxError> {
    let shelf_u = u64::try_from(shelf_id)?;
    let version_u = u64::try_from(version)?;
    let shelf: Option<ShelfView> = lode_mod_shelves::shelf_for_module(ctx, depot, shelf_u)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let Some(shelf) = shelf.filter(|s| s.version == version_u && s.state == ShelfState::Open)
    else {
        return Ok(Vec::new());
    };
    let Some((root, config)) = head_config(ctx, depot, &shelf.branch).await? else {
        return Ok(Vec::new());
    };
    let scripts = server_scripts(&config);
    let changes = shelf.changes.clone().unwrap_or_default();
    let files = input_files(&changes);
    let mut results = Vec::new();
    let mut todo = Vec::new();
    for s in scripts {
        let m = matching(&files, &s.paths);
        if m.is_empty() {
            results.push(ScriptResult::skipped(&s.name, "no matching files"));
        } else {
            let req = shelf_check(
                shelf_u,
                version_u,
                &s.name,
                CheckState::Pending,
                format!("checking {} files", m.len()),
            );
            if let Err(e) = lode_mod_checks::report_check_as(ctx, depot, &setup.reporter, req).await
            {
                tracing::warn!(error = ?e, "could not report a pending validation check");
            }
            todo.push((s, m));
        }
    }
    if !todo.is_empty() {
        let depot_name = ctx
            .meta()
            .read(move |tx| tx.depot(depot))
            .await?
            .map(|d| d.name)
            .unwrap_or_default();
        let dir = setup.work_dir.join(format!(
            "{}-{shelf_id}-{version}-{}",
            depot.0,
            uuid::Uuid::new_v4().simple()
        ));
        let prepared = prepare(ctx, setup, depot, &root, &config, &todo, &changes, &dir).await;
        for (s, m) in &todo {
            let r = match (&setup.python, &prepared) {
                (Err(e), _) => ScriptResult::error(&s.name, m.len(), e.clone()),
                (_, Err(e)) => ScriptResult::error(&s.name, m.len(), e.clone()),
                (Ok(py), Ok(())) if !py.at_least(config.validate.python) => ScriptResult::error(
                    &s.name,
                    m.len(),
                    format!(
                        "the server's Python is {}, older than the {} this depot needs",
                        py.version_text(),
                        config.validate.python_text().unwrap_or_default()
                    ),
                ),
                (Ok(py), Ok(())) => {
                    let input = Input {
                        version: lode_validate::INPUT_VERSION,
                        phase: "server".to_owned(),
                        depot: depot_name.clone(),
                        branch: shelf.branch.clone(),
                        files: m.clone(),
                    };
                    let script = dir.join(s.run.as_str());
                    lode_validate::run(
                        py,
                        &s.name,
                        &dir,
                        &script,
                        &input,
                        Duration::from_secs(u64::from(s.timeout_secs)),
                    )
                    .await
                }
            };
            results.push(r);
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    Ok(results)
}

/// Writes the shelf's matching changed files, then the head's scripts and support files on
/// top (so a shelf cannot replace them), into `dir`.
#[allow(clippy::too_many_arguments)]
async fn prepare(
    ctx: &ModuleCtx,
    setup: &Setup,
    depot: DepotId,
    root: &ObjectId,
    config: &LodeConfig,
    todo: &[(&ValidateScript, Vec<lode_validate::InputFile>)],
    changes: &[lode_core::Change],
    dir: &Path,
) -> Result<(), String> {
    let err = |e: BoxError| format!("could not prepare the files: {e}");
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| err(e.into()))?;
    let wanted: std::collections::HashSet<&str> = todo
        .iter()
        .flat_map(|(_, m)| m.iter().map(|f| f.path.as_str()))
        .collect();
    let mut total: u64 = 0;
    let too_big = || {
        format!(
            "the files to check are larger than the server's limit of {} MiB",
            setup.max_input / (1024 * 1024)
        )
    };
    for c in changes {
        if c.action == ChangeAction::Delete || !wanted.contains(c.path.as_str()) {
            continue;
        }
        if c.kind.is_some_and(|k| k != EntryKind::File) {
            continue;
        }
        let Some(id) = c.new_id else { continue };
        total = total.saturating_add(c.size.unwrap_or(0));
        if total > setup.max_input {
            return Err(too_big());
        }
        write_blob(ctx, depot, &id, &dir.join(c.path.as_str()))
            .await
            .map_err(err)?;
    }
    let mut head_files = Vec::new();
    for pattern in config.validate.support.patterns() {
        walk(
            ctx,
            depot,
            root,
            &literal_prefix(pattern),
            &config.validate.support,
            &mut head_files,
        )
        .await
        .map_err(err)?;
    }
    for (s, _) in todo {
        if let Some(e) = ctx
            .depots()
            .lookup(depot, root, &s.run)
            .await
            .map_err(|e| err(format!("{e:?}").into()))?
            .filter(|e| e.kind == EntryKind::File)
        {
            head_files.push((s.run.clone(), e.id, e.size));
        }
    }
    head_files.sort_by(|a, b| a.0.cmp(&b.0));
    head_files.dedup_by(|a, b| a.0 == b.0);
    for (path, id, size) in head_files {
        total = total.saturating_add(size);
        if total > setup.max_input {
            return Err(too_big());
        }
        write_blob(ctx, depot, &id, &dir.join(path.as_str()))
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// `GET /depots/{d}/validate/{shelf}/{version}`: the state and full results of one run.
async fn results(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, shelf, version)): ApiPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    caller.require_scope("read")?;
    let (d, access) = ctx.depots().resolve(&caller, &depot).await?;
    if !access.is_visible() {
        return Err(ApiError::not_found(format!("no depot `{depot}`")));
    }
    let num = |s: &str| {
        s.parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| ApiError::not_found("no such shelf version"))
    };
    let (shelf, version) = (num(&shelf)?, num(&version)?);
    let d = d.id.0;
    let row = ctx
        .meta()
        .read(move |tx| {
            tx.db().query_opt(
                "SELECT state, results FROM mod_validate_run
                 WHERE depot_id = ?1 AND shelf_id = ?2 AND shelf_version = ?3",
                vals![d, shelf, version],
                |r| Ok((r.get::<String>(0)?, r.get::<Option<String>>(1)?)),
            )
        })
        .await?
        .ok_or_else(|| ApiError::not_found("this shelf version has not been validated"))?;
    let results: Value = row
        .1
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or(Value::Array(Vec::new()));
    Ok(Json(json!({
        "shelf": shelf,
        "version": version,
        "state": row.0,
        "results": results,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(literal_prefix("tools/validate/**"), "tools/validate");
        assert_eq!(literal_prefix("tools/*.py"), "tools");
        assert_eq!(literal_prefix("**/*.json"), "");
        assert_eq!(literal_prefix("schema.json"), "");
    }
}
