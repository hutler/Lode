//! Lode module: signed webhooks (ARCHITECTURE §9.4, SPEC §6.5). Off by default.
//!
//! A hook names a URL, the events it wants and optionally a branch pattern. For every
//! matching outbox event, `on_event` records one `pending` delivery and returns; it never
//! touches the network, so a dead receiver cannot hold the outbox back. A background worker
//! sends deliveries, one at a time per hook and oldest first, signing each body with the
//! hook's secret, and retries failures on a fixed schedule for 24 hours.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use lode_core::{DepotId, ErrorCode, PathGlobSet};
use lode_meta::{MetaError, Migration, Millis, PrincipalId, PrincipalKind, Reads, WriteTx};
use lode_proto::consts::paths;
use lode_proto::webhooks::{
    CreateWebhookRequest, CreatedWebhook, DeliveryList, DeliveryState, DeliveryView, WebhookList,
    WebhookView, EVENTS, HEADER_DELIVERY, HEADER_EVENT, HEADER_SIGNATURE, PING_EVENT,
};
use lode_proto::PrincipalRef;
use lode_server::auth::{ms_to_time, SCOPE_ADMIN};
use lode_server::module::{BoxError, BoxFuture, Event, Module, ModuleCtx, Stop};
use lode_server::{ApiError, ApiJson, ApiPath, ApiResult, Caller};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use sha2::Sha256;

/// Module name, as in `[modules]`.
pub const NAME: &str = "webhooks";

/// Retry delays after the 1st, 2nd, 3rd and 4th failed attempt; every later one waits an
/// hour.
pub const RETRY_DELAYS_MS: [i64; 4] = [10_000, 60_000, 300_000, 1_800_000];
/// Delay between attempts once the schedule above is used up.
pub const HOURLY_MS: i64 = 3_600_000;
/// No attempt is made later than this after the first one; the delivery is then `failed`.
pub const GIVE_UP_AFTER_MS: i64 = 24 * HOURLY_MS;
/// Finished deliveries are deleted after this long.
pub const RETENTION_MS: i64 = 7 * 24 * HOURLY_MS;
/// A receiver must answer within this time.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of random secret per hook.
pub const SECRET_BYTES: usize = 32;
/// Longest URL accepted.
pub const MAX_URL_LEN: usize = 2048;
/// Longest branch pattern accepted.
pub const MAX_BRANCH_GLOB_LEN: usize = 200;
/// Most deliveries listed by `GET …/deliveries`.
pub const DELIVERIES_LISTED: u32 = 100;
/// The worker looks for due deliveries at least this often.
const IDLE_POLL_MS: i64 = 30_000;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_webhooks_hook (
              id          INTEGER PRIMARY KEY,
              depot_id    INTEGER NOT NULL,
              url         TEXT NOT NULL,
              secret      BLOB NOT NULL,
              events      TEXT NOT NULL,
              branch_glob TEXT,
              active      INTEGER NOT NULL DEFAULT 1,
              created_by  INTEGER NOT NULL,
              created_at  INTEGER NOT NULL
          );
          CREATE INDEX mod_webhooks_hook_depot ON mod_webhooks_hook (depot_id);
          CREATE TABLE mod_webhooks_delivery (
              id               INTEGER PRIMARY KEY,
              hook_id          INTEGER NOT NULL,
              outbox_id        INTEGER,
              event            TEXT NOT NULL,
              body             TEXT NOT NULL,
              state            TEXT NOT NULL,
              attempts         INTEGER NOT NULL DEFAULT 0,
              next_attempt_at  INTEGER,
              first_attempt_at INTEGER,
              last_status      INTEGER,
              last_error       TEXT,
              created_at       INTEGER NOT NULL,
              UNIQUE (hook_id, outbox_id)
          );
          CREATE INDEX mod_webhooks_delivery_pending
              ON mod_webhooks_delivery (state, hook_id, id);
          CREATE INDEX mod_webhooks_delivery_created ON mod_webhooks_delivery (created_at);",
}];

/// The webhooks module. Clones share one HTTP client and one wake-up signal.
#[derive(Clone)]
pub struct Webhooks {
    inner: Arc<Inner>,
}

struct Inner {
    client: Option<reqwest::Client>,
    wake: tokio::sync::Notify,
}

