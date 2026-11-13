//! T056 acceptance: one end-to-end scenario for each section of SPEC §5–7, through the real
//! `lode` binary against a real server. The detailed behaviour of each feature has its own
//! tests; these check that the sections hold together as the SPEC describes them.
//!
//! §6.5 (webhooks) and §6.6 (checks) are not in this build: their modules are scaffolds until
//! M6. `docs/SPEC.md` lists them under "Deviations in 0.1".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;

use lode_core::{blob_id, Change, ChangeAction, DepotPath, EntryKind};
use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::commit::CommitRequest;
use lode_testkit::{TestServer, TestWorkspace};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------------------
// Helpers

async fn server() -> TestServer {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
}

fn url(server: &TestServer) -> String {
    format!("{}/game", server.url())
}

/// A user with `scopes` and an allow rule at `level` on the whole depot; returns the token.
async fn member(server: &TestServer, name: &str, scopes: &[&str], level: AccessLevel) -> String {
    let token = server.create_user(name, scopes).await;
    server
        .acl("game", name, "*", "**", AclEffect::Allow, level)
        .await;
    token
}

fn checkout_as(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &url(server), "."]);
    ws
}

fn admin_ws(server: &TestServer) -> TestWorkspace {
    checkout_as(server, server.admin_token())
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

/// `lode --json args…`, which must fail; returns the error code.
fn fails(ws: &TestWorkspace, args: &[&str]) -> String {
    let (status, v) = ws.json_status(args);
    assert_ne!(status, 0, "lode {args:?} succeeded: {v}");
    code(&v).to_owned()
}

/// Changed files and their status.
fn changes(ws: &TestWorkspace) -> Vec<(String, String)> {
    ws.json(&["status"])["files"]
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

fn text(ws: &TestWorkspace, path: &str) -> String {
    String::from_utf8(ws.read(path)).unwrap()
}

fn last_revision(ws: &TestWorkspace) -> Value {
    ws.json(&["log", "-v", "-n", "1"])["revisions"][0].clone()
}

fn add_change(path: &str, content: &[u8]) -> Change {
    Change {
        path: DepotPath::parse(path).unwrap(),
        action: ChangeAction::Add,
        base_id: None,
        new_id: Some(blob_id(content).unwrap()),
        kind: Some(EntryKind::File),
        executable: false,
        size: Some(content.len() as u64),
        copy_from: None,
    }
}

fn request(id: &str, changes: Vec<Change>) -> CommitRequest {
    CommitRequest {
        request_id: id.into(),
        message: id.into(),
        changes,
        merged_from: vec![],
        provenance: None,
        extensions: Default::default(),
    }
}

type McpClient = RunningService<RoleClient, ()>;

async fn mcp_call(c: &McpClient, name: &str, args: Value) -> CallToolResult {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    c.call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn mcp_ok(c: &McpClient, name: &str, args: Value) -> Value {
    let r = mcp_call(c, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", r.structured_content);
    r.structured_content.expect("structured result")
}

async fn mcp_tools(c: &McpClient) -> Vec<String> {
    c.list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.into_owned())
        .collect()
}

async fn server_mcp(server: &TestServer, token: &str) -> McpClient {
    let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", server.url()))
        .auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("connect to the server MCP endpoint")
}

async fn local_mcp(ws: &TestWorkspace, args: &[&str]) -> McpClient {
    let mut cmd = ws.std_cmd();
    cmd.arg("mcp").args(args);
    let transport = TokioChildProcess::new(tokio::process::Command::from(cmd)).unwrap();
    ().serve(transport).await.expect("start lode mcp")
}

// ---------------------------------------------------------------------------------------
// §5 Core features

/// §5.1: atomic commits, per-file optimistic concurrency, what a revision records, retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_1_commits() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("x.txt", b"x\n"), ("y.txt", b"y\n")])
        .await;
    let a = admin_ws(&server);
    let b = admin_ws(&server);

    // Unrelated commits do not block each other.
    a.write("x.txt", "x by a\n");
    a.json(&["commit", "-m", "a"]);
    b.write("y.txt", "y by b\n");
    b.json(&["commit", "-m", "b"]);
    // A file changed since b last synced it is out of date, and nothing is committed.
    let head = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;
    b.write("x.txt", "x by b\n");
    b.write("y.txt", "y again\n");
    assert_eq!(fails(&b, &["commit", "-m", "stale"]), "out_of_date");
    assert_eq!(
        server
            .admin()
            .branch("game", "main")
            .await
            .unwrap()
            .head_rev,
        head
    );

    // A revision records author, submitter, time, message, client, and copy origins.
    a.json(&["cp", "x.txt", "x-copy.txt"]);
    let c = a.json(&["commit", "-m", "copy"]);
    let r = last_revision(&a);
    assert_eq!(r["rev"], c["rev"]);
    assert_eq!(r["author"]["name"], "admin");
    assert_eq!(r["submitter"]["name"], "admin");
    assert_eq!(r["message"], "copy");
    assert!(r["created_at"].is_string());
    assert!(
        r["provenance"]["client"]
            .as_str()
            .unwrap()
            .starts_with("lode-cli/"),
        "{r}"
    );
    let copy = &r["changes"][0];
    assert_eq!(copy["path"], "x-copy.txt");
    assert_eq!(copy["copy_from"]["path"], "x.txt");

    // The same request id twice: one revision.
    let content = b"retried";
    let id = blob_id(&content[..]).unwrap();
    server
        .admin()
        .put_blob("game", &id, Cursor::new(content.to_vec()))
        .await
        .unwrap();
    let req = request("retry-1", vec![add_change("retry.txt", content)]);
    let first = server.admin().commit("game", "main", &req).await.unwrap();
    let again = server.admin().commit("game", "main", &req).await.unwrap();
    assert_eq!((again.rev, again.replayed), (first.rev, true));
}

/// §5.2: scanning, explicit add/mv/cp, revert and clean, three-way update, views.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_2_workspaces() {
    let server = server().await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("src/a.txt", b"1\n2\n3\n"),
                ("art/raw/big.dat", b"\x00raw"),
                ("img.bin", b"\x00\x01"),
                ("blob.bin", b"\x00\x01"),
            ],
        )
        .await;
    let ws = admin_ws(&server);
    let other = admin_ws(&server);

    // No "open for edit": an edited file shows up by itself; a new one only once added.
    ws.write("src/a.txt", "one\n2\n3\n");
    ws.write("new.txt", "new\n");
    let st = changes(&ws);
    assert!(
        st.contains(&("src/a.txt".into(), "modified".into())),
        "{st:?}"
    );
    assert!(
        st.contains(&("new.txt".into(), "untracked".into())),
        "{st:?}"
    );
    ws.json(&["add", "new.txt"]);
    ws.json(&["mv", "img.bin", "pics/img.bin"]);

    // Revert of an added file leaves it untracked; clean removes it.
    ws.json(&["revert", "new.txt"]);
    assert!(ws.exists("new.txt"));
    let cleaned = ws.json(&["clean"]);
    assert_eq!(cleaned["removed"], json!(["new.txt"]));

    // Update merges text changed on both sides; binary on both sides is a conflict.
    other.write("src/a.txt", "1\n2\nthree\n");
    other.write("blob.bin", "\x00\x02");
    other.json(&["commit", "-m", "other"]);
    ws.write("blob.bin", "\x00\x03");
    let up = ws.json(&["update"]);
    assert_eq!(text(&ws, "src/a.txt"), "one\n2\nthree\n");
    assert_eq!(up["counts"]["merged"], 1, "{up}");
    assert_eq!(up["counts"]["conflicted"], 1, "{up}");
    ws.json(&["resolve", "blob.bin", "--mine"]);
    ws.json(&["commit", "-m", "mine"]);
    // History follows the move.
    let log = ws.json(&["log", "--follow", "pics/img.bin"]);
    assert!(log["revisions"].as_array().unwrap().len() >= 2, "{log}");

    // A view leaves files out and brings them back.
    ws.json(&["view", "exclude", "art/raw/**"]);
    assert!(!ws.exists("art/raw/big.dat"));
    ws.json(&["view", "exclude", "--remove", "art/raw/**"]);
    assert!(ws.exists("art/raw/big.dat"));
    assert!(changes(&ws).is_empty());
}

