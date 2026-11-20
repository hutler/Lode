//! T203 acceptance: validation scripts through `lode mcp`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Value};

type McpClient = RunningService<RoleClient, ()>;

async fn start(ws: &TestWorkspace) -> McpClient {
    let mut cmd = ws.std_cmd();
    cmd.arg("mcp").args(["--agent-tool", "tester"]);
    let transport = TokioChildProcess::new(tokio::process::Command::from(cmd)).unwrap();
    ().serve(transport).await.expect("start lode mcp")
}

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

async fn call_err(c: &McpClient, name: &str, args: Value) -> String {
    let r = call_raw(c, name, args).await;
    assert_eq!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// An agent `bot` (read, shelve, lock) that may read depot `game`, with its workspace.
async fn agent_workspace(server: &TestServer) -> TestWorkspace {
    server.create_user("ana", &["read"]).await;
    let bot = server
        .create_agent("bot", "ana", &["read", "shelve", "lock"])
        .await;
    server
        .acl(
            "game",
            "bot",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    let ws = TestWorkspace::new(server).with_token(&bot);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

const CONFIG: &[u8] = b"[[validate.script]]\nname = \"json\"\nrun = \"tools/json_valid.py\"\npaths = [\"Data/**/*.json\"]\n";

const SCRIPT: &[u8] = br#"import json, sys
bad = 0
for f in json.load(sys.stdin)["files"]:
    try:
        json.load(open(f["path"], encoding="utf-8"))
    except ValueError as e:
        print(json.dumps({"path": f["path"], "message": "not JSON"}))
        bad += 1
sys.exit(1 if bad else 0)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_gets_the_same_refusal() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[(".lodeconfig", CONFIG), ("tools/json_valid.py", SCRIPT)],
        )
        .await;
    let ws = agent_workspace(&server).await;
    ws.write("Data/a.json", "{oops\n");
    let c = start(&ws).await;
    call(&c, "add", json!({ "paths": ["Data/a.json"] })).await;

    // The agent cannot trust the depot's scripts itself.
    assert_eq!(
        call_err(&c, "validate", json!({})).await,
        "validation_untrusted"
    );
    assert_eq!(
        call_err(&c, "shelve", json!({ "description": "a" })).await,
        "validation_untrusted"
    );
    let r = call_raw(&c, "validate", json!({ "trust": true })).await;
    assert_eq!(r.is_error, Some(true));

    // The person trusts them for this workspace.
    let _ = ws.json_status(&["validate", "--trust"]);

    let r = call_raw(&c, "validate", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    let e = r.structured_content.unwrap();
    assert_eq!(e["code"], "validation_failed", "{e}");
    assert_eq!(
        e["details"]["results"][0]["problems"][0]["path"],
        "Data/a.json"
    );
    assert_eq!(
        call_err(&c, "shelve", json!({ "description": "a" })).await,
        "validation_failed"
    );

    ws.write("Data/a.json", "{\"ok\": true}\n");
    let v = call(&c, "validate", json!({ "script": "json" })).await;
    assert_eq!(v["passed"], true, "{v}");
    assert_eq!(v["results"][0]["outcome"], "passed", "{v}");
    let v = call(&c, "shelve", json!({ "description": "a" })).await;
    assert!(v["shelf"].as_u64().is_some(), "{v}");
    c.cancel().await.unwrap();
}
