//! Lode module: the event feed (SPEC §6.7). On by default.
//!
//! The feed keeps a copy of every outbox event (ARCHITECTURE §8.1) for a week, so a client
//! that only reads can see what happened in a depot since it last looked, and can hold one
//! request open until something does. Events carry identifiers only, as for webhooks; a
//! reader fetches details with its own token.
//!
//! The outbox itself is pruned once every module has seen an event, so the feed stores what
//! it needs in its own table, keyed by the outbox id. Waiting requests are woken after the
//! row is written, never before, so a woken request always finds what woke it.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use lode_core::DepotId;
use lode_meta::{vals, MetaError, Migration, Reads as _, WriteTx};
use lode_proto::consts::paths;
use lode_proto::feed::{EventList, EventsQuery, EVENTS, MAX_EVENTS, MAX_WAIT_SECONDS};
use lode_server::auth::SCOPE_READ;
use lode_server::module::{BoxError, BoxFuture, Event, Module, ModuleCtx, Stop, ToolDef};
use lode_server::{ApiError, ApiPath, ApiQuery, ApiResult, Caller, DepotAccess};
use serde_json::{json, Value};

/// Module name, as in `[modules]`.
pub const NAME: &str = "feed";

/// Events are kept this long.
pub const KEEP_MS: i64 = 7 * 24 * 3_600_000;
/// At most this many events are kept per depot; older ones are dropped first.
pub const KEEP_EVENTS: i64 = 100_000;
/// Requests held waiting at once, per server; beyond this a request answers at once.
pub const MAX_WAITING: usize = 256;
/// Longest wait of the MCP tool, below the timeout of common MCP clients.
pub const MAX_TOOL_WAIT_SECONDS: u64 = 50;
/// Default wait of the MCP tool.
pub const DEFAULT_TOOL_WAIT_SECONDS: u64 = 25;
/// How often old events are pruned.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_feed_event (
              id          INTEGER PRIMARY KEY,
              depot_id    INTEGER NOT NULL,
              created_at  INTEGER NOT NULL,
              body        TEXT NOT NULL
          );
          CREATE INDEX mod_feed_event_depot ON mod_feed_event (depot_id, id);
          CREATE INDEX mod_feed_event_created ON mod_feed_event (created_at);
          CREATE TABLE mod_feed_depot (
              depot_id        INTEGER PRIMARY KEY,
              pruned_through  INTEGER NOT NULL
          );",
    postgres: "CREATE TABLE mod_feed_event (
              id          BIGINT PRIMARY KEY,
              depot_id    BIGINT NOT NULL,
              created_at  BIGINT NOT NULL,
              body        TEXT COLLATE \"C\" NOT NULL
          );
          CREATE INDEX mod_feed_event_depot ON mod_feed_event (depot_id, id);
          CREATE INDEX mod_feed_event_created ON mod_feed_event (created_at);
          CREATE TABLE mod_feed_depot (
              depot_id        BIGINT PRIMARY KEY,
              pruned_through  BIGINT NOT NULL
          );",
}];

/// The feed module. Clones share the wake-up signal and the count of waiting requests.
#[derive(Clone, Default)]
pub struct Feed {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Woken after each stored event.
    stored: tokio::sync::Notify,
    /// Requests waiting now.
    waiting: AtomicUsize,
}

impl std::fmt::Debug for Feed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feed").finish_non_exhaustive()
    }
}

