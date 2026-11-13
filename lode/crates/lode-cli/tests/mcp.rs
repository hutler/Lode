//! T042 acceptance: `lode mcp` driven over stdio by the MCP SDK's own client.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Value};

type McpClient = RunningService<RoleClient, ()>;

/// Starts `lode mcp` in the workspace with `args`.
async fn start(ws: &TestWorkspace, args: &[&str]) -> McpClient {
    let mut cmd = ws.std_cmd();
    cmd.arg("mcp").args(args).env("LODE_AGENT_SESSION", "s-1");
    let transport = TokioChildProcess::new(tokio::process::Command::from(cmd)).unwrap();
    ().serve(transport).await.expect("start lode mcp")
}

async fn names(c: &McpClient) -> Vec<String> {
    c.list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.into_owned())
        .collect()
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

fn files(v: &Value) -> Vec<(String, String)> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_session_over_stdio() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("src/a.txt", b"one\n")])
        .await;
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
    let ws = TestWorkspace::new(&server).with_token(&bot);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    let c = start(&ws, &["--agent-model", "model-x", "--agent-tool", "fixer"]).await;
    // No `write` scope: no commit tool, and calling it anyway fails.
    let list = names(&c).await;
    assert!(list.contains(&"shelve".to_owned()), "{list:?}");
    assert!(!list.contains(&"commit".to_owned()), "{list:?}");
    assert_eq!(
        call_err(&c, "commit", json!({ "message": "x" })).await,
        "permission_denied"
    );

    // status → edit on disk → diff → shelve.
    assert_eq!(files(&call(&c, "status", json!({})).await), vec![]);
    ws.write("src/a.txt", "two\n");
    ws.write("src/b.txt", "new\n");
    call(&c, "add", json!({ "paths": ["src/b.txt"] })).await;
    assert_eq!(
        files(&call(&c, "status", json!({})).await),
        vec![
            ("src/a.txt".to_owned(), "modified".to_owned()),
            ("src/b.txt".to_owned(), "added".to_owned())
        ]
    );
    let d = call(&c, "diff", json!({ "paths": ["src/a.txt"] })).await;
    let patch = d["files"][0]["patch"].as_str().unwrap();
    assert!(patch.contains("-one") && patch.contains("+two"), "{patch}");
    let s = call(&c, "shelve", json!({ "description": "Count higher" })).await;
    let id = s["shelf"].as_u64().unwrap();
    assert_eq!(s["files"], 2);
    // Shelving takes the changes out of the workspace.
    assert_eq!(files(&call(&c, "status", json!({})).await), vec![]);

    // History and content at a revision.
    let log = call(&c, "log", json!({ "limit": 5 })).await;
    assert_eq!(log["revisions"][0]["rev"], r1);
    let f = call(
        &c,
        "read_file_at_revision",
        json!({ "path": "src/a.txt", "rev": r1 }),
    )
    .await;
    assert_eq!(f["content"], "one\n");

    // Paths may not leave the workspace.
    for bad in ["../outside.txt", "/etc/passwd", "src/../../x"] {
        assert_eq!(
            call_err(&c, "add", json!({ "paths": [bad] })).await,
            "outside_workspace",
            "{bad}"
        );
    }
    assert_eq!(
        call_err(&c, "read_file_at_revision", json!({ "path": "../a.txt" })).await,
        "outside_workspace"
    );
    c.cancel().await.unwrap();

    // The shelf is on the server, with the agent's provenance.
    let shelf = server.admin().shelf("game", id, false).await.unwrap();
    assert_eq!(shelf.owner.name, "bot");
    let p = shelf.provenance.unwrap();
    assert_eq!(p.via, Some(lode_proto::commit::Via::Api));
    let agent = p.agent.unwrap();
    assert_eq!(agent.get("model").map(String::as_str), Some("model-x"));
    assert_eq!(agent.get("tool").map(String::as_str), Some("fixer"));
    assert_eq!(agent.get("session").map(String::as_str), Some("s-1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_with_write_scope() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"one\n")])
        .await;
    let ana = server.create_user("ana", &["read", "write"]).await;
    server
        .acl(
            "game",
            "ana",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    let ws = TestWorkspace::new(&server).with_token(&ana);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    let c = start(&ws, &["--agent-model", "model-y"]).await;
    let list = names(&c).await;
    assert_eq!(list.last().map(String::as_str), Some("commit"), "{list:?}");
    ws.write("a.txt", "two\n");
    let done = call(&c, "commit", json!({ "message": "Bump" })).await;
    let rev = done["rev"].as_u64().unwrap();
    c.cancel().await.unwrap();

    let view = server.admin().revision("game", rev).await.unwrap();
    assert_eq!(view.message, "Bump");
    let p = view.provenance.unwrap();
    assert_eq!(
        p.agent.unwrap().get("model").map(String::as_str),
        Some("model-y")
    );
    assert!(p.client.unwrap().starts_with("lode-cli/"));
}
