//! T017 acceptance: the module framework, exercised by a small example module.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use http_body_util::BodyExt;
use lode_core::blob_id;
use lode_meta::{Migration, Reads, WriteTx};
use lode_proto::commit::CommitRequest;
use lode_server::commit::{CommitCtx, CommittedCtx, Origin};
use lode_server::events::Dispatcher;
use lode_server::module::{BoxError, BoxFuture, Event, Module, ModuleCtx, ScopeDef, ToolDef};
use lode_server::{init, open_state, ApiError, AppState, Caller, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Author, submitter, origin label and origin attributes of one commit.
type CommitSeen = (String, String, String, BTreeMap<String, String>);

/// What the example module observed.
#[derive(Default)]
struct Seen {
    events: Mutex<Vec<Event>>,
    /// (author, submitter, origin label, origin attrs) of every commit that reached pre_commit.
    commits: Mutex<Vec<CommitSeen>>,
    /// Event deliveries to fail before succeeding.
    fail_events: AtomicUsize,
}

/// Rejects empty messages and paths named in its `[example] forbid` configuration, records a
/// row per revision, counts events and offers one tool.
struct Example {
    name: &'static str,
    seen: Arc<Seen>,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: "CREATE TABLE mod_example_seen (rev INTEGER NOT NULL);",
    postgres: "CREATE TABLE mod_example_seen (rev BIGINT NOT NULL);",
}];

const SCOPES: &[ScopeDef] = &[ScopeDef {
    name: "example",
    agent_default: true,
}];

async fn count(Extension(seen): Extension<Arc<Seen>>, _caller: Caller) -> Json<Value> {
    Json(json!({ "events": seen.events.lock().unwrap().len() }))
}

async fn on_behalf(
    State(ctx): State<ModuleCtx>,
    Extension(_seen): Extension<Arc<Seen>>,
    caller: Caller,
    UrlPath(user): UrlPath<String>,
    Json(req): Json<CommitRequest>,
) -> Result<Json<Value>, ApiError> {
    let (depot, _) = ctx.depots().resolve(&caller, "game").await?;
    let author = ctx
        .meta()
        .read(move |tx| tx.principal_by_name(&user))
        .await?
        .ok_or_else(|| ApiError::not_found("no such user"))?;
    let attrs = BTreeMap::from([("ticket".to_owned(), "42".to_owned())]);
    let resp = ctx
        .commits()
        .commit(&caller, &depot, "main", req, Some(author), attrs)
        .await?;
    Ok(Json(json!({ "rev": resp.rev })))
}