impl Feed {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self::default())
    }

    /// Drops events older than [`KEEP_MS`] and beyond [`KEEP_EVENTS`] per depot, recording
    /// per depot the newest id dropped. Returns how many were dropped. The worker calls this
    /// hourly; tests call it directly with a manual clock.
    pub async fn prune(&self, ctx: &ModuleCtx) -> Result<usize, MetaError> {
        let cutoff = ctx.clock().now_ms() - KEEP_MS;
        ctx.meta()
            .store()
            .write(move |tx| {
                let depots: Vec<i64> = tx.conn().query(
                    "SELECT DISTINCT depot_id FROM mod_feed_event",
                    vals![],
                    |r| r.get(0),
                )?;
                let mut dropped = 0;
                for d in depots {
                    // The newest id to drop: the older of the age limit and the count limit.
                    let by_age: Option<i64> = tx.conn().value(
                        "SELECT max(id) FROM mod_feed_event WHERE depot_id = ?1 AND created_at < ?2",
                        vals![d, cutoff],
                    )?;
                    let by_count: Option<i64> = tx.conn().query_opt(
                        "SELECT id FROM mod_feed_event WHERE depot_id = ?1
                         ORDER BY id DESC LIMIT 1 OFFSET ?2",
                        vals![d, KEEP_EVENTS],
                        |r| r.get(0),
                    )?;
                    let Some(through) = by_age.max(by_count) else {
                        continue;
                    };
                    dropped += tx.conn().execute(
                        "DELETE FROM mod_feed_event WHERE depot_id = ?1 AND id <= ?2",
                        vals![d, through],
                    )?;
                    tx.conn().execute(
                        "INSERT INTO mod_feed_depot (depot_id, pruned_through) VALUES (?1, ?2)
                         ON CONFLICT (depot_id) DO UPDATE SET pruned_through = excluded.pruned_through",
                        vals![d, through],
                    )?;
                }
                Ok(dropped)
            })
            .await
    }
}

/// Where a read starts and what it keeps.
struct Filter {
    depot: DepotId,
    access: DepotAccess,
    kinds: Option<HashSet<String>>,
}

impl Filter {
    /// Whether the caller may see `event`: every event of a depot they can see, except those
    /// naming a branch they can read nothing on.
    fn keeps(&self, event: &Value) -> bool {
        if let Some(k) = &self.kinds {
            if !event["event"].as_str().is_some_and(|e| k.contains(e)) {
                return false;
            }
        }
        match event["data"]["branch"].as_str() {
            Some(b) => self.access.branch(b).reads_anything(),
            None => true,
        }
    }
}

fn parse_kinds(kinds: Option<&str>) -> ApiResult<Option<HashSet<String>>> {
    let Some(kinds) = kinds.filter(|k| !k.is_empty()) else {
        return Ok(None);
    };
    let mut out = HashSet::new();
    for k in kinds.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        if !EVENTS.contains(&k) {
            return Err(ApiError::invalid_request(format!(
                "unknown event `{k}`; known events: {}",
                EVENTS.join(", ")
            )));
        }
        out.insert(k.to_owned());
    }
    Ok(Some(out))
}

