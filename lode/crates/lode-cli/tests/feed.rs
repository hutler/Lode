//! T192/T193 acceptance: the event feed over HTTP, `lode events`, and `wait_for_events` in
//! `lode mcp`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::depot::CreateBranchRequest;
use lode_proto::feed::{EventList, EventsQuery};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Value};

fn q(after: Option<i64>, kinds: Option<&str>, wait: u64) -> EventsQuery {
    EventsQuery {
        after,
        kinds: kinds.map(str::to_owned),
        wait: Some(wait),
    }
}

fn names(l: &EventList) -> Vec<String> {
    l.events
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_owned())
        .collect()
}

/// Reads from `after` until `want` events arrived (the dispatcher delivers them shortly
/// after each change), returning them and the cursor.
async fn collect(
    c: &lode_client::Client,
    depot: &str,
    mut after: i64,
    want: usize,
) -> (Vec<Value>, i64) {
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while got.len() < want {
        assert!(Instant::now() < deadline, "only {got:?} arrived");
        let l = c.events(depot, &q(Some(after), None, 5)).await.unwrap();
        after = l.next;
        got.extend(l.events);
    }
    (got, after)
}

async fn setup() -> (TestServer, String) {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"one\n")])
        .await;
    let lea = server
        .create_user("lea", &["read", "write", "lock", "shelve"])
        .await;
    server
        .acl(
            "game",
            "lea",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    (server, lea)
}