/// §5.3: cheap branches from a revision, switching, archiving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_3_branches() {
    let server = server().await;
    let r1 = server
        .commit_files("game", "main", &[("a.txt", b"one\n")])
        .await;
    server
        .commit_files("game", "main", &[("a.txt", b"two\n")])
        .await;
    let ws = admin_ws(&server);
    let b = ws.json(&["branch", "create", "old", "--from", &format!("main@{r1}")]);
    assert_eq!(b["parent"], json!({ "branch": "main", "rev": r1 }));
    let sw = ws.json(&["switch", "old"]);
    assert_eq!(sw["branch"], "old");
    assert_eq!(text(&ws, "a.txt"), "one\n");
    ws.write("a.txt", "on old\n");
    ws.json(&["commit", "-m", "on old"]);
    ws.json(&["switch", "main"]);
    assert_eq!(text(&ws, "a.txt"), "two\n");

    ws.json(&["branch", "archive", "old"]);
    let list = ws.json(&["branch", "list"]);
    let names: Vec<&str> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["main"]);
    // Archived, not deleted: still readable, closed to commits.
    let all = ws.json(&["branch", "list", "--all"]);
    assert_eq!(all["branches"].as_array().unwrap().len(), 2);
    let cat = ws.json(&["cat", "-b", "old", "a.txt"]);
    assert_eq!(cat["content"], "on old\n");
    let old = checkout_branch(&server, "old");
    old.write("a.txt", "late\n");
    assert_eq!(fails(&old, &["commit", "-m", "late"]), "branch_archived");
}