/// One look at the table: the matching events after `after`, and the cursor past them.
async fn look(ctx: &ModuleCtx, f: &Filter, after: i64) -> ApiResult<EventList> {
    let depot = f.depot.0;
    let (rows, pruned, newest) = ctx
        .meta()
        .read(move |tx| {
            let rows: Vec<(i64, String)> = tx.db().query(
                "SELECT id, body FROM mod_feed_event WHERE depot_id = ?1 AND id > ?2
                 ORDER BY id LIMIT ?3",
                vals![depot, after, (MAX_EVENTS * 4) as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let pruned: Option<i64> = tx.db().query_opt(
                "SELECT pruned_through FROM mod_feed_depot WHERE depot_id = ?1",
                vals![depot],
                |r| r.get(0),
            )?;
            let newest: Option<i64> = tx
                .db()
                .value("SELECT max(id) FROM mod_feed_event", vals![])?;
            Ok((rows, pruned, newest))
        })
        .await?;
    let scanned_all = rows.len() < MAX_EVENTS * 4;
    let mut next = after;
    let mut events = Vec::new();
    for (id, body) in rows {
        if events.len() == MAX_EVENTS {
            break;
        }
        next = id;
        let Ok(v) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        if f.keeps(&v) {
            events.push(v);
        }
    }
    // Nothing left to read for this depot: jump the cursor to the newest event anywhere, so
    // the next look starts after it.
    if scanned_all && events.len() < MAX_EVENTS {
        next = next.max(newest.unwrap_or(0)).max(after);
    }
    Ok(EventList {
        events,
        next,
        truncated: pruned.is_some_and(|p| after < p),
    })
}

/// Reads events for `caller` in `depot`, holding the request up to `wait` seconds until one
/// arrives.
pub async fn events(
    module: &Feed,
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    q: EventsQuery,
) -> ApiResult<EventList> {
    caller.require_scope(SCOPE_READ)?;
    let (d, access) = ctx.depots().resolve(caller, depot).await?;
    let filter = Filter {
        depot: d.id,
        access,
        kinds: parse_kinds(q.kinds.as_deref())?,
    };
    let Some(after) = q.after else {
        let newest: Option<i64> = ctx
            .meta()
            .read(|tx| tx.db().value("SELECT max(id) FROM mod_feed_event", vals![]))
            .await?;
        return Ok(EventList {
            events: Vec::new(),
            next: newest.unwrap_or(0),
            truncated: false,
        });
    };
    if after < 0 {
        return Err(ApiError::invalid_request(
            "`after` is an event id, 0 or more",
        ));
    }
    let wait = q.wait.unwrap_or(0).min(MAX_WAIT_SECONDS);
    let first = look(ctx, &filter, after).await?;
    if wait == 0 || !first.events.is_empty() || first.truncated {
        return Ok(first);
    }
    let inner = &module.inner;
    if inner.waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
        inner.waiting.fetch_sub(1, Ordering::SeqCst);
        return Ok(first);
    }
    let _waiting = Waiting(&inner.waiting);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    let mut cursor = first.next;
    loop {
        // Registered before looking, so an event stored between the look and the wait wakes
        // this request.
        let stored = inner.stored.notified();
        tokio::pin!(stored);
        stored.as_mut().enable();
        let now = look(ctx, &filter, cursor).await?;
        if !now.events.is_empty() || now.truncated {
            return Ok(now);
        }
        cursor = now.next;
        tokio::select! {
            () = stored => {}
            () = tokio::time::sleep_until(deadline) => {
                return Ok(EventList { events: Vec::new(), next: cursor, truncated: false });
            }
        }
    }
}

/// Counts a waiting request until dropped.
struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn query_from(q: &ApiQuery) -> ApiResult<EventsQuery> {
    let num = |key: &str| -> ApiResult<Option<i64>> {
        match q.get(key) {
            None | Some("") => Ok(None),
            Some(v) => v
                .parse::<i64>()
                .map(Some)
                .map_err(|_| ApiError::invalid_request(format!("`{key}` must be a number"))),
        }
    };
    let wait = match num("wait")? {
        None => None,
        Some(w) if w < 0 => return Err(ApiError::invalid_request("`wait` must be 0 or more")),
        Some(w) => Some(w.unsigned_abs()),
    };
    Ok(EventsQuery {
        after: num("after")?,
        kinds: q.get("kinds").map(str::to_owned),
        wait,
    })
}

async fn list(
    State(ctx): State<ModuleCtx>,
    axum::Extension(module): axum::Extension<Feed>,
    caller: Caller,
    ApiPath(depot): ApiPath<String>,
    q: ApiQuery,
) -> ApiResult<Json<EventList>> {
    let q = query_from(&q)?;
    Ok(Json(events(&module, &ctx, &caller, &depot, q).await?))
}

/// Stores one event, unless its depot is gone.
fn store(tx: &WriteTx<'_>, event: &Event, now: i64) -> Result<bool, MetaError> {
    let Some(depot) = event.depot.as_deref() else {
        return Ok(false);
    };
    let Some(d) = tx.depot_by_name(depot)? else {
        return Ok(false);
    };
    let body = serde_json::to_string(event)
        .map_err(|e| MetaError::BadValue(format!("cannot encode event {}: {e}", event.id)))?;
    let added = tx.conn().execute(
        "INSERT INTO mod_feed_event (id, depot_id, created_at, body) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO NOTHING",
        vals![event.id, d.id.0, now, &body],
    )?;
    Ok(added > 0)
}

fn tool(module: &Feed, ctx: &ModuleCtx) -> ToolDef {
    let module = module.clone();
    let ctx = ctx.clone();
    ToolDef {
        name: "wait_for_events".into(),
        description: "Wait for something to happen in a depot: a new revision, a branch \
                      created or archived, a lock taken or released, a shelf created, \
                      updated, submitted or deleted, or a check result. Answers at once when \
                      events after `after` exist, otherwise waits up to `timeout_s` seconds \
                      (default 25, at most 50). Without `after` it answers no events and the \
                      current cursor. Keep `next` and pass it as `after` on the next call. \
                      Events carry ids only: read details with `log`, `get_shelf`, \
                      `list_locks` or `list_checks`. If `truncated` is true, events were \
                      dropped (they are kept for 7 days): read the history again."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "depot": { "type": "string", "description": "Depot name" },
                "after": { "type": "integer", "minimum": 0, "description": "The `next` of the previous call" },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string", "enum": EVENTS },
                    "description": "Only these events (default: all)"
                },
                "timeout_s": { "type": "integer", "minimum": 0, "maximum": MAX_TOOL_WAIT_SECONDS, "description": "Seconds to wait (default 25)" }
            },
            "required": ["depot"],
            "additionalProperties": false
        }),
        scope: SCOPE_READ.into(),
        handler: Arc::new(move |caller, input| {
            let module = module.clone();
            let ctx = ctx.clone();
            Box::pin(async move {
                let depot = input["depot"]
                    .as_str()
                    .ok_or_else(|| ApiError::invalid_request("`depot` is required"))?
                    .to_owned();
                let after =
                    match &input["after"] {
                        Value::Null => None,
                        v => Some(v.as_i64().ok_or_else(|| {
                            ApiError::invalid_request("`after` must be an integer")
                        })?),
                    };
                let kinds = match &input["kinds"] {
                    Value::Null => None,
                    Value::Array(a) => Some(
                        a.iter()
                            .enumerate()
                            .map(|(index, value)| {
                                value
                                    .as_str()
                                    .filter(|kind| EVENTS.contains(kind))
                                    .ok_or_else(|| {
                                        ApiError::invalid_request(format!(
                                            "kinds[{index}] must be one of: {}",
                                            EVENTS.join(", ")
                                        ))
                                    })
                            })
                            .collect::<ApiResult<Vec<_>>>()?
                            .join(","),
                    ),
                    _ => return Err(ApiError::invalid_request("`kinds` must be a list")),
                };
                let wait = match &input["timeout_s"] {
                    Value::Null => DEFAULT_TOOL_WAIT_SECONDS,
                    v => v.as_u64().ok_or_else(|| {
                        ApiError::invalid_request("`timeout_s` must be 0 or more")
                    })?,
                }
                .min(MAX_TOOL_WAIT_SECONDS);
                let q = EventsQuery {
                    after,
                    kinds,
                    wait: Some(wait),
                };
                let list = events(&module, &ctx, &caller, &depot, q).await?;
                serde_json::to_value(list).map_err(|_| ApiError::internal())
            })
        }),
    }
}

