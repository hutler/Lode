//! T043 acceptance: an agent proposes changes through MCP without a workspace; a person
//! reviews them with the CLI and submits them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
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

async fn call_err(client: &McpClient, name: &str, args: Value) -> String {
    let r = call_raw(client, name, args).await;
    assert_eq!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
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

#[tokio::test]
async fn published_schemas_and_input_errors_identify_the_edit() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let client = connect(&server, server.admin_token()).await;
    let tools = client.list_all_tools().await.unwrap();
    for (name, required) in [
        (
            "create_shelf",
            json!(["depot", "branch", "description", "edits"]),
        ),
        ("update_shelf", json!(["depot", "id", "edits"])),
    ] {
        let schema = &tools.iter().find(|t| t.name == name).unwrap().input_schema;
        assert_eq!(schema["required"], required);
        let item = &schema["properties"]["edits"]["items"];
        assert_eq!(item["required"], json!(["path", "action"]));
        assert_eq!(
            item["properties"]["action"]["enum"],
            json!(["add", "modify", "delete"])
        );
        assert_eq!(
            item["properties"]["copy_from"]["required"],
            json!(["path", "rev"])
        );

        for (edits, expected) in [
            (
                json!([{"path":"a"}]),
                r#"edits[0]: missing required field "action" (one of: add, modify, delete)"#,
            ),
            (
                json!([{"path":"a","action":"delete"},{"path":"b","action":"rename"}]),
                r#"edits[1]: invalid field "action" (one of: add, modify, delete)"#,
            ),
            (
                json!([{"path":"a","action":"add","copy_from":{"path":"b"}}]),
                "edits[0].copy_from: missing field `rev`",
            ),
        ] {
            let mut args = if name == "create_shelf" {
                json!({"depot":"game","branch":"main","description":"test"})
            } else {
                json!({"depot":"game","id":1})
            };
            args["edits"] = edits;
            let result = call_raw(&client, name, args).await;
            assert_eq!(result.is_error, Some(true));
            let error = result.structured_content.unwrap();
            assert_eq!(error["code"], "invalid_request");
            assert_eq!(error["message"], expected);
        }
    }
    let checks = &tools
        .iter()
        .find(|t| t.name == "list_checks")
        .unwrap()
        .input_schema;
    assert_eq!(checks["if"]["properties"]["target_kind"]["const"], "shelf");
    assert_eq!(checks["then"]["required"], json!(["target_version"]));
    let result = call_raw(
        &client,
        "list_checks",
        json!({"depot":"game","target_id":1}),
    )
    .await;
    let error = result.structured_content.unwrap();
    assert_eq!(error["code"], "invalid_request");
    assert_eq!(error["message"], "target_kind must be revision or shelf");
    let result = call_raw(
        &client,
        "wait_for_events",
        json!({"depot":"game","kinds":[42]}),
    )
    .await;
    let error = result.structured_content.unwrap();
    assert_eq!(error["code"], "invalid_request");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with("kinds[0] must be one of: "),
        "{message}"
    );
    assert!(message.contains(lode_proto::feed::EVENTS[0]));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn http_shelf_and_commit_errors_identify_the_change() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let http = reqwest::Client::new();
    for (route, mut body) in [
        (
            "shelves",
            json!({"branch":"main","base_rev":1,"description":"test"}),
        ),
        (
            "branches/main/commits",
            json!({"request_id":"test","message":"test"}),
        ),
    ] {
        for (change, reason) in [
            (json!({"path":"b"}), "missing required field"),
            (json!({"path":"b","action":"rename"}), "invalid field"),
        ] {
            body["changes"] = json!([{"path":"a","action":"delete"}, change]);
            let response = http
                .post(format!("{}/api/v1/depots/game/{route}", server.url()))
                .bearer_auth(server.admin_token())
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            let error: Value = response.json().await.unwrap();
            assert_eq!(error["code"], "invalid_request");
            assert_eq!(
                error["message"],
                format!(r#"changes[1]: {reason} "action" (one of: add, modify, delete)"#)
            );
        }
    }
}