fn checkout_branch(server: &TestServer, branch: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &url(server), ".", "-b", branch]);
    ws
}

/// §5.4: merge records `merged_from`; the next merge starts after it; `-r` picks one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_4_merge() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let ws = admin_ws(&server);
    let created = ws.json(&["branch", "create", "feat"])["created_rev"]
        .as_u64()
        .unwrap();
    server
        .commit_files("game", "feat", &[("f1.txt", b"1\n")])
        .await;
    let f2 = server
        .commit_files("game", "feat", &[("f2.txt", b"2\n")])
        .await;

    let pick = ws.json(&["merge", "feat", "-r", &f2.to_string()]);
    assert_eq!(
        (pick["from_rev"].as_u64(), pick["to_rev"].as_u64()),
        (Some(f2), Some(f2))
    );
    ws.json(&["revert", "--all"]);
    ws.json(&["clean"]);

    // Everything since the branch was made.
    let m = ws.json(&["merge", "feat"]);
    let f1 = created;
    assert_eq!(
        (m["from_rev"].as_u64(), m["to_rev"].as_u64()),
        (Some(f1), Some(f2))
    );
    ws.json(&["commit", "-m", "merge feat"]);
    let r = last_revision(&ws);
    assert_eq!(
        r["merged_from"],
        json!([{ "branch": "feat", "from_rev": f1, "to_rev": f2 }])
    );
    assert_eq!(ws.json(&["merge", "feat"])["nothing_to_do"], true);
}