impl Module for Feed {
    fn name(&self) -> &'static str {
        NAME
    }

    fn migrations(&self) -> &'static [Migration] {
        MIGRATIONS
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        Some(
            Router::new()
                .route(paths::EVENTS, get(list))
                .layer(axum::Extension(self.clone()))
                .with_state(ctx),
        )
    }

    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> {
        vec![tool(self, &ctx)]
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
                .write(move |tx| store(tx, &ev, now))
                .await?;
            if added {
                self.inner.stored.notify_waiters();
            }
            Ok(())
        })
    }

    fn purge_depot(&self, tx: &WriteTx<'_>, depot: DepotId) -> Result<(), MetaError> {
        tx.conn().execute(
            "DELETE FROM mod_feed_event WHERE depot_id = ?1",
            vals![depot.0],
        )?;
        tx.conn().execute(
            "DELETE FROM mod_feed_depot WHERE depot_id = ?1",
            vals![depot.0],
        )?;
        Ok(())
    }

    fn worker(&self, ctx: ModuleCtx, mut stop: Stop) -> Option<BoxFuture<'static, ()>> {
        let this = self.clone();
        Some(Box::pin(async move {
            loop {
                if stop.is_stopped() {
                    return;
                }
                if let Err(e) = this.prune(&ctx).await {
                    tracing::error!(error = %e, "pruning the event feed failed");
                }
                tokio::select! {
                    () = stop.wait() => return,
                    () = tokio::time::sleep(PRUNE_EVERY) => {}
                }
            }
        }))
    }
}
