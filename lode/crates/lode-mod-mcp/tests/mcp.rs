//! T041 acceptance: the server MCP endpoint, driven by the MCP SDK's own client.
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

async fn tool_names(client: &McpClient) -> Vec<String> {
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.into_owned())
        .collect();
    names.sort();
    names
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

/// Calls a tool that must succeed; its structured result.
async fn call(client: &McpClient, name: &str, args: Value) -> Value {
    let r = call_raw(client, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.expect("structured result")
}

/// Calls a tool that must fail; its Lode error code.
async fn call_err(client: &McpClient, name: &str, args: Value) -> String {
    let r = call_raw(client, name, args).await;
    assert_eq!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn lines(n: usize, tag: &str) -> String {
    (1..=n).map(|i| format!("{tag} line {i}\n")).collect()
}

/// A depot with a hidden directory, a large file, a binary file and two revisions.
async fn seeded() -> (TestServer, u64, u64) {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let big = lines(20_000, "big");
    let mid = lines(500, "mid");
    let r1 = server
        .commit_files(
            "game",
            "main",
            &[
                (
                    "src/main.rs",
                    &b"fn main() {\n    println!(\"hi\");\n}\n"[..],
                ),
                ("secret/key.txt", &b"hunter2 key\n"[..]),
                ("big.txt", big.as_bytes()),
                ("img.bin", &[0x89, b'P', b'N', b'G', 0, 1, 2, 3][..]),
                ("mid.txt", mid.as_bytes()),
            ],
        )
        .await;
    let big2 = lines(20_000, "huge");
    let mid2 = lines(500, "middle");
    let r2 = server
        .commit_files(
            "game",
            "main",
            &[
                (
                    "src/main.rs",
                    &b"fn main() {\n    println!(\"hello\");\n}\n"[..],
                ),
                ("secret/key.txt", &b"hunter3 key\n"[..]),
                ("big.txt", big2.as_bytes()),
                ("mid.txt", mid2.as_bytes()),
            ],
        )
        .await;
    (server, r1, r2)
}

#[tokio::test]
async fn read_tools_respect_access() {
    let (server, r1, r2) = seeded().await;
    let ana = server.create_user("ana", &["read"]).await;
    server
        .acl(
            "game",
            "ana",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Read,
        )
        .await;
    server
        .acl(
            "game",
            "ana",
            "*",
            "secret/**",
            AclEffect::Deny,
            AccessLevel::Read,
        )
        .await;
    let c = connect(&server, &ana).await;

    // Core tools plus those the locks and shelves modules put in the registry; no write tools
    // for a read-only token.
    let names = tool_names(&c).await;
    for t in [
        "whoami",
        "list_depots",
        "list_branches",
        "log",
        "show_revision",
        "list_dir",
        "read_file",
        "diff",
        "blame",
        "search_history",
        "list_locks",
        "list_shelves",
        "get_shelf",
    ] {
        assert!(names.contains(&t.to_owned()), "{t} missing from {names:?}");
    }

    let me = call(&c, "whoami", json!({})).await;
    assert_eq!(
        (me["name"].clone(), me["kind"].clone(), me["scopes"].clone()),
        (json!("ana"), json!("user"), json!(["read"]))
    );
    let depots = call(&c, "list_depots", json!({})).await;
    assert_eq!(depots["depots"][0]["name"], "game");
    let branches = call(&c, "list_branches", json!({ "depot": "game" })).await;
    assert_eq!(branches["branches"][0]["name"], "main");
    assert_eq!(branches["branches"][0]["head_rev"], r2);

    let log = call(&c, "log", json!({ "depot": "game", "limit": 1 })).await;
    assert_eq!(log["revisions"][0]["rev"], r2);
    assert_eq!(log["more"], true);
    let older = call(&c, "log", json!({ "depot": "game", "before": r2 })).await;
    assert_eq!(older["revisions"][0]["rev"], r1);

    // The hidden directory does not show anywhere.
    let shown = call(&c, "show_revision", json!({ "depot": "game", "rev": r2 })).await;
    let changed: Vec<&str> = shown["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert!(changed.contains(&"src/main.rs"), "{changed:?}");
    assert!(
        !changed.iter().any(|p| p.starts_with("secret")),
        "{changed:?}"
    );
    let root = call(&c, "list_dir", json!({ "depot": "game" })).await;
    let entries: Vec<&str> = root["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(entries, vec!["big.txt", "img.bin", "mid.txt", "src"]);
    assert_eq!(root["rev"], r2);
    assert_eq!(
        call_err(&c, "list_dir", json!({ "depot": "game", "path": "secret" })).await,
        "not_found"
    );
    assert_eq!(
        call_err(
            &c,
            "read_file",
            json!({ "depot": "game", "path": "secret/key.txt" })
        )
        .await,
        "not_found"
    );
    let found = call(
        &c,
        "search_history",
        json!({ "depot": "game", "query": "KEY" }),
    )
    .await;
    assert_eq!(found["revisions"], json!([]), "{found}");
    let b = call(
        &c,
        "blame",
        json!({ "depot": "game", "path": "src/main.rs", "start_line": 2, "end_line": 3 }),
    )
    .await;
    assert_eq!(b["total_lines"], 3);
    assert_eq!(
        b["lines"],
        json!([
            { "line": 2, "rev": r2, "text": "    println!(\"hello\");" },
            { "line": 3, "rev": r1, "text": "}" }
        ])
    );
    assert_eq!(b["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(b["revisions"][0]["author"]["name"], "admin");
    assert_eq!(
        call_err(&c, "blame", json!({ "depot": "game", "path": "img.bin" })).await,
        "invalid_request"
    );
    assert_eq!(
        call_err(
            &c,
            "blame",
            json!({ "depot": "game", "path": "secret/key.txt" })
        )
        .await,
        "not_found"
    );
    let d = call(&c, "diff", json!({ "depot": "game", "to_rev": r2 })).await;
    assert_eq!(d["from_rev"], r1);
    assert!(!d["diff"].as_str().unwrap().contains("hunter"), "{d}");
    let paths: Vec<&str> = d["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["big.txt", "mid.txt", "src/main.rs"]);
    // Too large to compare, so listed only.
    assert_eq!(d["files"][0]["skipped"], "too_large");

    // Files: whole, by lines, binary, and capped.
    let f = call(
        &c,
        "read_file",
        json!({ "depot": "game", "path": "src/main.rs", "rev": r1 }),
    )
    .await;
    assert_eq!(f["text"], "fn main() {\n    println!(\"hi\");\n}\n");
    assert_eq!(
        (f["truncated"].clone(), f["end_line"].clone()),
        (json!(false), json!(3))
    );
    let f = call(
        &c,
        "read_file",
        json!({ "depot": "game", "path": "big.txt", "start_line": 5, "end_line": 6 }),
    )
    .await;
    assert_eq!(f["text"], "huge line 5\nhuge line 6\n");
    let f = call(
        &c,
        "read_file",
        json!({ "depot": "game", "path": "img.bin" }),
    )
    .await;
    assert_eq!(
        (f["binary"].clone(), f["size"].clone()),
        (json!(true), json!(8))
    );
    assert!(f.get("text").is_none());
    let f = call(
        &c,
        "read_file",
        json!({ "depot": "game", "path": "big.txt" }),
    )
    .await;
    assert_eq!(f["truncated"], true);
    assert_eq!(f["text"].as_str().unwrap().len(), 256 * 1024);
    let next = f["end_line"].as_u64().unwrap() + 1;
    let f = call(
        &c,
        "read_file",
        json!({ "depot": "game", "path": "big.txt", "start_line": next, "max_bytes": 100 }),
    )
    .await;
    assert!(
        f["text"]
            .as_str()
            .unwrap()
            .starts_with(&format!("huge line {next}\n")),
        "{f}"
    );

    // A large diff is cut with the flag set; a small one is not.
    let d = call(
        &c,
        "diff",
        json!({ "depot": "game", "from_rev": r1, "to_rev": r2, "max_bytes": 4096 }),
    )
    .await;
    assert_eq!(d["truncated"], true);
    assert!(d["diff"].as_str().unwrap().len() <= 4096);
    let d = call(
        &c,
        "diff",
        json!({ "depot": "game", "from_rev": r1, "to_rev": r2, "path": "src" }),
    )
    .await;
    assert_eq!(d["truncated"], false);
    let text = d["diff"].as_str().unwrap();
    assert!(
        text.contains("--- a/src/main.rs") && text.contains("+    println!(\"hello\");"),
        "{text}"
    );

    // History search over messages and paths.
    let found = call(
        &c,
        "search_history",
        json!({ "depot": "game", "query": "MAIN.rs" }),
    )
    .await;
    let revs: Vec<u64> = found["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_u64().unwrap())
        .collect();
    assert_eq!(revs, vec![r2, r1]);
    assert_eq!(
        found["revisions"][0]["matched_paths"],
        json!(["src/main.rs"])
    );
    let found = call(
        &c,
        "search_history",
        json!({ "depot": "game", "query": "main", "limit": 1 }),
    )
    .await;
    assert_eq!(found["next_before"], r2);

    // Errors carry the Lode code; unknown depots look like missing ones.
    assert_eq!(
        call_err(&c, "log", json!({ "depot": "nope" })).await,
        "not_found"
    );
    assert_eq!(
        call_err(&c, "log", json!({ "depot": "game", "bogus": 1 })).await,
        "invalid_request"
    );
    assert!(c
        .call_tool(CallToolRequestParams::new("no_such_tool"))
        .await
        .is_err());
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn tokens_and_modules_decide_the_tool_list() {
    let (server, _, _) = seeded().await;

    // No token: the standard challenge.
    let resp = reqwest::Client::new()
        .post(format!("{}/mcp", server.url()))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert!(resp.headers().contains_key("www-authenticate"));

    // A token without `read` sees no read tools and cannot call them.
    let shelver = server.create_user("sam", &["shelve"]).await;
    let c = connect(&server, &shelver).await;
    let names = tool_names(&c).await;
    assert!(!names.contains(&"read_file".to_owned()), "{names:?}");
    assert_eq!(call_err(&c, "whoami", json!({})).await, "permission_denied");
    c.cancel().await.unwrap();

    // With locks and shelves disabled their tools are gone.
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.modules.locks = false;
            c.modules.shelves = false;
        }),
    })
    .await;
    let c = connect(&server, server.admin_token()).await;
    let names = tool_names(&c).await;
    assert!(names.contains(&"read_file".to_owned()), "{names:?}");
    for t in ["list_locks", "list_shelves", "get_shelf", "list_checks"] {
        assert!(!names.contains(&t.to_owned()), "{t} in {names:?}");
    }
    c.cancel().await.unwrap();
}

/// T063: `list_checks` is there only with the checks module, and lists what was reported.
#[tokio::test]
async fn list_checks_comes_with_the_checks_module() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| c.modules.checks = true),
    })
    .await;
    server.create_depot("game").await;
    let rev = server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let ci = server.create_user("ci", &["read", "checks"]).await;
    server
        .acl("game", "ci", "*", "**", AclEffect::Allow, AccessLevel::Read)
        .await;
    server
        .client(&ci)
        .report_check(
            "game",
            &lode_proto::checks::ReportCheckRequest {
                target: lode_proto::checks::CheckTarget {
                    kind: lode_proto::checks::CheckTargetKind::Revision,
                    id: rev,
                    version: None,
                },
                name: "ci/build".into(),
                state: lode_proto::checks::CheckState::Success,
                summary: Some("ok".into()),
                url: None,
            },
        )
        .await
        .unwrap();
    let c = connect(&server, server.admin_token()).await;
    assert!(tool_names(&c).await.contains(&"list_checks".to_owned()));
    let list = call(
        &c,
        "list_checks",
        json!({ "depot": "game", "target_kind": "revision", "target_id": rev }),
    )
    .await;
    assert_eq!(list["checks"][0]["name"], "ci/build");
    assert_eq!(list["checks"][0]["state"], "success");
    assert_eq!(list["checks"][0]["reporter"]["name"], "ci");
    assert_eq!(
        call_err(
            &c,
            "list_checks",
            json!({ "depot": "game", "target_kind": "shelf", "target_id": 1 })
        )
        .await,
        "invalid_request"
    );
    c.cancel().await.unwrap();
}