/// §5.5: password sign-in, scopes before rules, default no access, deny beats allow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_5_identity_and_permissions() {
    let server = server().await;
    server
        .commit_files(
            "game",
            "main",
            &[("open.txt", b"open\n"), ("secret/key.txt", b"k\n")],
        )
        .await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());
    let out = admin
        .cmd()
        .args([
            "--json",
            "admin",
            "user",
            "create",
            "ana",
            "--password-stdin",
        ])
        .write_stdin("password-ana\n")
        .output()
        .unwrap();
    assert!(out.status.success());

    // Signed in with a password, but no rule yet: nothing is visible.
    let ana = TestWorkspace::new(&server);
    let out = ana
        .cmd()
        .args([
            "--json",
            "login",
            server.url(),
            "--name",
            "ana",
            "--password-stdin",
        ])
        .write_stdin("password-ana\n")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(ana.json(&["whoami"])["name"], "ana");
    let (status, _) = ana.json_status(&["checkout", &url(&server), "."]);
    assert_ne!(status, 0);

    // Read everywhere but secret/.
    admin.json(&[
        "admin", "acl", "add", "allow", "write", "ana", "--depot", "game",
    ]);
    admin.json(&[
        "admin",
        "acl",
        "add",
        "deny",
        "read",
        "ana",
        "--depot",
        "game",
        "--path",
        "secret/**",
    ]);
    ana.json(&["checkout", &url(&server), "."]);
    assert!(ana.exists("open.txt") && !ana.exists("secret/key.txt"));

    // A read-only token cannot commit, whatever the rules allow.
    let ro = server.token_for("ana", &["read"]).await;
    let ro_ws = checkout_as(&server, &ro);
    ro_ws.write("open.txt", "changed\n");
    assert_eq!(fails(&ro_ws, &["commit", "-m", "x"]), "permission_denied");
    // The full sign-in can.
    ana.write("open.txt", "changed\n");
    ana.json(&["commit", "-m", "by ana"]);
}

/// §5.6: `.lodeignore` and `.lodeconfig`, from the branch head; a broken config is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_6_versioned_configuration() {
    let server = server().await;
    let config = b"[locks]\nrequired = [\"**/*.psd\"]\n\n[files]\nbinary = [\"**/*.dat\"]\n";
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeignore", b"*.tmp\n"),
                (".lodeconfig", config),
                ("door.psd", b"psd"),
                ("level.dat", b"text-like but binary\n"),
            ],
        )
        .await;
    let ws = admin_ws(&server);
    ws.write("scratch.tmp", "x");
    assert!(changes(&ws).is_empty(), "ignored files stay out of status");

    // Lock-required files arrive read-only and need a lock to be committed.
    assert!(std::fs::metadata(ws.root().join("door.psd"))
        .unwrap()
        .permissions()
        .readonly());

    // [files].binary: both sides changing level.dat is a conflict, not a text merge.
    let other = admin_ws(&server);
    other.write("level.dat", "theirs\n");
    other.json(&["commit", "-m", "theirs"]);
    ws.write("level.dat", "mine\n");
    let up = ws.json(&["update"]);
    assert_eq!(up["counts"]["conflicted"], 1, "{up}");
    assert!(ws.exists(&format!("level.dat.theirs-r{}", up["rev"])));
    ws.json(&["resolve", "level.dat", "--theirs"]);

    // A commit that would leave .lodeconfig unparseable is refused.
    ws.write(".lodeconfig", "[locks\nrequired = 1\n");
    assert_eq!(
        fails(&ws, &["commit", "-m", "broken", ".lodeconfig"]),
        "invalid_request"
    );
}

/// §5.7: names that cannot be checked out everywhere are refused, by the client and the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5_7_path_rules() {
    for bad in [
        "a//b",
        "./a",
        "a/../b",
        "a\\b",
        "a<b",
        "a:b",
        "a|b",
        "a?b",
        "a*b",
        "a\"b",
        "dot.",
        "space ",
        "CON",
        "nul.txt",
        "COM1",
        "LPT9.log",
        "COM\u{b9}",
        "tab\there",
    ] {
        assert!(DepotPath::parse(bad).is_err(), "{bad:?} accepted");
    }
    // NFC: the decomposed and composed spellings are one path.
    let composed = DepotPath::parse("caf\u{e9}.txt").unwrap();
    let decomposed = DepotPath::parse("cafe\u{301}.txt").unwrap();
    assert_eq!(composed, decomposed);

    // The server refuses a name that differs from an existing one only by case.
    let server = server().await;
    server
        .commit_files("game", "main", &[("README.md", b"r\n")])
        .await;
    let content = b"other readme";
    let id = blob_id(&content[..]).unwrap();
    server
        .admin()
        .put_blob("game", &id, Cursor::new(content.to_vec()))
        .await
        .unwrap();
    let err = server
        .admin()
        .commit(
            "game",
            "main",
            &request("case", vec![add_change("Readme.md", content)]),
        )
        .await
        .unwrap_err();
    assert!(err.is_code(lode_core::ErrorCode::InvalidPath), "{err:?}");
}

