//! Lode module: checks (ARCHITECTURE §9.5, SPEC §6.6). Off by default.
//!
//! A check is a named result that an outside system, such as a build server, attaches to a
//! revision or to one version of a shelf. Reporting needs the `checks` scope, which grants
//! nothing else. Lode stores and lists checks; it does not run anything.

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::{DepotId, ErrorCode};
use lode_meta::{MetaError, Migration, Millis, PrincipalId, PrincipalKind, Reads};
use lode_proto::checks::{
    CheckList, CheckState, CheckTarget, CheckTargetKind, CheckView, ChecksQuery,
    ReportCheckRequest, MAX_NAME_LEN, MAX_SUMMARY_LEN, MAX_URL_LEN,
};
use lode_proto::consts::paths;
use lode_proto::PrincipalRef;
use lode_server::auth::{ms_to_time, SCOPE_READ};
use lode_server::commit::{CommitCtx, Origin};
use lode_server::module::{Module, ModuleCtx, ScopeDef};
use lode_server::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult, Caller};
use rusqlite::params;
use serde_json::json;

/// Module name, as in `[modules]`.
pub const NAME: &str = "checks";
/// Scope needed to report results. It allows nothing else and is not an agent default.
pub const SCOPE_CHECKS: &str = "checks";

const SCOPES: &[ScopeDef] = &[ScopeDef {
    name: SCOPE_CHECKS,
    agent_default: false,
}];

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_checks_check (
              depot_id       INTEGER NOT NULL,
              target_kind    TEXT NOT NULL,
              target_id      INTEGER NOT NULL,
              target_version INTEGER NOT NULL,
              name           TEXT NOT NULL,
              state          TEXT NOT NULL,
              summary        TEXT,
              url            TEXT,
              reporter_id    INTEGER NOT NULL,
              updated_at     INTEGER NOT NULL,
              PRIMARY KEY (depot_id, target_kind, target_id, target_version, name)
          ) WITHOUT ROWID;",
}];

/// The checks module.
#[derive(Debug, Default, Clone, Copy)]
pub struct Checks;

impl Checks {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self)
    }
}

/// One stored check, as read from the table.
#[derive(Debug, Clone)]
pub(crate) struct CheckRow {
    pub(crate) kind: String,
    pub(crate) target_id: i64,
    pub(crate) version: i64,
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) summary: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) reporter_id: i64,
    pub(crate) updated_at: Millis,
}

const COLUMNS: &str =
    "target_kind, target_id, target_version, name, state, summary, url, reporter_id, updated_at";