impl Module for Example {
    fn name(&self) -> &'static str {
        self.name
    }

    fn scopes(&self) -> &'static [ScopeDef] {
        SCOPES
    }

    fn migrations(&self) -> &'static [Migration] {
        // A second instance under another name owns no tables.
        if self.name == "example" {
            MIGRATIONS
        } else {
            &[]
        }
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        if self.name != "example" {
            return None;
        }
        Some(
            Router::new()
                .route("/api/v1/example/count", get(count))
                .route("/api/v1/example/on-behalf/{user}", post(on_behalf))
                .layer(Extension(self.seen.clone()))
                .with_state(ctx),
        )
    }

    fn pre_commit(&self, _tx: &dyn Reads, ctx: &CommitCtx<'_>) -> Result<(), ApiError> {
        let attrs = match ctx.origin {
            Origin::Module { attrs, .. } => attrs.clone(),
            Origin::Direct => BTreeMap::new(),
        };
        self.seen.commits.lock().unwrap().push((
            ctx.author.name.clone(),
            ctx.submitter.name.clone(),
            ctx.origin.label().to_owned(),
            attrs,
        ));
        if ctx.changes.is_empty() {
            return Ok(());
        }
        if self.name == "example" && ctx.message.trim().is_empty() {
            return Err(ApiError::custom(
                "message_required",
                StatusCode::UNPROCESSABLE_ENTITY,
                "the example module needs a commit message",
            ));
        }
        // The module's own section of the branch configuration, already parsed.
        if let Some(forbid) = ctx
            .config_section("example")
            .and_then(|s| s.get("forbid"))
            .and_then(|v| v.as_str())
        {
            if ctx.changes.iter().any(|c| c.path.as_str() == forbid) {
                return Err(ApiError::custom(
                    "forbidden_path",
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("{forbid} is forbidden by [example]"),
                ));
            }
        }
        Ok(())
    }

    fn in_commit(&self, tx: &WriteTx<'_>, ctx: &CommittedCtx<'_>) -> Result<(), ApiError> {
        if self.name != "example" {
            return Ok(());
        }
        tx.conn()
            .execute(
                "INSERT INTO mod_example_seen (rev) VALUES (?1)",
                lode_meta::vals![ctx.rev as i64],
            )
            .map_err(|_| ApiError::internal())?;
        if ctx
            .commit
            .changes
            .iter()
            .any(|c| c.path.as_str() == "boom.txt")
        {
            return Err(ApiError::custom(
                "boom",
                StatusCode::CONFLICT,
                "in_commit failed on purpose",
            ));
        }
        Ok(())
    }

    fn on_event<'a>(
        &'a self,
        _ctx: &'a ModuleCtx,
        event: &'a Event,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            // A plain compare-exchange: `fetch_update` is deprecated on newer toolchains and its
            // replacement is newer than the minimum supported Rust version.
            let n = self.seen.fail_events.load(Ordering::SeqCst);
            let fail = n > 0
                && self
                    .seen
                    .fail_events
                    .compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
            if fail {
                return Err("not now".into());
            }
            self.seen.events.lock().unwrap().push(event.clone());
            Ok(())
        })
    }

    fn mcp_tools(&self, _ctx: ModuleCtx) -> Vec<ToolDef> {
        vec![ToolDef {
            name: format!("{}_echo", self.name),
            description: "Returns its input.".into(),
            input_schema: json!({ "type": "object" }),
            scope: "read".into(),
            handler: Arc::new(|_caller, input| Box::pin(async move { Ok(input) })),
        }]
    }
}

struct H {
    state: AppState,
    admin: String,
}

async fn open(dir: &Path, modules: Vec<Arc<dyn Module>>) -> AppState {
    open_state(
        ServerConfig::with_data_dir(dir),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        modules,
    )
    .await
    .unwrap()
}

impl H {
    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
        let body = if body.is_null() {
            Body::empty()
        } else {
            b = b.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let resp = lode_server::router(self.state.clone())
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn blob(&self, content: &[u8]) -> String {
        let id = blob_id(content).unwrap();
        let req = Request::put(format!("/api/v1/depots/game/blobs/{id}"))
            .header("authorization", format!("Bearer {}", self.admin))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let resp = lode_server::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert!(resp.status().is_success());
        id.to_string()
    }

    async fn commit(&self, rid: &str, message: &str, path: &str) -> (StatusCode, Value) {
        let id = self.blob(path.as_bytes()).await;
        self.send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            &self.admin,
            json!({ "request_id": rid, "message": message, "changes": [
                { "path": path, "action": "add", "new_id": id, "kind": "file", "size": path.len() }
            ] }),
        )
        .await
    }

    async fn head(&self) -> u64 {
        self.state
            .meta()
            .read(|tx| {
                let d = tx.depot_by_name("game")?.unwrap();
                Ok(tx.branch(d.id, "main")?.unwrap().head_rev)
            })
            .await
            .unwrap()
    }

    async fn outbox_len(&self) -> usize {
        self.state
            .meta()
            .read(|tx| tx.outbox_after(0, 10_000))
            .await
            .unwrap()
            .len()
    }
}

async fn setup(modules: Vec<Arc<dyn Module>>) -> (H, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let state = open(dir.path(), modules).await;
    let h = H { state, admin };
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin,
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    (h, dir)
}

fn example(seen: &Arc<Seen>) -> Arc<dyn Module> {
    Arc::new(Example {
        name: "example",
        seen: seen.clone(),
    })
}

#[tokio::test]
async fn pre_commit_rejects_with_its_own_code() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    let (s, body) = h.commit("a", "  ", "a.txt").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "message_required");
    assert_eq!(body["message"], "the example module needs a commit message");
    assert_eq!(h.head().await, 1);
    let (s, _) = h.commit("b", "fine", "a.txt").await;
    assert_eq!(s, StatusCode::CREATED);
}