// ---------------------------------------------------------------------------------------
// §6 Default modules

/// §6.1: exclusive, all-or-nothing locks; commits respect them; release on commit; force.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s6_1_locks() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("a.psd", b"a"), ("b.psd", b"b")])
        .await;
    let ana = member(
        &server,
        "ana",
        &["read", "write", "lock"],
        AccessLevel::Write,
    )
    .await;
    let bob = member(
        &server,
        "bob",
        &["read", "write", "lock"],
        AccessLevel::Write,
    )
    .await;
    let wa = checkout_as(&server, &ana);
    let wb = checkout_as(&server, &bob);

    wa.json(&["lock", "a.psd", "-m", "painting"]);
    // All or nothing: b.psd is not taken because a.psd is held.
    assert_eq!(fails(&wb, &["lock", "b.psd", "a.psd"]), "locked");
    assert_eq!(wb.json(&["locks"])["locks"].as_array().unwrap().len(), 1);
    wb.write("a.psd", "bob's");
    assert_eq!(fails(&wb, &["commit", "-m", "x"]), "locked");
    wb.json(&["revert", "--all"]);

    // Commit releases the lock unless --keep-locks.
    wa.write("a.psd", "ana's");
    wa.json(&["commit", "-m", "paint", "--keep-locks"]);
    assert_eq!(
        wa.json(&["locks", "--mine"])["locks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    wa.write("a.psd", "ana's again");
    wa.json(&["commit", "-m", "paint again"]);
    assert!(wa.json(&["locks"])["locks"].as_array().unwrap().is_empty());

    // Only the head version can be locked.
    assert_eq!(fails(&wb, &["lock", "a.psd"]), "out_of_date");
    wb.json(&["update"]);
    wb.json(&["lock", "a.psd"]);
    // An admin breaks it.
    let breaker = server
        .token_for("admin", &["read", "write", "admin", "lock"])
        .await;
    let admin = checkout_as(&server, &breaker);
    admin.json(&["unlock", "--force", "a.psd"]);
    assert!(admin.json(&["locks"])["locks"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// §6.2: shelves with versions; unshelve elsewhere; submit by version, owner as author.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s6_2_shelves() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let scopes = ["read", "write", "shelve"];
    let ana = member(&server, "ana", &scopes, AccessLevel::Write).await;
    let bob = member(&server, "bob", &scopes, AccessLevel::Write).await;
    let wa = checkout_as(&server, &ana);
    let wb = checkout_as(&server, &bob);

    wa.write("a.txt", "a by ana\n");
    let s = wa.json(&["shelve", "-m", "proposal"]);
    let id = s["shelf"].as_u64().unwrap();
    assert_eq!(s["version"], 1);
    assert_eq!(text(&wa, "a.txt"), "a\n", "reverted locally");
    let listed = wb.json(&["shelves"]);
    assert_eq!(listed["shelves"][0]["owner"]["name"], "ana");

    // Bob tries it out in his workspace.
    let u = wb.json(&["unshelve", &id.to_string()]);
    assert_eq!(u["files"][0]["outcome"], "applied");
    assert_eq!(text(&wb, "a.txt"), "a by ana\n");
    wb.json(&["revert", "--all"]);

    // Ana revises it: version 2. Approving version 1 is refused.
    wa.json(&["unshelve", &id.to_string()]);
    wa.write("a.txt", "a by ana, revised\n");
    let s2 = wa.json(&["shelve", "--update", &id.to_string()]);
    assert_eq!(s2["version"], 2);
    assert_eq!(
        fails(&wb, &["shelf", "submit", &format!("{id}@1")]),
        "conflict"
    );
    let sub = wb.json(&["shelf", "submit", &format!("{id}@2")]);
    let rev = sub["rev"].as_u64().unwrap();
    let r = server.admin().revision("game", rev).await.unwrap();
    assert_eq!(r.author.name, "ana");
    assert_eq!(r.submitter.name, "bob");
    let st = wb.json(&["shelves", "--state", "submitted"]);
    assert_eq!(st["shelves"][0]["submitted_rev"], rev);
}

/// §6.3: changelists split pending work for commit and shelve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s6_3_changelists() {
    let server = server().await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("ui.txt", b"ui\n"),
                ("net.txt", b"net\n"),
                ("doc.txt", b"doc\n"),
            ],
        )
        .await;
    let ws = admin_ws(&server);
    ws.write("ui.txt", "ui fixed\n");
    ws.write("net.txt", "net fixed\n");
    ws.write("doc.txt", "doc fixed\n");
    ws.json(&["changelist", "new", "ui", "-m", "Fix the UI"]);
    ws.json(&["changelist", "move", "ui", "ui.txt"]);
    let lists = ws.json(&["changelist", "list"]);
    assert_eq!(lists["changelists"][0]["name"], "default");

    let c = ws.json(&["commit", "-c", "ui"]);
    assert_eq!(c["files"], 1);
    assert_eq!(last_revision(&ws)["message"], "Fix the UI");
    let left: Vec<String> = changes(&ws).into_iter().map(|(p, _)| p).collect();
    assert_eq!(left, ["doc.txt", "net.txt"]);
}

