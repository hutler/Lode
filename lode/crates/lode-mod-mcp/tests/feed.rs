//! T193 acceptance: `wait_for_events` on the server MCP endpoint.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestServerOptions};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};

type McpClient = RunningService<RoleClient, ()>;

async fn connect(server: &TestServer, token: &str) -> McpClient {
    let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", server.url()))
        .auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("connect")
}

async fn call_raw(client: &McpClient, name: &str, args: Value) -> CallToolResult {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn call(client: &McpClient, name: &str, args: Value) -> Value {
    let r = call_raw(client, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.expect("structured result")
}

async fn tool_names(client: &McpClient) -> Vec<String> {
    client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.into_owned())
        .collect()
}

/// Waits from `after` until one event of `kind` arrives.
async fn wait_for(c: &McpClient, after: i64, kind: &str) -> (Value, i64) {
    let mut after = after;
    for _ in 0..10 {
        let got = call(
            c,
            "wait_for_events",
            json!({ "depot": "game", "after": after, "kinds": [kind], "timeout_s": 10 }),
        )
        .await;
        after = got["next"].as_i64().unwrap();
        if let Some(e) = got["events"].as_array().unwrap().first() {
            return (e.clone(), after);
        }
    }
    panic!("no {kind} arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_sees_a_shelf_another_user_made() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let base = server
        .commit_files("game", "main", &[("a.txt", b"one\n")])
        .await;
    server.create_user("ana", &["read"]).await;
    let bot = server
        .create_agent("bot", "ana", &["read", "shelve", "lock"])
        .await;
    let lea = server.create_user("lea", &["read", "shelve", "lock"]).await;
    for who in ["bot", "lea"] {
        server
            .acl("game", who, "*", "**", AclEffect::Allow, AccessLevel::Read)
            .await;
    }
    let agent = connect(&server, &bot).await;
    assert!(tool_names(&agent)
        .await
        .contains(&"wait_for_events".to_owned()));
    // The setup commit has arrived; from there the agent waits for shelves.
    let (_, after) = wait_for(&agent, 0, "revision.created").await;
    let start = call(&agent, "wait_for_events", json!({ "depot": "game" })).await;
    assert_eq!(start["events"], json!([]));
    assert!(start["next"].as_i64().unwrap() >= after);

    let person = connect(&server, &lea).await;
    let shelf = call(
        &person,
        "create_shelf",
        json!({
            "depot": "game", "branch": "main", "base_rev": base,
            "description": "For the agent",
            "edits": [{ "path": "a.txt", "action": "modify", "text": "two\n" }]
        }),
    )
    .await;
    let (event, next) = wait_for(&agent, after, "shelf.created").await;
    assert_eq!(event["data"]["shelf_id"], shelf["id"], "{event}");
    assert_eq!(event["data"]["owner"], "lea");

    // Nothing more within a second; asked again from the same cursor, the same answer.
    let a = call(
        &agent,
        "wait_for_events",
        json!({ "depot": "game", "after": next, "timeout_s": 1 }),
    )
    .await;
    assert_eq!(a["events"], json!([]));
    let again = call(
        &agent,
        "wait_for_events",
        json!({ "depot": "game", "after": after, "timeout_s": 0 }),
    )
    .await;
    let again2 = call(
        &agent,
        "wait_for_events",
        json!({ "depot": "game", "after": after, "timeout_s": 0 }),
    )
    .await;
    assert_eq!(again, again2);
    agent.cancel().await.unwrap();
    person.cancel().await.unwrap();
}

#[tokio::test]
async fn no_tool_without_the_module() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.feed = false),
    })
    .await;
    let c = connect(&server, server.admin_token()).await;
    assert!(!tool_names(&c).await.contains(&"wait_for_events".to_owned()));
    c.cancel().await.unwrap();
}