#[tokio::test]
async fn in_commit_failure_rolls_everything_back() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    let before = h.outbox_len().await;
    let (s, body) = h.commit("boom", "m", "boom.txt").await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "boom");
    assert_eq!(h.head().await, 1);
    assert_eq!(h.outbox_len().await, before);
    let rows: i64 = h
        .state
        .meta()
        .with_writer(|c| c.value("SELECT count(*) FROM mod_example_seen", lode_meta::vals![]))
        .unwrap();
    assert_eq!(rows, 0);
    let rev2 = h
        .state
        .meta()
        .read(|tx| {
            let d = tx.depot_by_name("game")?.unwrap();
            tx.revision(d.id, 2)
        })
        .await
        .unwrap();
    assert!(rev2.is_none());
    // The number was not used up either way; the next commit gets r2.
    let (s, body) = h.commit("ok", "m", "ok.txt").await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(body["rev"], 2);
}

#[tokio::test]
async fn events_survive_a_restart_before_delivery() {
    let seen = Arc::new(Seen::default());
    let dir = {
        let (h, dir) = setup(vec![example(&seen)]).await;
        let (s, _) = h.commit("a", "first", "a.txt").await;
        assert_eq!(s, StatusCode::CREATED);
        // The server stops here, before any dispatcher pass.
        dir
    };
    assert!(seen.events.lock().unwrap().is_empty());
    let state = open(dir.path(), vec![example(&seen)]).await;
    let report = Dispatcher::default().pass(&state).await;
    assert_eq!(report.delivered, 1);
    let events = seen.events.lock().unwrap().clone();
    assert_eq!(events[0].event, "revision.created");
    assert_eq!(events[0].depot.as_deref(), Some("game"));
    assert_eq!(events[0].data["rev"], 2);
    assert_eq!(events[0].data["origin"], "direct");
    // Delivered once: a second pass has nothing.
    assert_eq!(Dispatcher::default().pass(&state).await.delivered, 0);
}

#[tokio::test]
async fn a_failing_subscriber_does_not_block_the_others() {
    let ok = Arc::new(Seen::default());
    let flaky = Arc::new(Seen::default());
    flaky.fail_events.store(1, Ordering::SeqCst);
    let modules: Vec<Arc<dyn Module>> = vec![
        example(&ok),
        Arc::new(Example {
            name: "flaky",
            seen: flaky.clone(),
        }),
    ];
    let (h, _dir) = setup(modules).await;
    h.commit("a", "one", "a.txt").await;
    h.commit("b", "two", "b.txt").await;
    let mut d = Dispatcher::default();
    let report = d.pass(&h.state).await;
    assert_eq!(report.failed, 1);
    assert_eq!(ok.events.lock().unwrap().len(), 2);
    assert!(flaky.events.lock().unwrap().is_empty());
    // Not pruned past the slow subscriber.
    assert_eq!(h.outbox_len().await, 2);
    // Backing off: an immediate pass leaves it alone.
    assert_eq!(d.pass(&h.state).await.failed, 0);
    assert!(flaky.events.lock().unwrap().is_empty());
    // A later pass (a fresh dispatcher has no backoff) delivers in order.
    Dispatcher::default().pass(&h.state).await;
    let ids: Vec<i64> = flaky.events.lock().unwrap().iter().map(|e| e.id).collect();
    let ok_ids: Vec<i64> = ok.events.lock().unwrap().iter().map(|e| e.id).collect();
    assert_eq!(ids, ok_ids);
    assert_eq!(h.outbox_len().await, 0);
}

#[tokio::test]
async fn disabled_module_contributes_nothing() {
    let (h, _dir) = setup(vec![]).await;
    let (s, _) = h
        .send("GET", "/api/v1/example/count", &h.admin, Value::Null)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let tables: i64 = h
        .state
        .meta()
        .with_writer(|c| {
            c.tables()
                .map(|t| t.iter().filter(|n| n.starts_with("mod_example_")).count() as i64)
        })
        .unwrap();
    assert_eq!(tables, 0);
    assert!(h.state.tools().is_empty());
    // No hook: an empty message is fine.
    let (s, _) = h.commit("a", "", "a.txt").await;
    assert_eq!(s, StatusCode::CREATED);
    // Its scope is unknown.
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "t", "scopes": ["read", "example"] }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, info) = h.send("GET", "/api/v1/info", &h.admin, Value::Null).await;
    assert_eq!(info["modules"], json!([]));
}

