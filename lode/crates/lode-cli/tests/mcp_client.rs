//! T190/T191 acceptance: `lode mcp` as a desktop client — remote status, branches, locks,
//! conflicts, switch, merge, resolve and changelists, with the result shelved for a person.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::depot::CreateBranchRequest;
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

/// The `--json` answer of a command without the `ok` envelope field, as a tool answers it.
fn cli(ws: &TestWorkspace, args: &[&str]) -> Value {
    let mut v = ws.json(args);
    v.as_object_mut().unwrap().remove("ok");
    v
}

/// A workspace of the server admin, the person reviewing the agent's work.
fn person(server: &TestServer) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

fn make_writable(ws: &TestWorkspace, path: &str) {
    let p = ws.root().join(path);
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
}

const CONFIG: &[u8] = b"[locks]\nrequired = [\"**/*.psd\"]\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sees_what_a_person_sees() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeconfig", CONFIG),
                ("a.txt", b"one\n"),
                ("art/door.psd", b"door 1"),
                ("art/wall.psd", b"wall 1"),
            ],
        )
        .await;
    let ws = agent_workspace(&server).await;
    // Another person locks one file, and a newer version of another lands on the server.
    let lea = server.create_user("lea", &["read", "write", "lock"]).await;
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
    let other = TestWorkspace::new(&server).with_token(&lea);
    other.json(&["checkout", &format!("{}/game", server.url()), "."]);
    other.json(&["lock", "art/wall.psd"]);
    let head = server
        .commit_files("game", "main", &[("a.txt", b"two\n")])
        .await;
    // The agent changes a file that needs a lock without taking it.
    make_writable(&ws, "art/door.psd");
    ws.write("art/door.psd", "door 2");

    let c = start(&ws).await;
    let local = call(&c, "status", json!({})).await;
    assert_eq!(local["summary"]["changed"], 1, "{local}");
    assert_eq!(local["summary"]["needs_lock"], 1, "{local}");
    assert_eq!(local["summary"]["pending_merge"], false);
    assert!(local.get("remote").is_none());
    assert!(local["summary"].get("head_rev").is_none());

    let s = call(&c, "status", json!({ "remote": true })).await;
    assert_eq!(s["summary"]["head_rev"], head, "{s}");
    assert_eq!(s["summary"]["newer_at_head"], 1, "{s}");
    assert_eq!(s["summary"]["locked_by_others"], 1, "{s}");
    let remote = s["remote"]["files"].as_array().unwrap();
    let wall = remote.iter().find(|f| f["path"] == "art/wall.psd").unwrap();
    assert_eq!(wall["locked_by"]["name"], "lea");
    assert!(remote
        .iter()
        .any(|f| f["path"] == "a.txt" && f["newer"] == true));

    // Branches and locks answer what the command line answers.
    let branches = call(&c, "branches", json!({})).await;
    assert_eq!(branches, cli(&ws, &["branch", "list"]));
    assert_eq!(branches["current"], "main");
    let locks = call(&c, "locks", json!({})).await;
    assert_eq!(locks, cli(&ws, &["locks"]));
    let mine = call(&c, "locks", json!({ "mine": true })).await;
    assert_eq!(mine["locks"].as_array().unwrap().len(), 0);
    let under = call(&c, "locks", json!({ "paths": ["art"] })).await;
    assert_eq!(under["locks"].as_array().unwrap().len(), 1);

    // No conflicts yet; paths stay inside the workspace.
    let none = call(&c, "conflicts", json!({})).await;
    assert_eq!(none["conflicts"], json!([]));
    assert_eq!(
        call_err(&c, "locks", json!({ "paths": ["../x"] })).await,
        "outside_workspace"
    );
    assert_eq!(
        call_err(&c, "conflicts", json!({ "path": "/etc/passwd" })).await,
        "outside_workspace"
    );
    c.cancel().await.unwrap();
}