fn workspace(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_of_commits_locks_and_shelves() {
    let (server, lea) = setup().await;
    let c = server.client(&lea);
    let ws = workspace(&server, &lea);
    // The first event (the setup commit) may still be on its way: wait for it, then start
    // from now.
    collect(&c, "game", 0, 1).await;
    let start = ws.json(&["events"]);
    assert_eq!(start["events"], json!([]));
    let start = start["next"].as_i64().unwrap();
    assert!(start > 0);

    let rev = server
        .commit_files("game", "main", &[("a.txt", b"two\n")])
        .await;
    ws.json(&["update"]);
    ws.json(&["lock", "a.txt"]);
    ws.write("b.txt", "new\n");
    ws.json(&["add", "b.txt"]);
    let shelf = ws.json(&["shelve", "-m", "proposal"]);
    let (events, next) = collect(&c, "game", start, 3).await;
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["revision.created", "lock.acquired", "shelf.created"],
        "{events:?}"
    );
    assert_eq!(events[0]["data"]["rev"], rev);
    assert_eq!(events[0]["depot"], "game");
    assert_eq!(events[2]["data"]["shelf_id"], shelf["shelf"]);
    // Ids only: no paths, no content.
    assert!(!events.iter().any(|e| e.to_string().contains("b.txt")));

    // The CLI reads the same, and filters by kind.
    let cli = ws.json(&[
        "events",
        "--after",
        &start.to_string(),
        "--kinds",
        "shelf.created",
    ]);
    assert_eq!(cli["events"].as_array().unwrap().len(), 1, "{cli}");
    assert_eq!(cli["events"][0]["event"], "shelf.created");
    let (code, bad) = ws.json_status(&["events", "--after", "0", "--kinds", "nope"]);
    assert_ne!(code, 0);
    assert_eq!(bad["error"]["code"], "invalid_request", "{bad}");

    // Nothing new: a wait runs out empty and keeps the cursor.
    let empty = c.events("game", &q(Some(next), None, 1)).await.unwrap();
    assert!(empty.events.is_empty());
    assert_eq!(empty.next, next);
    assert!(!empty.truncated);

    // A waiting request returns as soon as an event arrives.
    let waiter = {
        let c = c.clone();
        tokio::spawn(async move {
            let t = Instant::now();
            let l = c.events("game", &q(Some(next), None, 50)).await.unwrap();
            (l, t.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    ws.json(&["unlock", "a.txt"]);
    let (l, took) = waiter.await.unwrap();
    assert_eq!(names(&l), ["lock.released"], "{l:?}");
    assert!(took < Duration::from_secs(40), "{took:?}");

    // A token without `read` is refused.
    let blind = server.token_for("lea", &["lock"]).await;
    let e = server
        .client(&blind)
        .events("game", &q(Some(0), None, 0))
        .await
        .unwrap_err();
    assert!(e.is_code(lode_core::ErrorCode::PermissionDenied), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_on_unreadable_branches_are_left_out() {
    let (server, _) = setup().await;
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: "secret".into(),
                from_branch: "main".into(),
                from_rev: None,
                stream: None,
            },
        )
        .await
        .unwrap();
    let bob = server.create_user("bob", &["read"]).await;
    server
        .acl(
            "game",
            "bob",
            "main",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    let secret = server
        .commit_files("game", "secret", &[("plan.txt", b"x\n")])
        .await;
    let public = server
        .commit_files("game", "main", &[("a.txt", b"two\n")])
        .await;
    // The admin sees both branches' events; bob only main's.
    let all = collect(&server.admin(), "game", 0, 4).await.0;
    let revs = |evs: &[Value]| -> Vec<u64> {
        evs.iter()
            .filter(|e| e["event"] == "revision.created")
            .map(|e| e["data"]["rev"].as_u64().unwrap())
            .collect()
    };
    assert!(revs(&all).contains(&secret), "{all:?}");
    let bobs = server
        .client(&bob)
        .events("game", &q(Some(0), None, 0))
        .await
        .unwrap();
    assert!(revs(&bobs.events).contains(&public), "{bobs:?}");
    assert!(!revs(&bobs.events).contains(&secret), "{bobs:?}");
    assert!(!bobs.events.iter().any(|e| e["data"]["branch"] == "secret"));
    // A user with no access to the depot does not learn it exists.
    let eve = server.create_user("eve", &["read"]).await;
    let e = server
        .client(&eve)
        .events("game", &q(Some(0), None, 0))
        .await
        .unwrap_err();
    assert!(e.is_code(lode_core::ErrorCode::NotFound), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_events_are_dropped_and_the_cursor_says_so() {
    let (mut server, lea) = setup().await;
    let c = server.client(&lea);
    let (_, next) = collect(&c, "game", 0, 1).await;
    // A week and a day later the server restarts; the feed prunes on start.
    server.clock().advance_ms(8 * 24 * 3_600_000);
    server.restart().await;
    let c = server.client(&lea);
    let deadline = Instant::now() + Duration::from_secs(30);
    let l = loop {
        let l = c.events("game", &q(Some(0), None, 0)).await.unwrap();
        if l.truncated {
            break l;
        }
        assert!(Instant::now() < deadline, "never pruned: {l:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(l.events.is_empty(), "{l:?}");
    // From the cursor a client kept, nothing is missing, and new events still arrive.
    let kept = c.events("game", &q(Some(next), None, 0)).await.unwrap();
    assert!(!kept.truncated);
    let rev = server
        .commit_files("game", "main", &[("a.txt", b"after\n")])
        .await;
    let (evs, _) = collect(&c, "game", next, 1).await;
    assert_eq!(evs[0]["data"]["rev"], rev);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_module_there_is_no_feed() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.feed = false),
    })
    .await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    let (code, out) = ws.json_status(&["events"]);
    assert_ne!(code, 0);
    assert_eq!(out["error"]["code"], "module_disabled", "{out}");
}

type McpClient = RunningService<RoleClient, ()>;

async fn call_raw(c: &McpClient, name: &str, args: Value) -> CallToolResult {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    c.call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn call(c: &McpClient, name: &str, args: Value) -> Value {
    let r = call_raw(c, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.expect("structured result")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lode_mcp_waits_for_a_new_revision_without_blocking_other_tools() {
    let (server, lea) = setup().await;
    let c = server.client(&lea);
    collect(&c, "game", 0, 1).await;
    let ws = workspace(&server, &lea);
    let mut cmd = ws.std_cmd();
    cmd.arg("mcp");
    let transport = TokioChildProcess::new(tokio::process::Command::from(cmd)).unwrap();
    let mcp: McpClient = ().serve(transport).await.expect("start lode mcp");
    let mcp = std::sync::Arc::new(mcp);

    let start = call(&mcp, "wait_for_events", json!({})).await;
    assert_eq!(start["events"], json!([]));
    let after = start["next"].as_i64().unwrap();

    let waiting = {
        let mcp = mcp.clone();
        tokio::spawn(async move {
            call(
                &mcp,
                "wait_for_events",
                json!({ "after": after, "kinds": ["revision.created"], "timeout_s": 50 }),
            )
            .await
        })
    };
    // Other tools answer while the wait is held.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = call(&mcp, "status", json!({})).await;
    assert_eq!(s["branch"], "main");
    let rev = server
        .commit_files("game", "main", &[("a.txt", b"two\n")])
        .await;
    let got = waiting.await.unwrap();
    assert_eq!(got["events"][0]["event"], "revision.created", "{got}");
    assert_eq!(got["events"][0]["data"]["rev"], rev);
    // Nothing more: a short wait answers empty with the same cursor.
    let next = got["next"].as_i64().unwrap();
    let none = call(
        &mcp,
        "wait_for_events",
        json!({ "after": next, "timeout_s": 1 }),
    )
    .await;
    assert_eq!(none["events"], json!([]));
    assert_eq!(none["next"], next);
}