// The CLI runs synchronously, so the server needs threads of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_shelf_reviewed_and_submitted_by_a_person() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.limits.mcp_max_edits = 3;
            c.limits.mcp_max_inline_bytes = 1024;
        }),
    })
    .await;
    server.create_depot("game").await;
    let base = server
        .commit_files(
            "game",
            "main",
            &[
                ("src/a.txt", &b"one\n"[..]),
                ("src/run.sh", &b"echo hi\n"[..]),
                ("secret/key.txt", &b"k\n"[..]),
            ],
        )
        .await;
    let ana = server
        .create_user("ana", &["read", "write", "shelve"])
        .await;
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
    let bot = server.create_agent("bot", "ana", &["read", "shelve"]).await;
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
    server
        .acl(
            "game",
            "bot",
            "*",
            "secret/**",
            AclEffect::Deny,
            AccessLevel::Read,
        )
        .await;

    // The agent may shelve but not submit.
    let agent = connect(&server, &bot).await;
    let names = tool_names(&agent).await;
    assert!(names.contains(&"create_shelf".to_owned()), "{names:?}");
    assert!(names.contains(&"update_shelf".to_owned()), "{names:?}");
    assert!(!names.contains(&"submit_shelf".to_owned()), "{names:?}");

    // It reads a file and proposes a change based on what it read.
    let f = call(
        &agent,
        "read_file",
        json!({ "depot": "game", "path": "src/a.txt" }),
    )
    .await;
    assert_eq!(f["text"], "one\n");
    let shelf = call(
        &agent,
        "create_shelf",
        json!({
            "depot": "game", "branch": "main", "base_rev": f["rev"],
            "description": "Count higher",
            "edits": [
                { "path": "src/a.txt", "action": "modify", "text": "two\n" },
                { "path": "docs/new.bin", "action": "add", "base64": "AAEC" },
                { "path": "src/copy.sh", "action": "add",
                  "copy_from": { "path": "src/run.sh", "rev": base } }
            ],
            "agent": { "model": "model-x", "session": "s-7" }
        }),
    )
    .await;
    let id = shelf["id"].as_u64().unwrap();
    assert_eq!(shelf["version"], 1);
    assert_eq!(shelf["owner"]["name"], "bot");
    assert_eq!(
        shelf["provenance"],
        json!({ "via": "mcp", "agent": { "model": "model-x", "session": "s-7" } })
    );
    let changes = shelf["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 3);
    let a = changes.iter().find(|c| c["path"] == "src/a.txt").unwrap();
    assert!(a["base_id"].is_string(), "base_id derived: {a}");
    let copy = changes.iter().find(|c| c["path"] == "src/copy.sh").unwrap();
    assert_eq!(copy["copy_from"]["path"], "src/run.sh");

    // Refusals: a path it cannot read, too many edits, too much content, a bad action.
    let hidden = json!({
        "depot": "game", "branch": "main", "description": "x",
        "edits": [{ "path": "secret/key.txt", "action": "modify", "text": "x\n" }]
    });
    assert_eq!(call_err(&agent, "create_shelf", hidden).await, "not_found");
    let edit = json!({ "path": "src/a.txt", "action": "modify", "text": "x\n" });
    let many = json!({
        "depot": "game", "branch": "main", "description": "x",
        "edits": [edit.clone(), edit.clone(), edit.clone(), edit]
    });
    assert_eq!(call_err(&agent, "create_shelf", many).await, "too_large");
    let big = json!({
        "depot": "game", "branch": "main", "description": "x",
        "edits": [{ "path": "src/a.txt", "action": "modify", "text": "x".repeat(2000) }]
    });
    assert_eq!(call_err(&agent, "create_shelf", big).await, "too_large");
    let bad = json!({
        "depot": "game", "branch": "main", "description": "x",
        "edits": [{ "path": "src/a.txt", "action": "rename" }]
    });
    assert_eq!(
        call_err(&agent, "create_shelf", bad).await,
        "invalid_request"
    );

    // The agent revises its shelf; nobody else may.
    let shelf = call(
        &agent,
        "update_shelf",
        json!({
            "depot": "game", "id": id,
            "edits": [
                { "path": "src/a.txt", "action": "modify", "text": "three\n" },
                { "path": "docs/new.bin", "action": "add", "base64": "AAEC" }
            ]
        }),
    )
    .await;
    assert_eq!(shelf["version"], 2);
    assert_eq!(shelf["description"], "Count higher");
    let human = connect(&server, &ana).await;
    assert_eq!(
        call_err(
            &human,
            "update_shelf",
            json!({ "depot": "game", "id": id, "edits": [] })
        )
        .await,
        "permission_denied"
    );
    agent.cancel().await.unwrap();

    // A person reviews it with the CLI and submits the version they reviewed.
    let ws = TestWorkspace::new(&server).with_token(&ana);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    let diff = ws.json(&["shelf", "diff", &id.to_string()]);
    let patches = diff["diff"].as_array().unwrap();
    let patch = patches.iter().find(|p| p["path"] == "src/a.txt").unwrap()["patch"]
        .as_str()
        .unwrap();
    assert!(
        patch.contains("-one") && patch.contains("+three"),
        "{patch}"
    );
    assert_eq!(
        call_err(
            &human,
            "submit_shelf",
            json!({ "depot": "game", "id": id, "expected_version": 1 })
        )
        .await,
        "conflict"
    );
    let done = call(
        &human,
        "submit_shelf",
        json!({ "depot": "game", "id": id, "expected_version": 2 }),
    )
    .await;
    let rev = done["rev"].as_u64().unwrap();
    let view = call(
        &human,
        "show_revision",
        json!({ "depot": "game", "rev": rev }),
    )
    .await;
    assert_eq!(
        view["author"],
        json!({ "name": "bot", "kind": "agent", "owner": "ana" })
    );
    assert_eq!(view["submitter"]["name"], "ana");
    assert_eq!(view["message"], "Count higher");
    assert_eq!(view["provenance"]["via"], "mcp");
    assert_eq!(view["provenance"]["submitted_shelf"], id);
    assert_eq!(view["provenance"]["agent"]["model"], "model-x");
    let f = call(
        &human,
        "read_file",
        json!({ "depot": "game", "path": "src/a.txt" }),
    )
    .await;
    assert_eq!(f["text"], "three\n");
    human.cancel().await.unwrap();
}