#[tokio::test]
async fn enabled_module_routes_scopes_tools_and_info() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    let (_, info) = h.send("GET", "/api/v1/info", &h.admin, Value::Null).await;
    assert_eq!(info["modules"], json!(["example"]));
    let (s, body) = h
        .send("GET", "/api/v1/example/count", &h.admin, Value::Null)
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    // Module routes get the same authentication and error handling.
    let (s, body) = h
        .send("GET", "/api/v1/example/count", "lode_bad", Value::Null)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");

    let tool = h.state.tools().get("example_echo").unwrap();
    assert_eq!(tool.scope, "read");
    assert_eq!(h.state.scopes().agent_defaults(), vec!["example"]);
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/tokens",
            &h.admin,
            json!({ "name": "t", "scopes": ["read"] }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn hook_reads_its_config_section_and_sees_author_and_submitter() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    let cfg = b"[example]\nforbid = \"secret.txt\"\n";
    let id = h.blob(cfg).await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches/main/commits",
            &h.admin,
            json!({ "request_id": "cfg", "message": "config", "changes": [
                { "path": ".lodeconfig", "action": "add", "new_id": id, "kind": "file", "size": cfg.len() }
            ] }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let (s, body) = h.commit("s", "m", "secret.txt").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "forbidden_path");

    // A commit on someone's behalf, through the module's own route.
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/admin/principals",
            &h.admin,
            json!({ "kind": "user", "name": "ana" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let blob = h.blob(b"x").await;
    let (s, body) = h
        .send(
            "POST",
            "/api/v1/example/on-behalf/ana",
            &h.admin,
            json!({ "request_id": "ob", "message": "for ana", "changes": [
                { "path": "x.txt", "action": "add", "new_id": blob, "kind": "file", "size": 1 }
            ] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let last = seen.commits.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.0, "ana");
    assert_eq!(last.1, "admin");
    assert_eq!(last.2, "example");
    assert_eq!(last.3.get("ticket").map(String::as_str), Some("42"));
    // A direct commit has author = submitter and origin direct.
    let direct = seen.commits.lock().unwrap()[0].clone();
    assert_eq!(
        (direct.0.as_str(), direct.1.as_str(), direct.2.as_str()),
        ("admin", "admin", "direct")
    );

    Dispatcher::default().pass(&h.state).await;
    let events = seen.events.lock().unwrap().clone();
    let ev = events.last().unwrap();
    assert_eq!(ev.data["author"], "ana");
    assert_eq!(ev.data["submitter"], "admin");
    assert_eq!(ev.data["origin"], "example");
}

#[tokio::test]
async fn outbox_is_pruned_and_stale_cursors_dropped() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    // A cursor left behind by a module that is now disabled.
    h.state
        .meta()
        .write(|tx| tx.set_outbox_cursor("retired", 0))
        .await
        .unwrap();
    h.commit("a", "one", "a.txt").await;
    h.commit("b", "two", "b.txt").await;
    assert!(h.outbox_len().await > 0);
    let report = Dispatcher::default().pass(&h.state).await;
    assert_eq!(report.delivered, 2);
    assert_eq!(h.outbox_len().await, 0);
    let cursors = h.state.meta().read(|tx| tx.outbox_cursors()).await.unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "example");
}

#[tokio::test]
async fn spawned_dispatcher_delivers_after_commits() {
    let seen = Arc::new(Seen::default());
    let (h, _dir) = setup(vec![example(&seen)]).await;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = Dispatcher::spawn(h.state.clone(), async move {
        let _ = stopped.await;
    });
    h.commit("a", "one", "a.txt").await;
    for _ in 0..200 {
        if !seen.events.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(seen.events.lock().unwrap().len(), 1);
    stop.send(()).unwrap();
    task.await.unwrap();
}