/// §6.4: the server and local MCP endpoints, with write tools only for tokens that allow them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s6_4_mcp() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let reader = member(&server, "reader", &["read"], AccessLevel::Read).await;
    let writer = member(
        &server,
        "writer",
        &["read", "write", "shelve", "lock"],
        AccessLevel::Write,
    )
    .await;

    let ro = server_mcp(&server, &reader).await;
    let tools = mcp_tools(&ro).await;
    for t in [
        "whoami",
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
        assert!(tools.contains(&t.to_owned()), "{t} missing from {tools:?}");
    }
    assert!(!tools.contains(&"create_shelf".to_owned()), "{tools:?}");
    let f = mcp_ok(
        &ro,
        "read_file",
        json!({ "depot": "game", "path": "a.txt" }),
    )
    .await;
    assert_eq!(f["text"], "a\n");
    ro.cancel().await.unwrap();

    let rw = server_mcp(&server, &writer).await;
    let tools = mcp_tools(&rw).await;
    assert!(
        tools.contains(&"create_shelf".to_owned()) && tools.contains(&"update_shelf".to_owned())
    );
    rw.cancel().await.unwrap();

    let ws = checkout_as(&server, &writer);
    let local = local_mcp(&ws, &[]).await;
    let tools = mcp_tools(&local).await;
    for t in [
        "status", "diff", "add", "remove", "move", "revert", "update", "lock", "unlock", "shelve",
        "unshelve", "commit",
    ] {
        assert!(tools.contains(&t.to_owned()), "{t} missing from {tools:?}");
    }
    ws.write("a.txt", "via mcp\n");
    let st = mcp_ok(&local, "status", json!({})).await;
    assert_eq!(st["files"][0]["path"], "a.txt");
    local.cancel().await.unwrap();
}

// ---------------------------------------------------------------------------------------
// §7 AI support