const BIN_BASE: &[u8] = b"\x00\x01base\xff";
const BIN_FEAT: &[u8] = b"\x00\x01feat\xff";
const BIN_MAIN: &[u8] = b"\x00\x01main\xff";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merges_resolves_and_shelves_for_review() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[("a.txt", b"1\n2\n3\n"), ("art.bin", BIN_BASE)],
        )
        .await;
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: "feat".into(),
                from_branch: "main".into(),
                from_rev: None,
                stream: None,
            },
        )
        .await
        .unwrap();
    let feat_head = server
        .commit_files(
            "game",
            "feat",
            &[("a.txt", b"1\nfeat\n3\n"), ("art.bin", BIN_FEAT)],
        )
        .await;
    server
        .commit_files(
            "game",
            "main",
            &[("a.txt", b"1\nmain\n3\n"), ("art.bin", BIN_MAIN)],
        )
        .await;
    let ws = agent_workspace(&server).await;
    let c = start(&ws).await;

    // Switch away and back.
    let sw = call(&c, "switch", json!({ "branch": "feat" })).await;
    assert_eq!(sw["branch"], "feat", "{sw}");
    assert_eq!(ws.read("a.txt"), b"1\nfeat\n3\n");
    call(&c, "switch", json!({ "branch": "main" })).await;
    assert_eq!(ws.read("a.txt"), b"1\nmain\n3\n");

    // Merge with one text and one binary conflict.
    let m = call(&c, "merge", json!({ "branch": "feat" })).await;
    assert_eq!(m["counts"]["conflicted"], 2, "{m}");
    let list = call(&c, "conflicts", json!({})).await;
    let list = list["conflicts"].as_array().unwrap().clone();
    assert_eq!(list.len(), 2, "{list:?}");
    let text_one = list.iter().find(|x| x["path"] == "a.txt").unwrap();
    assert_eq!(text_one["text"], true);
    assert_eq!(text_one["theirs_rev"], feat_head);
    let bin_one = list.iter().find(|x| x["path"] == "art.bin").unwrap();
    assert_eq!(bin_one["text"], false);

    let sides = call(&c, "conflicts", json!({ "path": "a.txt" })).await;
    assert_eq!(sides["base"]["text"], "1\n2\n3\n", "{sides}");
    assert_eq!(sides["mine"]["text"], "1\nmain\n3\n");
    assert_eq!(sides["theirs"]["text"], "1\nfeat\n3\n");
    assert!(sides["result"]["text"]
        .as_str()
        .unwrap()
        .contains("<<<<<<<"));
    let cut = call(&c, "conflicts", json!({ "path": "a.txt", "max_bytes": 3 })).await;
    assert_eq!(cut["base"]["cut"], true, "{cut}");
    assert!(cut["base"].get("text").is_none());
    let bin = call(&c, "conflicts", json!({ "path": "art.bin" })).await;
    assert_eq!(bin["theirs"]["binary"], true, "{bin}");
    assert_eq!(bin["theirs"]["size"], BIN_FEAT.len());
    assert!(bin["theirs"].get("text").is_none());

    // Bad resolve input is refused before anything changes.
    assert_eq!(
        call_err(
            &c,
            "resolve",
            json!({ "path": "a.txt", "take": "mine", "text": "x" })
        )
        .await,
        "usage"
    );
    assert_eq!(
        call_err(&c, "resolve", json!({ "path": "a.txt" })).await,
        "usage"
    );
    call(
        &c,
        "resolve",
        json!({ "path": "a.txt", "text": "1\nmain and feat\n3\n" }),
    )
    .await;
    call(
        &c,
        "resolve",
        json!({ "path": "art.bin", "take": "theirs" }),
    )
    .await;
    assert_eq!(ws.read("a.txt"), b"1\nmain and feat\n3\n");
    assert_eq!(ws.read("art.bin"), BIN_FEAT);
    let s = call(&c, "status", json!({})).await;
    assert_eq!(s["summary"]["conflicted"], 0, "{s}");
    assert_eq!(s["summary"]["pending_merge"], true);

    // A changelist groups part of the work; the merge is shelved for review.
    call(
        &c,
        "changelist",
        json!({ "action": "new", "name": "art", "description": "Art from feat" }),
    )
    .await;
    let moved = call(
        &c,
        "changelist",
        json!({ "action": "move", "name": "art", "paths": ["art.bin"] }),
    )
    .await;
    assert_eq!(moved["moved"], json!(["art.bin"]), "{moved}");
    let lists = call(&c, "changelist", json!({ "action": "list" })).await;
    assert!(lists["changelists"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["name"] == "art"));
    assert_eq!(
        call_err(&c, "changelist", json!({ "action": "rename" })).await,
        "usage"
    );
    call(
        &c,
        "changelist",
        json!({ "action": "delete", "name": "art" }),
    )
    .await;
    let shelf = call(
        &c,
        "shelve",
        json!({ "description": "Merge feat: a.txt took both lines, art.bin took feat's" }),
    )
    .await;
    let id = shelf["shelf"].as_u64().unwrap();
    c.cancel().await.unwrap();

    // The person submits the version they reviewed; the merge is recorded.
    let review = person(&server);
    let done = review.json(&["shelf", "submit", &format!("{id}@1")]);
    let rev = done["rev"].as_u64().unwrap();
    let view = server.admin().revision("game", rev).await.unwrap();
    assert_eq!(view.author.name, "bot");
    assert_eq!(view.submitter.name, "admin");
    assert_eq!(view.merged_from.len(), 1, "{:?}", view.merged_from);
    assert_eq!(view.merged_from[0].branch, "feat");
    assert_eq!(view.merged_from[0].to_rev, feat_head);
}