impl std::fmt::Debug for Webhooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhooks").finish_non_exhaustive()
    }
}

impl Default for Webhooks {
    fn default() -> Self {
        Self::new()
    }
}

impl Webhooks {
    /// A new module instance.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("lode-webhooks/", env!("CARGO_PKG_VERSION")))
            .build();
        let client = match client {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::error!(error = %e, "cannot build the webhook HTTP client; deliveries will fail");
                None
            }
        };
        Self {
            inner: Arc::new(Inner {
                client,
                wake: tokio::sync::Notify::new(),
            }),
        }
    }

    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self::new())
    }

    /// One round of the delivery worker: deletes old deliveries, then makes one attempt for
    /// the oldest pending delivery of every hook whose attempt is due, hooks in parallel.
    /// Returns what happened and when the next attempt is due. The worker calls this in a
    /// loop; tests call it directly with a manual clock.
    pub async fn run_once(&self, ctx: &ModuleCtx) -> Result<RunReport, MetaError> {
        let now = ctx.clock().now_ms();
        let pruned = ctx
            .meta()
            .store()
            .write(move |tx| {
                Ok(tx.conn().execute(
                    "DELETE FROM mod_webhooks_delivery
                     WHERE state != 'pending' AND created_at < ?1",
                    [now - RETENTION_MS],
                )?)
            })
            .await?;
        let heads = ctx
            .meta()
            .read(move |tx| {
                let mut st = tx.db().prepare_cached(
                    "SELECT d.id, d.event, d.body, d.attempts, d.next_attempt_at,
                            d.first_attempt_at, h.url, h.secret
                     FROM mod_webhooks_delivery d
                     JOIN mod_webhooks_hook h ON h.id = d.hook_id
                     WHERE h.active = 1 AND d.state = 'pending'
                       AND d.id = (SELECT min(id) FROM mod_webhooks_delivery
                                   WHERE hook_id = d.hook_id AND state = 'pending')",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(Due {
                        id: r.get(0)?,
                        event: r.get(1)?,
                        body: r.get(2)?,
                        attempts: r.get(3)?,
                        next_attempt_at: r.get(4)?,
                        first_attempt_at: r.get(5)?,
                        url: r.get(6)?,
                        secret: r.get(7)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let due: Vec<Due> = heads
            .into_iter()
            .filter(|d| d.next_attempt_at.is_none_or(|t| t <= now))
            .collect();
        let client = self.inner.client.as_ref();
        let outcomes = futures::future::join_all(due.iter().map(|d| attempt(client, d))).await;
        let mut report = RunReport {
            pruned,
            ..RunReport::default()
        };
        let mut updates = Vec::with_capacity(due.len());
        for (d, outcome) in due.iter().zip(outcomes) {
            let first = d.first_attempt_at.unwrap_or(now);
            let attempts = d.attempts + 1;
            let u = match outcome {
                Ok(status) => {
                    report.delivered += 1;
                    Update {
                        id: d.id,
                        state: DeliveryState::Ok,
                        attempts,
                        next: None,
                        first,
                        status: Some(status),
                        error: None,
                    }
                }
                Err((status, error)) => {
                    let next = now + retry_delay(attempts);
                    let (state, next) = if next > first + GIVE_UP_AFTER_MS {
                        report.given_up += 1;
                        (DeliveryState::Failed, None)
                    } else {
                        report.retrying += 1;
                        (DeliveryState::Pending, Some(next))
                    };
                    Update {
                        id: d.id,
                        state,
                        attempts,
                        next,
                        first,
                        status,
                        error: Some(error),
                    }
                }
            };
            updates.push(u);
        }
        let next_due = ctx
            .meta()
            .store()
            .write(move |tx| {
                let mut st = tx.conn().prepare_cached(
                    "UPDATE mod_webhooks_delivery
                     SET state = ?2, attempts = ?3, next_attempt_at = ?4, first_attempt_at = ?5,
                         last_status = ?6, last_error = ?7
                     WHERE id = ?1 AND state = 'pending'",
                )?;
                for u in &updates {
                    st.execute(params![
                        u.id,
                        u.state.as_str(),
                        u.attempts,
                        u.next,
                        u.first,
                        u.status,
                        u.error
                    ])?;
                }
                let next: Option<i64> = tx.conn().query_row(
                    "SELECT min(coalesce(d.next_attempt_at, 0)) FROM mod_webhooks_delivery d
                     JOIN mod_webhooks_hook h ON h.id = d.hook_id
                     WHERE h.active = 1 AND d.state = 'pending'",
                    [],
                    |r| r.get(0),
                )?;
                Ok(next)
            })
            .await?;
        report.next_due = next_due;
        Ok(report)
    }
}

/// What one [`Webhooks::run_once`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunReport {
    /// Deliveries the receiver accepted.
    pub delivered: usize,
    /// Failed attempts that will be retried.
    pub retrying: usize,
    /// Deliveries marked `failed` after 24 hours.
    pub given_up: usize,
    /// Old deliveries deleted.
    pub pruned: usize,
    /// When the next attempt is due (clock milliseconds), if any delivery is pending.
    pub next_due: Option<Millis>,
}

/// The delay before the attempt after `failed` failed attempts.
pub fn retry_delay(failed: u32) -> i64 {
    usize::try_from(failed)
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| RETRY_DELAYS_MS.get(i).copied())
        .unwrap_or(HOURLY_MS)
}