fn check_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CheckRow> {
    Ok(CheckRow {
        kind: r.get(0)?,
        target_id: r.get(1)?,
        version: r.get(2)?,
        name: r.get(3)?,
        state: r.get(4)?,
        summary: r.get(5)?,
        url: r.get(6)?,
        reporter_id: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// The checks of one target, most recently reported first.
pub(crate) fn target_checks(
    db: &rusqlite::Connection,
    depot: DepotId,
    kind: CheckTargetKind,
    id: u64,
    version: u64,
) -> Result<Vec<CheckRow>, MetaError> {
    let mut st = db.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM mod_checks_check
         WHERE depot_id = ?1 AND target_kind = ?2 AND target_id = ?3 AND target_version = ?4
         ORDER BY updated_at DESC, name"
    ))?;
    let rows = st.query_map(
        params![depot.0, kind.as_str(), to_i64(id), to_i64(version)],
        check_row,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn principal_ref(tx: &dyn Reads, id: i64) -> Result<PrincipalRef, MetaError> {
    let p = tx.principal(PrincipalId(id))?;
    Ok(match p {
        Some(p) => {
            let owner = match (p.kind, p.owner_id) {
                (PrincipalKind::Agent, Some(o)) => tx.principal(o)?.map(|o| o.name),
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
    })
}

fn view(tx: &dyn Reads, r: CheckRow) -> Result<CheckView, MetaError> {
    let kind = CheckTargetKind::parse(&r.kind).unwrap_or(CheckTargetKind::Revision);
    Ok(CheckView {
        target: CheckTarget {
            kind,
            id: u64::try_from(r.target_id).unwrap_or_default(),
            version: match kind {
                CheckTargetKind::Revision => None,
                CheckTargetKind::Shelf => Some(u64::try_from(r.version).unwrap_or_default()),
            },
        },
        name: r.name,
        state: CheckState::parse(&r.state).unwrap_or(CheckState::Error),
        summary: r.summary,
        url: r.url,
        reporter: principal_ref(tx, r.reporter_id)?,
        updated_at: ms_to_time(r.updated_at),
    })
}

/// A valid check name: `[a-z0-9._/-]{1,100}`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'/' | b'-')
        })
}

fn check_url(url: &str) -> ApiResult<()> {
    if url.len() > MAX_URL_LEN {
        return Err(ApiError::invalid_request(format!(
            "url is longer than {MAX_URL_LEN} bytes"
        )));
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    match rest {
        Some(r)
            if !r.is_empty() && !r.starts_with('/') && !url.chars().any(char::is_whitespace) =>
        {
            Ok(())
        }
        _ => Err(ApiError::invalid_request(
            "url must be an http:// or https:// link",
        )),
    }
}

/// The target as stored: kind, id and version (0 for a revision).
fn stored_target(t: &CheckTarget) -> ApiResult<(CheckTargetKind, u64, u64)> {
    if t.id == 0 {
        return Err(ApiError::invalid_request("target id must be 1 or more"));
    }
    match t.kind {
        CheckTargetKind::Revision => match t.version {
            None | Some(0) => Ok((t.kind, t.id, 0)),
            Some(_) => Err(ApiError::invalid_request(
                "a revision target has no version",
            )),
        },
        CheckTargetKind::Shelf => match t.version {
            Some(v) if v >= 1 => Ok((t.kind, t.id, v)),
            _ => Err(ApiError::invalid_request(
                "a shelf target needs a version of 1 or more",
            )),
        },
    }
}

/// Fails with `not_found` unless `caller` can read revision `rev` of `depot`.
async fn readable(ctx: &ModuleCtx, caller: &Caller, depot: DepotId, rev: u64) -> ApiResult<()> {
    ctx.depots()
        .readable_rev(caller, depot, rev)
        .await
        .map(|_| ())
}

async fn report(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<ReportCheckRequest>,
) -> ApiResult<Json<CheckView>> {
    Ok(Json(report_check(&ctx, &caller, &depot, req).await?))
}

/// Creates or replaces a check (see `POST /depots/{d}/checks`).
pub async fn report_check(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    req: ReportCheckRequest,
) -> ApiResult<CheckView> {
    caller.require_scope(SCOPE_CHECKS)?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    if !access.is_visible() {
        return Err(ApiError::not_found(format!("no depot `{depot}`")));
    }
    if !valid_name(&req.name) {
        return Err(ApiError::invalid_request(format!(
            "check names are 1 to {MAX_NAME_LEN} of a-z, 0-9, `.`, `_`, `/` and `-`"
        )));
    }
    if req
        .summary
        .as_ref()
        .is_some_and(|s| s.len() > MAX_SUMMARY_LEN)
    {
        return Err(ApiError::invalid_request(format!(
            "summary is longer than {MAX_SUMMARY_LEN} bytes"
        )));
    }
    if let Some(u) = &req.url {
        check_url(u)?;
    }
    let (kind, id, version) = stored_target(&req.target)?;
    if kind == CheckTargetKind::Revision {
        readable(ctx, caller, d.id, id).await?;
    }
    let now = ctx.clock().now_ms();
    let me = caller.principal.id.0;
    let depot_id = d.id;
    let reporter = caller.principal.name.clone();
    let view = ctx
        .meta()
        .store()
        .write(move |tx| {
            tx.conn().execute(
                &format!(
                    "INSERT INTO mod_checks_check (depot_id, {COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT (depot_id, target_kind, target_id, target_version, name)
                     DO UPDATE SET state = excluded.state, summary = excluded.summary,
                                   url = excluded.url, reporter_id = excluded.reporter_id,
                                   updated_at = excluded.updated_at"
                ),
                params![
                    depot_id.0,
                    kind.as_str(),
                    to_i64(id),
                    to_i64(version),
                    req.name,
                    req.state.as_str(),
                    req.summary,
                    req.url,
                    me,
                    now
                ],
            )?;
            let data = json!({
                "target_kind": kind.as_str(),
                "target_id": id,
                "target_version": version,
                "name": req.name,
                "state": req.state.as_str(),
                "reporter": reporter,
            });
            tx.append_outbox(Some(depot_id), "check.updated", &data.to_string(), now)?;
            view(
                tx,
                CheckRow {
                    kind: kind.as_str().to_owned(),
                    target_id: to_i64(id),
                    version: to_i64(version),
                    name: req.name,
                    state: req.state.as_str().to_owned(),
                    summary: req.summary,
                    url: req.url,
                    reporter_id: me,
                    updated_at: now,
                },
            )
        })
        .await?;
    ctx.notify_events();
    Ok(view)
}

async fn list(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<CheckList>> {
    let kind = q
        .get("target_kind")
        .and_then(CheckTargetKind::parse)
        .ok_or_else(|| ApiError::invalid_request("target_kind must be revision or shelf"))?;
    let id = q
        .u64("target_id")?
        .ok_or_else(|| ApiError::invalid_request("target_id is required"))?;
    let query = ChecksQuery {
        target_kind: kind,
        target_id: id,
        target_version: q.u64("target_version")?,
    };
    Ok(Json(list_checks(&ctx, &caller, &depot, query).await?))
}

/// The checks of one target that `caller` may see (see `GET /depots/{d}/checks`).
pub async fn list_checks(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    q: ChecksQuery,
) -> ApiResult<CheckList> {
    caller.require_scope(SCOPE_READ)?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    if !access.is_visible() {
        return Err(ApiError::not_found(format!("no depot `{depot}`")));
    }
    let (kind, id, version) = stored_target(&CheckTarget {
        kind: q.target_kind,
        id: q.target_id,
        version: q.target_version,
    })?;
    if kind == CheckTargetKind::Revision {
        readable(ctx, caller, d.id, id).await?;
    }
    let depot_id = d.id;
    let checks = ctx
        .meta()
        .read(move |tx| {
            let rows = target_checks(tx.db(), depot_id, kind, id, version)?;
            rows.into_iter().map(|r| view(tx, r)).collect()
        })
        .await?;
    Ok(CheckList { checks })
}

/// The origin name shelf submits carry (ARCHITECTURE §8). The gate's only coupling to the
/// shelves module, together with the two attribute names below.
pub const SHELVES_ORIGIN: &str = "shelves";
/// Origin attribute holding the shelf id.
pub const ATTR_SHELF_ID: &str = "shelf_id";
/// Origin attribute holding the shelf version being submitted.
pub const ATTR_SHELF_VERSION: &str = "shelf_version";

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

/// The submit gate: every check the branch head's `[checks] required` names must be
/// `success` on this shelf version, reported by the named principal.
fn gate(tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
    let config = ctx.branch_config;
    if config.checks_required.is_empty() || !config.checks_branches.matches_str(&ctx.branch.name) {
        return Ok(());
    }
    let Some((shelf, version)) = submitted_shelf(ctx.origin) else {
        if matches!(ctx.origin, Origin::Module { name, .. } if name == SHELVES_ORIGIN) {
            // A shelf submit without its attributes cannot be checked; refuse rather than
            // let it through.
            return Err(ApiError::new(
                ErrorCode::ChecksRequired,
                "this shelf submit does not name its shelf version, so its checks cannot be verified",
            ));
        }
        return Ok(());
    };
    let rows = target_checks(tx.db(), ctx.depot, CheckTargetKind::Shelf, shelf, version)?;
    let mut missing = Vec::new();
    for req in &config.checks_required {
        let found = rows.iter().find(|r| r.name == req.name);
        let entry = match found {
            None => Some(json!({ "name": req.name, "reason": "absent", "reporter": req.reporter })),
            Some(r) => {
                let reporter = principal_ref(tx, r.reporter_id)?.name;
                if reporter != req.reporter {
                    Some(json!({ "name": req.name, "reason": "wrong_reporter",
                                 "state": r.state, "reported_by": reporter,
                                 "reporter": req.reporter }))
                } else if r.state != CheckState::Success.as_str() {
                    Some(json!({ "name": req.name, "reason": "not_success",
                                 "state": r.state, "reporter": req.reporter }))
                } else {
                    None
                }
            }
        };
        missing.extend(entry);
    }
    if missing.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = missing.iter().filter_map(|m| m["name"].as_str()).collect();
    Err(ApiError::new(
        ErrorCode::ChecksRequired,
        format!(
            "shelf {shelf} version {version} has not passed the checks `{}` requires: {}",
            ctx.branch.name,
            names.join(", ")
        ),
    )
    .with_details(json!({ "shelf_id": shelf, "shelf_version": version, "missing": missing })))
}

impl Module for Checks {
    fn name(&self) -> &'static str {
        NAME
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
                .route(paths::CHECKS, get(list).post(report))
                .with_state(ctx),
        )
    }

    fn pre_commit(&self, tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
        gate(tx, ctx)
    }
}
