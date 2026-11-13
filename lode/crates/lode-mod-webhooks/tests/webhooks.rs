//! T060 acceptance: webhooks registered, matched, signed, retried, given up and resumed.
//!
//! The server runs in process with a manual clock; the delivery worker is driven round by
//! round with `Webhooks::run_once`, so retries are tested by moving the clock, not by
//! waiting. A small HTTP receiver on a loopback port records what arrives.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use lode_core::blob_id;
use lode_meta::Reads;
use lode_mod_webhooks::{retry_delay, signature, Webhooks, GIVE_UP_AFTER_MS};
use lode_server::events::Dispatcher;
use lode_server::module::{Module, Stop};
use lode_server::{init, open_state, AppState, Clock, ManualClock, ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

// ---- receiver ----

#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: HeaderMap,
    body: Bytes,
}

#[derive(Default)]
struct RecvState {
    got: Vec<Received>,
    /// Status to answer per path; 200 when absent.
    status: HashMap<String, u16>,
}

#[derive(Clone, Default)]
struct Receiver(Arc<Mutex<RecvState>>);

impl Receiver {
    async fn start() -> (Self, String) {
        let r = Receiver::default();
        let shared = r.clone();
        let app = axum::Router::new().fallback(move |req: Request| {
            let shared = shared.clone();
            async move { shared.handle(req).await }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (r, url)
    }

    async fn handle(&self, req: Request) -> Response {
        let path = req.uri().path().to_owned();
        let headers = req.headers().clone();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let status = {
            let mut s = self.0.lock().unwrap();
            s.got.push(Received {
                path: path.clone(),
                headers,
                body,
            });
            s.status.get(&path).copied().unwrap_or(200)
        };
        if status == 302 {
            return Response::builder()
                .status(302)
                .header("location", "/ok")
                .body(Body::empty())
                .unwrap();
        }
        StatusCode::from_u16(status).unwrap().into_response()
    }

    fn set(&self, path: &str, status: u16) {
        self.0.lock().unwrap().status.insert(path.into(), status);
    }

    fn at(&self, path: &str) -> Vec<Received> {
        self.0
            .lock()
            .unwrap()
            .got
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }
}

fn header<'a>(r: &'a Received, name: &str) -> &'a str {
    r.headers.get(name).unwrap().to_str().unwrap()
}

// ---- server ----

/// `(path, new content, old content for a modification)`.
type FileEdit<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