/// §7.1: an agent has its own tokens and rules, shelves but cannot commit by default, and
/// stops working when its owner is disabled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s7_1_agent_identities() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let admin = TestWorkspace::new(&server).with_token(server.admin_token());
    member(&server, "ana", &["read", "write"], AccessLevel::Write).await;
    admin.json(&["admin", "agent", "create", "bot", "--owner", "ana"]);
    let token = admin.json(&["admin", "token", "create", "--for", "bot"])["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let bot = TestWorkspace::new(&server).with_token(&token);
    let who = bot.json(&["whoami"]);
    assert_eq!(
        (who["kind"].as_str(), who["owner"].as_str()),
        (Some("agent"), Some("ana"))
    );
    let mut scopes: Vec<&str> = who["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    scopes.sort_unstable();
    assert_eq!(scopes, ["lock", "read", "shelve"]);

    // No inherited access: nothing until a rule names the agent.
    let (status, _) = bot.json_status(&["checkout", &url(&server), "."]);
    assert_ne!(status, 0);
    admin.json(&[
        "admin", "acl", "add", "allow", "write", "bot", "--depot", "game",
    ]);
    bot.json(&["checkout", &url(&server), "."]);
    bot.write("a.txt", "by bot\n");
    assert_eq!(fails(&bot, &["commit", "-m", "x"]), "permission_denied");
    bot.json(&["shelve", "-m", "for ana"]);

    admin.json(&["admin", "user", "disable", "ana"]);
    let (status, _) = bot.json_status(&["whoami"]);
    assert_ne!(status, 0, "an agent of a disabled owner is shut out");
}

/// §7.2 and §7.3: an agent reads and proposes through the server MCP endpoint alone; a person
/// submits; the revision carries author, submitter and provenance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s7_2_workspace_free_and_7_3_provenance() {
    let server = server().await;
    server
        .commit_files("game", "main", &[("src/a.txt", b"one\n")])
        .await;
    let ana = member(
        &server,
        "ana",
        &["read", "write", "shelve"],
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

    let agent = server_mcp(&server, &bot).await;
    let f = mcp_ok(
        &agent,
        "read_file",
        json!({ "depot": "game", "path": "src/a.txt" }),
    )
    .await;
    let shelf = mcp_ok(
        &agent,
        "create_shelf",
        json!({
            "depot": "game", "branch": "main", "base_rev": f["rev"],
            "description": "Count higher",
            "edits": [{ "path": "src/a.txt", "action": "modify", "text": "two\n" }],
            "agent": { "model": "model-x", "session": "s-1", "tool": "tool-y" }
        }),
    )
    .await;
    agent.cancel().await.unwrap();
    let id = shelf["id"].as_u64().unwrap();

    let ws = checkout_as(&server, &ana);
    let sub = ws.json(&["shelf", "submit", &format!("{id}@1")]);
    let r = server
        .admin()
        .revision("game", sub["rev"].as_u64().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (r.author.name.as_str(), r.submitter.name.as_str()),
        ("bot", "ana")
    );
    // The shelf was made through MCP; the revision through the API by the person's submit,
    // and it carries the agent's record and the shelf it came from.
    assert_eq!(shelf["provenance"]["via"], "mcp", "{shelf}");
    let p = serde_json::to_value(&r.provenance).unwrap();
    assert_eq!(p["via"], "api", "{p}");
    assert_eq!(p["submitted_shelf"], id, "{p}");
    assert_eq!(p["agent"]["model"], "model-x", "{p}");

    // Through the CLI the server says `api`, and the client names itself.
    ws.json(&["update"]);
    assert_eq!(text(&ws, "src/a.txt"), "two\n");
    ws.write("src/a.txt", "three\n");
    ws.json(&["commit", "-m", "by hand"]);
    let r = last_revision(&ws);
    assert_eq!(r["provenance"]["via"], "api", "{r}");
    assert!(r["provenance"]["client"]
        .as_str()
        .unwrap()
        .starts_with("lode-cli/"));
}

/// §7.4: one JSON document on stdout, a stable error code, a non-zero exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s7_4_machine_readable_cli() {
    let server = server().await;
    let ws = admin_ws(&server);
    let out = ws.cmd().args(["--json", "status"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);

    let out = ws
        .cmd()
        .args(["--json", "cat", "missing.txt"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["ok"].as_bool(), code(&v)), (Some(false), "not_found"));

    let out = ws
        .cmd()
        .args(["--json", "commit", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(code(&v), "usage");
}
