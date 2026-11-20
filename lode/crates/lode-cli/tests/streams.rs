//! T161 acceptance: streams in the command line.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn refused(ws: &TestWorkspace, args: &[&str], direction: &str) -> Value {
    let (code, v) = ws.json_status(args);
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["error"]["code"], "conflict", "{v}");
    assert_eq!(v["error"]["details"]["reason"], "stream_flow", "{v}");
    assert_eq!(v["error"]["details"]["direction"], direction, "{v}");
    v
}

fn child<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} under {node}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_merge_only_the_way_their_types_allow() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    let token = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    let v = ws.json(&["branch", "set-type", "main", "mainline"]);
    assert_eq!(v["stream"], "mainline");
    for (name, t) in [
        ("dev", "development"),
        ("rel", "release"),
        ("dev2", "development"),
    ] {
        let v = ws.json(&["branch", "create", name, "--from", "main", "--type", t]);
        assert_eq!(v["stream"], t, "{v}");
    }
    // A new branch has a parent, so it cannot be a mainline.
    let (code, _) = ws.json_status(&["branch", "create", "m2", "--type", "mainline"]);
    assert_eq!(code, 2);

    server
        .commit_files("game", "main", &[("m1.txt", b"m1\n")])
        .await;
    let m2 = server
        .commit_files("game", "main", &[("m2.txt", b"m2\n")])
        .await;
    server
        .commit_files("game", "dev", &[("d.txt", b"d\n")])
        .await;
    server
        .commit_files("game", "rel", &[("fix.txt", b"fix\n")])
        .await;

    let list = ws.json(&["branch", "list"]);
    let dev = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "dev")
        .unwrap();
    assert_eq!(dev["stream"], "development");
    let tree = ws.json(&["branch", "tree"]);
    let main = &tree["tree"][0];
    assert_eq!(main["name"], "main", "{tree}");
    assert_eq!(main["stream"], "mainline");
    assert!(main["down"].is_null());
    let dev = child(main, "dev");
    // The revision that created the branch counts as one of its own.
    assert_eq!(
        (dev["down"].as_u64(), dev["up"].as_u64()),
        (Some(2), Some(2)),
        "{tree}"
    );
    assert_eq!(child(main, "dev2")["up"], 1);
    assert_eq!(child(main, "rel")["stream"], "release");

    // Sideways: refused before anything is written.
    ws.json(&["switch", "dev2"]);
    refused(&ws, &["merge", "dev"], "sideways");
    assert!(!ws.exists("d.txt"));
    assert!(ws.json(&["status"])["merges"]
        .as_array()
        .is_none_or(Vec::is_empty));

    // Copy up while behind: refused with the count; fine after merging down.
    ws.json(&["switch", "main"]);
    let v = refused(&ws, &["merge", "dev"], "copy_up");
    assert_eq!(v["error"]["details"]["behind"], 2, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("merge `main` into `dev` first"),
        "{v}"
    );
    ws.json(&["switch", "dev"]);
    let m = ws.json(&["merge", "main"]);
    assert_eq!(m["flow"]["direction"], "merge_down");
    ws.json(&["commit", "-m", "down"]);
    ws.json(&["switch", "main"]);
    let m = ws.json(&["merge", "dev"]);
    assert_eq!(m["flow"]["direction"], "copy_up");
    let c = ws.json(&["commit", "-m", "copy up"]);
    assert_eq!(c["forced_flow"], false);
    assert_eq!(String::from_utf8(ws.read("d.txt")).unwrap(), "d\n");

    // A release stream takes picks from its parent, not whole merges.
    ws.json(&["switch", "rel"]);
    refused(&ws, &["merge", "main"], "merge_down");
    let m2s = m2.to_string();
    ws.json(&["merge", "main", "-r", &m2s]);
    ws.json(&["commit", "-m", "pick"]);
    assert!(ws.exists("m2.txt") && !ws.exists("m1.txt"));
    // And merges up freely.
    ws.json(&["switch", "main"]);
    ws.json(&["merge", "rel"]);
    ws.json(&["commit", "-m", "fixes up"]);
    assert!(ws.exists("fix.txt"));

    // A depot admin may force a refused merge; the commit sends it on.
    ws.json(&["switch", "dev2"]);
    let m = ws.json(&["merge", "dev", "--force-flow"]);
    assert_eq!(m["flow"]["allowed"], false);
    let c = ws.json(&["commit", "-m", "forced"]);
    assert_eq!(c["forced_flow"], true, "{c}");

    // A shelf carrying a refused merge is refused on submit, unless forced.
    server
        .commit_files("game", "dev", &[("d3.txt", b"d3\n")])
        .await;
    ws.json(&["merge", "dev", "--force-flow"]);
    let id = ws.json(&["shelve", "-m", "side"])["shelf"]
        .as_u64()
        .unwrap();
    let spec = format!("{id}@1");
    refused(&ws, &["shelf", "submit", &spec], "sideways");
    let v = ws.json(&["shelf", "submit", &spec, "--force-flow"]);
    assert!(v["rev"].as_u64().is_some(), "{v}");

    // A writer cannot.
    let ana = server.create_user("ana", &["read", "write"]).await;
    server
        .acl(
            "game",
            "ana",
            "**",
            "**",
            lode_proto::admin::AclEffect::Allow,
            lode_proto::admin::AccessLevel::Write,
        )
        .await;
    server
        .commit_files("game", "dev", &[("d2.txt", b"d2\n")])
        .await;
    let w = TestWorkspace::new(&server).with_token(&ana);
    w.json(&["checkout", &format!("{}/game", server.url()), "."]);
    w.json(&["switch", "dev2"]);
    w.json(&["merge", "dev", "--force-flow"]);
    let (code, v) = w.json_status(&["commit", "-m", "forced by ana"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "permission_denied", "{v}");
}