struct H {
    state: AppState,
    clock: ManualClock,
    wh: Webhooks,
    admin: String,
    dir: tempfile::TempDir,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn config(dir: &std::path::Path, webhooks: bool, allow_http: bool) -> ServerConfig {
    let mut c = ServerConfig::with_data_dir(dir);
    c.modules.webhooks = webhooks;
    c.webhooks.allow_http = allow_http;
    c
}

async fn open(
    dir: &std::path::Path,
    clock: &ManualClock,
    cfg: ServerConfig,
) -> (AppState, Webhooks) {
    let wh = Webhooks::new();
    let available: Vec<Arc<dyn Module>> =
        vec![lode_mod_locks::Locks::module(), Arc::new(wh.clone())];
    let modules = lode_server::module::select_enabled(&cfg.modules, available);
    let _ = dir;
    let state = open_state(cfg, Arc::new(clock.clone()), modules)
        .await
        .unwrap();
    (state, wh)
}

async fn setup_with(allow_http: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let clock = ManualClock::new(now_ms());
    let (state, wh) = open(dir.path(), &clock, config(dir.path(), true, allow_http)).await;
    let h = H {
        state,
        clock,
        wh,
        admin,
        dir,
    };
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots",
            &h.admin.clone(),
            json!({ "name": "game" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h
}

async fn setup() -> H {
    setup_with(true).await
}

fn next() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

impl H {
    async fn send(&self, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        send(&self.state, method, uri, token, body).await
    }

    async fn hook(&self, url: &str, events: &[&str], branch: Option<&str>) -> (i64, String) {
        let mut body = json!({ "url": url, "events": events });
        if let Some(b) = branch {
            body["branch"] = json!(b);
        }
        let (s, v) = self
            .send("POST", "/api/v1/depots/game/webhooks", &self.admin, body)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        (
            v["hook"]["id"].as_i64().unwrap(),
            v["secret"].as_str().unwrap().to_owned(),
        )
    }

    /// Commits `files` to `branch` as additions or modifications.
    async fn commit(&self, branch: &str, files: &[FileEdit<'_>]) -> u64 {
        let mut changes = Vec::new();
        for (path, content, old) in files {
            let id = blob_id(*content).unwrap().to_string();
            let req = axum::http::Request::put(format!("/api/v1/depots/game/blobs/{id}"))
                .header("authorization", format!("Bearer {}", self.admin))
                .body(Body::from(content.to_vec()))
                .unwrap();
            let resp = lode_server::router(self.state.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert!(resp.status().is_success());
            let mut c =
                json!({ "path": path, "new_id": id, "kind": "file", "size": content.len() });
            match old {
                Some(o) => {
                    c["action"] = json!("modify");
                    c["base_id"] = json!(blob_id(*o).unwrap().to_string());
                }
                None => c["action"] = json!("add"),
            }
            changes.push(c);
        }
        let (s, v) = self
            .send(
                "POST",
                &format!("/api/v1/depots/game/branches/{branch}/commits"),
                &self.admin,
                json!({ "request_id": format!("r{}", next()), "message": "m", "changes": changes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["rev"].as_u64().unwrap()
    }

    /// Hands pending outbox events to the module.
    async fn dispatch(&self) {
        Dispatcher::default().pass(&self.state).await;
    }

    async fn round(&self) -> lode_mod_webhooks::RunReport {
        self.wh
            .run_once(&self.state.module_ctx("webhooks"))
            .await
            .unwrap()
    }

    async fn deliveries(&self, hook: i64) -> Vec<Value> {
        let (s, v) = self
            .send(
                "GET",
                &format!("/api/v1/depots/game/webhooks/{hook}/deliveries"),
                &self.admin,
                Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["deliveries"].as_array().unwrap().clone()
    }

    async fn user(&self, name: &str, scopes: &[&str]) -> String {
        let (s, v) = self
            .send(
                "POST",
                "/api/v1/admin/principals",
                &self.admin,
                json!({ "kind": "user", "name": name }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let (s, v) = self
            .send(
                "POST",
                &format!("/api/v1/admin/principals/{name}/tokens"),
                &self.admin,
                json!({ "name": "t", "scopes": scopes }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let body = if body.is_null() {
        Body::empty()
    } else {
        b = b.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let resp = lode_server::router(state.clone())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_events_arrive_signed_once() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    let (main_hook, secret) = h
        .hook(
            &format!("{base}/main"),
            &["revision.created", "branch.created"],
            Some("main"),
        )
        .await;
    let (all_hook, _) = h
        .hook(&format!("{base}/all"), &["branch.created"], None)
        .await;

    let r1 = h
        .commit("main", &[("Source/secret-name.cpp", b"one\n", None)])
        .await;
    let (s, _) = h
        .send(
            "POST",
            "/api/v1/depots/game/branches",
            &h.admin,
            json!({ "name": "dev", "from_branch": "main" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    h.commit(
        "dev",
        &[("Source/secret-name.cpp", b"two\n", Some(b"one\n"))],
    )
    .await;
    h.dispatch().await;
    // Delivered twice over: the outbox gives events at least once, deliveries are unique.
    h.dispatch().await;
    let report = h.round().await;
    assert_eq!(report.delivered, 2, "{report:?}");
    assert_eq!(h.round().await.delivered, 0);

    // `main` gets the revision on main only; branch.created is on `dev`, outside its pattern.
    let got = rx.at("/main");
    assert_eq!(got.len(), 1, "{got:?}");
    let req = &got[0];
    assert_eq!(header(req, "x-lode-event"), "revision.created");
    assert_eq!(
        header(req, "x-lode-signature"),
        signature(secret.as_bytes(), &req.body)
    );
    assert_eq!(header(req, "content-type"), "application/json");
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["event"], "revision.created");
    assert_eq!(body["depot"], "game");
    assert_eq!(body["data"]["rev"], r1);
    assert_eq!(body["data"]["branch"], "main");
    // Identifiers only: no path and no content.
    let text = String::from_utf8_lossy(&req.body);
    assert!(
        !text.contains("secret-name") && !text.contains("one"),
        "{text}"
    );
    let ds = h.deliveries(main_hook).await;
    assert_eq!(ds.len(), 1);
    assert_eq!(header(req, "x-lode-delivery"), ds[0]["id"].to_string());
    assert_eq!(ds[0]["state"], "ok");
    assert_eq!(ds[0]["last_status"], 200);

    // The unfiltered hook gets branch.created for `dev`, and nothing else.
    let got = rx.at("/all");
    assert_eq!(got.len(), 1);
    assert_eq!(header(&got[0], "x-lode-event"), "branch.created");
    assert_eq!(h.deliveries(all_hook).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_retry_on_schedule_until_24_hours() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    let (flaky, _) = h
        .hook(&format!("{base}/flaky"), &["revision.created"], None)
        .await;
    let (dead, _) = h
        .hook(&format!("{base}/dead"), &["revision.created"], None)
        .await;
    h.hook(&format!("{base}/good"), &["revision.created"], None)
        .await;
    rx.set("/flaky", 500);
    rx.set("/dead", 503);

    h.commit("main", &[("a.txt", b"a\n", None)]).await;
    h.dispatch().await;
    let r = h.round().await;
    assert_eq!((r.delivered, r.retrying), (1, 2), "{r:?}");
    let first = h.clock.now_ms();
    assert_eq!(r.next_due, Some(first + 10_000));

    // Not due yet: nothing is sent.
    h.clock.advance_ms(9_000);
    let r = h.round().await;
    assert_eq!((r.delivered, r.retrying), (0, 0));
    h.clock.advance_ms(1_000);
    let r = h.round().await;
    assert_eq!(r.retrying, 2);

    // The flaky receiver recovers; its next attempt (after one minute) succeeds, with the
    // same delivery id as the failed attempts.
    rx.set("/flaky", 204);
    h.clock.advance_ms(60_000);
    let r = h.round().await;
    assert_eq!((r.delivered, r.retrying), (1, 1), "{r:?}");
    let got = rx.at("/flaky");
    assert_eq!(got.len(), 3);
    let ids: Vec<&str> = got.iter().map(|g| header(g, "x-lode-delivery")).collect();
    assert!(ids.iter().all(|i| *i == ids[0]), "{ids:?}");
    let ds = h.deliveries(flaky).await;
    assert_eq!(ds[0]["state"], "ok");
    assert_eq!(ds[0]["attempts"], 3);

    // A new revision while the dead hook keeps failing: the good hook gets it at once, and
    // the outbox is emptied even though one receiver is down.
    h.commit("main", &[("b.txt", b"b\n", None)]).await;
    h.dispatch().await;
    let outbox: i64 = h
        .state
        .meta()
        .read(|tx| {
            Ok(tx
                .db()
                .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(outbox, 0);
    let r = h.round().await;
    assert_eq!(r.delivered, 2, "{r:?}"); // good and flaky
    assert_eq!(rx.at("/good").len(), 2);

    // The dead hook: attempts follow the schedule, the last no later than 24 h after the
    // first, then the delivery is `failed`.
    let mut attempts = rx.at("/dead").len() as u32;
    assert_eq!(attempts, 3);
    let mut at = first + 10_000 + 60_000;
    loop {
        at += retry_delay(attempts);
        if at > first + GIVE_UP_AFTER_MS {
            break;
        }
        h.clock.set_ms(at);
        let r = h.round().await;
        attempts += 1;
        assert_eq!(rx.at("/dead").len() as u32, attempts);
        if r.given_up == 1 {
            break;
        }
    }
    let ds = h.deliveries(dead).await;
    let oldest = ds.last().unwrap();
    assert_eq!(oldest["state"], "failed", "{ds:?}");
    assert_eq!(oldest["last_status"], 503);
    // 10 s, 1 min, 5 min, 30 min, then hourly: 5 attempts in the first 37 minutes and one
    // per hour after that, up to 24 hours.
    assert_eq!(attempts, 5 + 23);
    // The dead hook's second delivery is next, and it is attempted now.
    h.clock.advance_ms(1);
    let before = rx.at("/dead").len();
    h.round().await;
    assert_eq!(rx.at("/dead").len(), before + 1);

    // A week later the finished deliveries are pruned.
    h.clock.advance_ms(8 * 24 * 3_600_000);
    let r = h.round().await;
    assert!(r.pruned >= 4, "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_not_followed() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    let (hook, _) = h
        .hook(&format!("{base}/moved"), &["revision.created"], None)
        .await;
    rx.set("/moved", 302);
    h.commit("main", &[("a.txt", b"a\n", None)]).await;
    h.dispatch().await;
    let r = h.round().await;
    assert_eq!((r.delivered, r.retrying), (0, 1));
    assert!(rx.at("/ok").is_empty());
    let ds = h.deliveries(hook).await;
    assert_eq!(ds[0]["state"], "pending");
    assert_eq!(ds[0]["last_status"], 302);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_loses_no_pending_delivery() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    h.hook(&format!("{base}/r"), &["revision.created"], None)
        .await;
    rx.set("/r", 500);
    let r1 = h.commit("main", &[("a.txt", b"a\n", None)]).await;
    let r2 = h.commit("main", &[("b.txt", b"b\n", None)]).await;
    h.dispatch().await;
    h.round().await;
    assert_eq!(rx.at("/r").len(), 1);

    // Stop the server, start it again on the same data.
    let H {
        state, clock, dir, ..
    } = h;
    drop(state);
    rx.set("/r", 200);
    let (state, wh) = open(dir.path(), &clock, config(dir.path(), true, true)).await;
    clock.advance_ms(10_000);
    let ctx = state.module_ctx("webhooks");
    wh.run_once(&ctx).await.unwrap();
    wh.run_once(&ctx).await.unwrap();
    let got = rx.at("/r");
    assert_eq!(got.len(), 3, "{got:?}");
    let revs: Vec<u64> = got[1..]
        .iter()
        .map(|g| {
            serde_json::from_slice::<Value>(&g.body).unwrap()["data"]["rev"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(revs, vec![r1, r2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_are_validated_and_secrets_stay_hidden() {
    let h = setup_with(false).await;
    let (s, v) = h
        .send(
            "POST",
            "/api/v1/depots/game/webhooks",
            &h.admin,
            json!({ "url": "http://127.0.0.1:9/x", "events": ["revision.created"] }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "invalid_request");
    for bad in [
        json!({ "url": "https://h/x", "events": [] }),
        json!({ "url": "https://h/x", "events": ["revision.made"] }),
        json!({ "url": "ftp://h/x", "events": ["revision.created"] }),
        json!({ "url": "https://u:p@h/x", "events": ["revision.created"] }),
        json!({ "url": "https://h/x", "events": ["revision.created"], "branch": "a[" }),
    ] {
        let (s, v) = h
            .send(
                "POST",
                "/api/v1/depots/game/webhooks",
                &h.admin,
                bad.clone(),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    let (id, secret) = h
        .hook("https://hooks.example/lode", &["revision.created"], None)
        .await;
    assert_eq!(secret.len(), 64);

    // Lists show no secret.
    let (s, v) = h
        .send("GET", "/api/v1/depots/game/webhooks", &h.admin, Value::Null)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["webhooks"][0]["id"], id);
    assert!(!v.to_string().contains(&secret));
    h.commit("main", &[("a.txt", b"a\n", None)]).await;
    h.dispatch().await;
    let ds = h.deliveries(id).await;
    assert_eq!(ds.len(), 1);
    assert!(!Value::Array(ds).to_string().contains(&secret));

    // Not for non-administrators.
    let ana = h.user("ana", &["read", "write"]).await;
    let (s, _) = h
        .send("GET", "/api/v1/depots/game/webhooks", &ana, Value::Null)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // /info names the module.
    let (_, info) = h.send("GET", "/api/v1/info", &h.admin, Value::Null).await;
    assert!(info["modules"]
        .as_array()
        .unwrap()
        .contains(&json!("webhooks")));

    // Remove: gone with its deliveries.
    let (s, _) = h
        .send(
            "DELETE",
            &format!("/api/v1/depots/game/webhooks/{id}"),
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = h
        .send(
            "GET",
            &format!("/api/v1/depots/game/webhooks/{id}/deliveries"),
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_is_delivered() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    let (id, secret) = h.hook(&format!("{base}/p"), &["shelf.created"], None).await;
    let (s, v) = h
        .send(
            "POST",
            &format!("/api/v1/depots/game/webhooks/{id}/ping"),
            &h.admin,
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["event"], "ping");
    assert_eq!(v["state"], "pending");
    h.round().await;
    let got = rx.at("/p");
    assert_eq!(got.len(), 1);
    assert_eq!(header(&got[0], "x-lode-event"), "ping");
    assert_eq!(
        header(&got[0], "x-lode-signature"),
        signature(secret.as_bytes(), &got[0].body)
    );
    let body: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(body["data"]["hook_id"], id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_worker_starts_with_the_server_and_stops_cleanly() {
    let h = setup().await;
    let (rx, base) = Receiver::start().await;
    h.hook(&format!("{base}/w"), &["revision.created"], None)
        .await;
    let (stop_tx, stop) = Stop::new();
    let workers = lode_server::module::spawn_workers(&h.state, &stop);
    assert_eq!(workers.len(), 1);
    h.commit("main", &[("a.txt", b"a\n", None)]).await;
    h.dispatch().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while rx.at("/w").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "nothing delivered");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop_tx.send(true).unwrap();
    for w in workers {
        tokio::time::timeout(Duration::from_secs(20), w)
            .await
            .expect("the worker stops")
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_module_has_no_routes_tables_or_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let admin = init(dir.path()).await.unwrap().admin_token;
    let clock = ManualClock::new(now_ms());
    let (state, _) = open(dir.path(), &clock, config(dir.path(), false, true)).await;
    let (s, _) = send(
        &state,
        "POST",
        "/api/v1/depots",
        &admin,
        json!({ "name": "game" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = send(
        &state,
        "GET",
        "/api/v1/depots/game/webhooks",
        &admin,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let tables: i64 = state
        .meta()
        .read(|tx| {
            Ok(tx.db().query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'mod_webhooks_%'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(tables, 0);
    let (stop_tx, stop) = Stop::new();
    let workers = lode_server::module::spawn_workers(&state, &stop);
    assert!(workers.is_empty());
    drop(stop_tx);
    let (_, info) = send(&state, "GET", "/api/v1/info", &admin, Value::Null).await;
    assert!(!info["modules"]
        .as_array()
        .unwrap()
        .contains(&json!("webhooks")));
}