struct Due {
    id: i64,
    event: String,
    body: String,
    attempts: u32,
    next_attempt_at: Option<i64>,
    first_attempt_at: Option<i64>,
    url: String,
    secret: Vec<u8>,
}

struct Update {
    id: i64,
    state: DeliveryState,
    attempts: u32,
    next: Option<i64>,
    first: i64,
    status: Option<u16>,
    error: Option<String>,
}

/// `sha256=<hex>` over `body` with `secret`, the `X-Lode-Signature` value.
pub fn signature(secret: &[u8], body: &[u8]) -> String {
    // HMAC accepts keys of any length, so this cannot fail.
    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// One attempt: the status on 2xx, otherwise the status (if any) and a short reason.
async fn attempt(client: Option<&reqwest::Client>, d: &Due) -> Result<u16, (Option<u16>, String)> {
    let Some(client) = client else {
        return Err((None, "no HTTP client".into()));
    };
    let resp = client
        .post(&d.url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(HEADER_EVENT, &d.event)
        .header(HEADER_DELIVERY, d.id.to_string())
        .header(HEADER_SIGNATURE, signature(&d.secret, d.body.as_bytes()))
        .body(d.body.clone())
        .send()
        .await;
    match resp {
        Ok(r) => {
            let status = r.status();
            if status.is_success() {
                Ok(status.as_u16())
            } else if status.is_redirection() {
                Err((
                    Some(status.as_u16()),
                    format!("HTTP {status}: redirects are not followed"),
                ))
            } else {
                Err((Some(status.as_u16()), format!("HTTP {status}")))
            }
        }
        Err(e) => {
            let reason = if e.is_timeout() {
                "no answer within 10 s".to_owned()
            } else if e.is_connect() {
                "cannot connect".to_owned()
            } else {
                e.without_url().to_string()
            };
            Err((None, reason))
        }
    }
}

fn sql(e: rusqlite::Error) -> MetaError {
    MetaError::from(e)
}

fn principal_ref(tx: &dyn Reads, id: i64) -> Result<PrincipalRef, MetaError> {
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

struct HookRow {
    id: i64,
    url: String,
    events: String,
    branch_glob: Option<String>,
    active: bool,
    created_by: i64,
    created_at: Millis,
}

const HOOK_COLS: &str = "id, url, events, branch_glob, active, created_by, created_at";

fn hook_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HookRow> {
    Ok(HookRow {
        id: r.get(0)?,
        url: r.get(1)?,
        events: r.get(2)?,
        branch_glob: r.get(3)?,
        active: r.get::<_, i64>(4)? != 0,
        created_by: r.get(5)?,
        created_at: r.get(6)?,
    })
}

fn hook_view(tx: &dyn Reads, depot: &str, h: HookRow) -> Result<WebhookView, MetaError> {
    Ok(WebhookView {
        id: h.id,
        depot: depot.to_owned(),
        url: h.url,
        events: serde_json::from_str(&h.events).unwrap_or_default(),
        branch: h.branch_glob,
        active: h.active,
        created_by: principal_ref(tx, h.created_by)?,
        created_at: ms_to_time(h.created_at),
    })
}

const DELIVERY_COLS: &str =
    "id, hook_id, event, state, attempts, next_attempt_at, last_status, last_error, created_at";

fn delivery_view(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryView> {
    let state: String = r.get(3)?;
    Ok(DeliveryView {
        id: r.get(0)?,
        hook_id: r.get(1)?,
        event: r.get(2)?,
        state: DeliveryState::parse(&state).unwrap_or(DeliveryState::Failed),
        attempts: r.get(4)?,
        next_attempt_at: r.get::<_, Option<i64>>(5)?.map(ms_to_time),
        last_status: r.get(6)?,
        last_error: r.get(7)?,
        created_at: ms_to_time(r.get(8)?),
    })
}

/// A depot the caller administers. Webhooks reveal activity across the whole depot, so they
/// are for depot administrators only.
async fn admin_depot(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
) -> ApiResult<(DepotId, String)> {
    caller.require_scope(SCOPE_ADMIN)?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    if !access.is_depot_admin() {
        return Err(ApiError::new(
            ErrorCode::PermissionDenied,
            format!("managing webhooks needs admin on depot `{}`", d.name),
        ));
    }
    Ok((d.id, d.name))
}

fn parse_id(id: &str) -> ApiResult<i64> {
    id.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::not_found(format!("no webhook {id}")))
}

fn check_url(url: &str, allow_http: bool) -> ApiResult<()> {
    if url.len() > MAX_URL_LEN {
        return Err(ApiError::invalid_request(format!(
            "the URL is longer than {MAX_URL_LEN} bytes"
        )));
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| ApiError::invalid_request(format!("invalid URL: {e}")))?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => {
            return Err(ApiError::invalid_request(
                "webhook URLs must use https:// (the server does not set [webhooks] allow_http)",
            ))
        }
        other => {
            return Err(ApiError::invalid_request(format!(
                "webhook URLs must use https://, not {other}://"
            )))
        }
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(ApiError::invalid_request("the URL has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::invalid_request(
            "the URL must not carry a user name or password; the signature authenticates Lode",
        ));
    }
    Ok(())
}

fn check_events(events: &[String]) -> ApiResult<Vec<String>> {
    if events.is_empty() {
        return Err(ApiError::invalid_request("name at least one event"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in events {
        if !EVENTS.contains(&e.as_str()) {
            return Err(ApiError::invalid_request(format!(
                "unknown event `{e}`; known events: {}",
                EVENTS.join(", ")
            )));
        }
        if seen.insert(e.as_str()) {
            out.push(e.clone());
        }
    }
    Ok(out)
}

fn branch_matcher(glob: &str) -> Option<PathGlobSet> {
    PathGlobSet::new([glob]).ok()
}

async fn list(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
) -> ApiResult<Json<WebhookList>> {
    let (depot_id, name) = admin_depot(&ctx, &caller, &depot).await?;
    let webhooks = ctx
        .meta()
        .read(move |tx| {
            let mut st = tx.db().prepare_cached(&format!(
                "SELECT {HOOK_COLS} FROM mod_webhooks_hook WHERE depot_id = ?1 ORDER BY id"
            ))?;
            let rows = st
                .query_map([depot_id.0], hook_row)?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter().map(|h| hook_view(tx, &name, h)).collect()
        })
        .await?;
    Ok(Json(WebhookList { webhooks }))
}

async fn create(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    ApiJson(req): ApiJson<CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<CreatedWebhook>)> {
    let (depot_id, name) = admin_depot(&ctx, &caller, &depot).await?;
    check_url(&req.url, ctx.config().webhooks.allow_http)?;
    let events = check_events(&req.events)?;
    let branch = match req.branch.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(g) if g.len() > MAX_BRANCH_GLOB_LEN => {
            return Err(ApiError::invalid_request(format!(
                "the branch pattern is longer than {MAX_BRANCH_GLOB_LEN} bytes"
            )))
        }
        Some(g) => {
            if branch_matcher(g).is_none() {
                return Err(ApiError::invalid_request(format!(
                    "invalid branch pattern `{g}`"
                )));
            }
            Some(g.to_owned())
        }
    };
    let mut raw = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let secret = hex::encode(raw);
    let now = ctx.clock().now_ms();
    let me = caller.principal.id.0;
    let url = req.url.clone();
    let events_json = serde_json::to_string(&events).map_err(|_| ApiError::internal())?;
    let key = secret.clone().into_bytes();
    let hook = ctx
        .meta()
        .store()
        .write(move |tx| {
            tx.conn().execute(
                "INSERT INTO mod_webhooks_hook
                     (depot_id, url, secret, events, branch_glob, active, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)",
                params![depot_id.0, url, key, events_json, branch, me, now],
            )?;
            let id = tx.conn().last_insert_rowid();
            let row = tx.conn().query_row(
                &format!("SELECT {HOOK_COLS} FROM mod_webhooks_hook WHERE id = ?1"),
                [id],
                hook_row,
            )?;
            hook_view(tx, &name, row)
        })
        .await?;
    tracing::info!(hook = hook.id, depot = %hook.depot, "webhook created");
    Ok((StatusCode::CREATED, Json(CreatedWebhook { hook, secret })))
}

async fn remove(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
) -> ApiResult<StatusCode> {
    let (depot_id, _) = admin_depot(&ctx, &caller, &depot).await?;
    let id = parse_id(&id)?;
    let gone = ctx
        .meta()
        .store()
        .write(move |tx| {
            let n = tx.conn().execute(
                "DELETE FROM mod_webhooks_hook WHERE id = ?1 AND depot_id = ?2",
                params![id, depot_id.0],
            )?;
            if n > 0 {
                tx.conn()
                    .execute("DELETE FROM mod_webhooks_delivery WHERE hook_id = ?1", [id])?;
            }
            Ok(n > 0)
        })
        .await?;
    if gone {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no webhook {id}")))
    }
}

fn hook_exists(tx: &dyn Reads, depot: DepotId, id: i64) -> Result<bool, MetaError> {
    Ok(tx
        .db()
        .query_row(
            "SELECT 1 FROM mod_webhooks_hook WHERE id = ?1 AND depot_id = ?2",
            params![id, depot.0],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql)?
        .is_some())
}

async fn deliveries(
    State(ctx): State<ModuleCtx>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
) -> ApiResult<Json<DeliveryList>> {
    let (depot_id, _) = admin_depot(&ctx, &caller, &depot).await?;
    let id = parse_id(&id)?;
    let list = ctx
        .meta()
        .read(move |tx| {
            if !hook_exists(tx, depot_id, id)? {
                return Ok(None);
            }
            let mut st = tx.db().prepare_cached(&format!(
                "SELECT {DELIVERY_COLS} FROM mod_webhooks_delivery
                 WHERE hook_id = ?1 ORDER BY id DESC LIMIT ?2"
            ))?;
            let rows = st
                .query_map(params![id, DELIVERIES_LISTED], delivery_view)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(rows))
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no webhook {id}")))?;
    Ok(Json(DeliveryList { deliveries: list }))
}

async fn ping(
    State(ctx): State<ModuleCtx>,
    axum::Extension(module): axum::Extension<Webhooks>,
    caller: Caller,
    ApiPath((depot, id)): ApiPath<(String, String)>,
) -> ApiResult<(StatusCode, Json<DeliveryView>)> {
    let (depot_id, name) = admin_depot(&ctx, &caller, &depot).await?;
    let id = parse_id(&id)?;
    let now = ctx.clock().now_ms();
    let body = json!({
        "id": 0,
        "event": PING_EVENT,
        "depot": name,
        "created_at": ms_to_time(now)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        "data": { "hook_id": id },
    })
    .to_string();
    let view = ctx
        .meta()
        .store()
        .write(move |tx| {
            if !hook_exists(tx, depot_id, id)? {
                return Ok(None);
            }
            tx.conn().execute(
                "INSERT INTO mod_webhooks_delivery
                     (hook_id, outbox_id, event, body, state, attempts, next_attempt_at, created_at)
                 VALUES (?1, NULL, ?2, ?3, 'pending', 0, ?4, ?4)",
                params![id, PING_EVENT, body, now],
            )?;
            let did = tx.conn().last_insert_rowid();
            Ok(Some(tx.conn().query_row(
                &format!("SELECT {DELIVERY_COLS} FROM mod_webhooks_delivery WHERE id = ?1"),
                [did],
                delivery_view,
            )?))
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no webhook {id}")))?;
    module.inner.wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(view)))
}

/// Records one pending delivery per matching hook. Returns how many were new.
fn enqueue(tx: &WriteTx<'_>, event: &Event, now: Millis) -> Result<usize, MetaError> {
    let Some(depot) = event.depot.as_deref() else {
        return Ok(0);
    };
    let Some(d) = tx.depot_by_name(depot)? else {
        return Ok(0);
    };
    let branch = event.data.get("branch").and_then(|b| b.as_str());
    let hooks = {
        let mut st = tx.conn().prepare_cached(&format!(
            "SELECT {HOOK_COLS} FROM mod_webhooks_hook WHERE depot_id = ?1 AND active = 1"
        ))?;
        let rows = st.query_map([d.id.0], hook_row)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut body = None;
    let mut added = 0;
    for h in hooks {
        let events: Vec<String> = serde_json::from_str(&h.events).unwrap_or_default();
        if !events.contains(&event.event) {
            continue;
        }
        if let (Some(glob), Some(b)) = (&h.branch_glob, branch) {
            if !branch_matcher(glob).is_some_and(|m| m.matches_str(b)) {
                continue;
            }
        }
        let body = match &body {
            Some(b) => b,
            None => body.insert(serde_json::to_string(event).map_err(|e| {
                MetaError::BadValue(format!("cannot encode event {}: {e}", event.id))
            })?),
        };
        added += tx.conn().execute(
            "INSERT OR IGNORE INTO mod_webhooks_delivery
                 (hook_id, outbox_id, event, body, state, attempts, next_attempt_at, created_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?5)",
            params![h.id, event.id, event.event, body, now],
        )?;
    }
    Ok(added)
}

impl Module for Webhooks {
    fn name(&self) -> &'static str {
        NAME
    }

    fn purge_depot(&self, tx: &WriteTx<'_>, depot: DepotId) -> Result<(), MetaError> {
        tx.conn().execute(
            "DELETE FROM mod_webhooks_delivery WHERE hook_id IN \
             (SELECT id FROM mod_webhooks_hook WHERE depot_id = ?1)",
            [depot.0],
        )?;
        tx.conn().execute(
            "DELETE FROM mod_webhooks_hook WHERE depot_id = ?1",
            [depot.0],
        )?;
        Ok(())
    }

    fn migrations(&self) -> &'static [Migration] {
        MIGRATIONS
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        Some(
            Router::new()
                .route(paths::WEBHOOKS, get(list).post(create))
                .route(paths::WEBHOOK, delete(remove))
                .route(paths::WEBHOOK_DELIVERIES, get(deliveries))
                .route(paths::WEBHOOK_PING, post(ping))
                .layer(axum::Extension(self.clone()))
                .with_state(ctx),
        )
    }

    fn on_event<'a>(
        &'a self,
        ctx: &'a ModuleCtx,
        event: &'a Event,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            if !EVENTS.contains(&event.event.as_str()) {
                return Ok(());
            }
            let now = ctx.clock().now_ms();
            let ev = event.clone();
            let added = ctx
                .meta()
                .store()
                .write(move |tx| enqueue(tx, &ev, now))
                .await?;
            if added > 0 {
                self.inner.wake.notify_one();
            }
            Ok(())
        })
    }

    fn worker(&self, ctx: ModuleCtx, mut stop: Stop) -> Option<BoxFuture<'static, ()>> {
        let this = self.clone();
        Some(Box::pin(async move {
            loop {
                if stop.is_stopped() {
                    return;
                }
                let round = tokio::select! {
                    () = stop.wait() => return,
                    r = this.run_once(&ctx) => r,
                };
                let wait_ms = match round {
                    Ok(r) => r
                        .next_due
                        .map_or(IDLE_POLL_MS, |t| t - ctx.clock().now_ms())
                        .clamp(0, IDLE_POLL_MS),
                    Err(e) => {
                        tracing::error!(error = %e, "webhook delivery round failed");
                        IDLE_POLL_MS
                    }
                };
                if wait_ms == 0 {
                    continue;
                }
                let wait = Duration::from_millis(wait_ms.unsigned_abs());
                tokio::select! {
                    () = stop.wait() => return,
                    () = this.inner.wake.notified() => {}
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }))
    }
}
