//! T033 acceptance: shelve, list, show, diff, unshelve and delete from a workspace.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::{json, Value};

const BIN: &[u8] = b"\x00\x01\x02binary\xff";

struct Team {
    server: TestServer,
    ana: String,
    bob: String,
}

async fn team() -> Team {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"one\ntwo\nthree\nfour\nfive\n"),
                ("a2.txt", b"alpha\nbeta\n"),
                ("b.txt", b"banana\n"),
                ("c.txt", b"cherry\n"),
                ("d/e.txt", b"elder\n"),
                ("bin.dat", BIN),
            ],
        )
        .await;
    let scopes = ["read", "write", "shelve"];
    let ana = server.create_user("ana", &scopes).await;
    let bob = server.create_user("bob", &scopes).await;
    for user in ["ana", "bob"] {
        server
            .acl(
                "game",
                user,
                "*",
                "**",
                AclEffect::Allow,
                AccessLevel::Write,
            )
            .await;
    }
    Team { server, ana, bob }
}

fn checkout(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

/// `status --json` without the changelist and lock fields.
fn pending(ws: &TestWorkspace) -> Vec<Value> {
    ws.json(&["status"])["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            json!({
                "path": f["path"],
                "status": f["status"],
                "copy_from": f["copy_from"]["path"],
                "moved": f["moved"],
            })
        })
        .collect()
}

fn outcomes(v: &Value) -> Vec<(String, String)> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shelve_then_unshelve_restores_the_pending_state() {
    let t = team().await;
    let ws = checkout(&t.server, &t.ana);
    ws.write("a.txt", "one\ntwo\nTHREE\nfour\nfive\n");
    ws.write("bin.dat", "\x00binary 2");
    ws.json(&["rm", "c.txt"]);
    ws.json(&["mv", "b.txt", "b2.txt"]);
    ws.write("d/new.txt", "new\n");
    ws.json(&["add", "d/new.txt"]);
    let before = pending(&ws);
    assert_eq!(before.len(), 6, "{before:?}");

    let out = ws.json(&["shelve", "-m", "work in progress"]);
    let id = out["shelf"].as_u64().unwrap();
    assert_eq!(
        (out["version"].clone(), out["files"].clone()),
        (json!(1), json!(6))
    );
    assert_eq!(out["kept"], false);
    // The workspace is clean again.
    assert!(pending(&ws).is_empty(), "{:?}", pending(&ws));
    assert_eq!(ws.read("a.txt"), b"one\ntwo\nthree\nfour\nfive\n");
    assert_eq!(ws.read("b.txt"), b"banana\n");
    assert_eq!(ws.read("c.txt"), b"cherry\n");
    assert!(!ws.root().join("b2.txt").exists());
    assert!(!ws.root().join("d/new.txt").exists());

    // Listing, showing and diffing print the version.
    let list = ws.json(&["shelves"]);
    assert_eq!(list["shelves"][0]["id"], id);
    assert_eq!(list["shelves"][0]["version"], 1);
    let text = String::from_utf8(ws.lode(&["shelves"]).get_output().stdout.clone()).unwrap();
    assert!(
        text.starts_with(&format!("{id}  v1  ana  main  work in progress")),
        "{text}"
    );
    let show = ws.json(&["shelf", "show", &id.to_string()]);
    assert_eq!(show["changes"].as_array().unwrap().len(), 6);
    let text = String::from_utf8(
        ws.lode(&["shelf", "show", &id.to_string()])
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        text.starts_with(&format!("Shelf {id}  version 1  open\n")),
        "{text}"
    );
    assert!(text.contains("A  b2.txt  (from b.txt@"), "{text}");
    let diff = ws.json(&["shelf", "diff", &id.to_string()]);
    let a = diff["diff"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["path"] == "a.txt")
        .unwrap();
    assert!(
        a["patch"].as_str().unwrap().contains("-three\n+THREE\n"),
        "{a}"
    );

    // Unshelve brings back exactly what was pending.
    let out = ws.json(&["unshelve", &id.to_string()]);
    assert!(outcomes(&out).iter().all(|(_, o)| o == "applied"), "{out}");
    assert_eq!(pending(&ws), before);
    assert_eq!(ws.read("a.txt"), b"one\ntwo\nTHREE\nfour\nfive\n");
    assert_eq!(ws.read("bin.dat"), b"\x00binary 2");
    assert_eq!(ws.read("b2.txt"), b"banana\n");
    assert_eq!(ws.read("d/new.txt"), b"new\n");
    assert!(!ws.root().join("c.txt").exists());
    // Unshelving twice changes nothing.
    let again = ws.json(&["unshelve", &id.to_string()]);
    assert!(
        outcomes(&again).iter().all(|(_, o)| o == "unchanged"),
        "{again}"
    );

    // And it commits as a rename.
    ws.json(&["commit", "-m", "done"]);
    let log = ws.json(&["log", "-n", "1", "-v"]);
    let b2 = log["revisions"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "b2.txt")
        .cloned()
        .unwrap();
    assert_eq!(b2["copy_from"]["path"], "b.txt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unshelving_another_users_shelf_merges_or_conflicts() {
    let t = team().await;
    let a = checkout(&t.server, &t.ana);
    let b = checkout(&t.server, &t.bob);
    a.write("a.txt", "ONE\ntwo\nthree\nfour\nfive\n");
    a.write("a2.txt", "alpha\nBETA by ana\n");
    a.write("bin.dat", "ana's binary\x00");
    a.write("c.txt", "cherry by ana\n");
    let id = a.json(&["shelve", "-m", "ana's work"])["shelf"]
        .as_u64()
        .unwrap();

    b.write("a.txt", "one\ntwo\nthree\nfour\nFIVE\n");
    b.write("a2.txt", "alpha\nBETA by bob\n");
    b.write("bin.dat", "bob's binary\x00");
    let out = b.json(&["unshelve", &id.to_string()]);
    assert_eq!(
        outcomes(&out),
        vec![
            ("a.txt".into(), "merged".into()),
            ("a2.txt".into(), "conflicted".into()),
            ("bin.dat".into(), "conflicted".into()),
            ("c.txt".into(), "applied".into()),
        ]
    );
    assert_eq!(b.read("a.txt"), b"ONE\ntwo\nthree\nfour\nFIVE\n");
    assert_eq!(b.read("c.txt"), b"cherry by ana\n");
    let a2 = String::from_utf8(b.read("a2.txt")).unwrap();
    assert!(a2.contains("<<<<<<< mine\nBETA by bob\n"), "{a2}");
    assert!(a2.contains(&format!(">>>>>>> shelf {id}\n")), "{a2}");
    assert_eq!(b.read("bin.dat"), b"bob's binary\x00");
    assert_eq!(b.read("bin.dat.theirs-shelf"), b"ana's binary\x00");
    let (code, v) = b.json_status(&["commit", "-m", "x"]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("conflicted")));

    // Taking the shelf's side keeps the workspace's base: the file is simply modified.
    b.json(&["resolve", "--theirs", "a2.txt"]);
    assert_eq!(b.read("a2.txt"), b"alpha\nBETA by ana\n");
    b.json(&["resolve", "--mine", "bin.dat"]);
    assert_eq!(b.read("bin.dat"), b"bob's binary\x00");
    assert!(!b.root().join("bin.dat.theirs-shelf").exists());
    let st = pending(&b);
    let names: Vec<(&str, &str)> = st
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("a.txt", "modified"),
            ("a2.txt", "modified"),
            ("bin.dat", "modified"),
            ("c.txt", "modified"),
        ]
    );
    b.json(&["commit", "-m", "bob takes ana's work"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_replaces_a_shelf_and_delete_removes_it() {
    let t = team().await;
    let a = checkout(&t.server, &t.ana);
    a.write("a.txt", "first\n");
    a.json(&["changelist", "new", "ui", "-m", "Button work"]);
    a.json(&["changelist", "move", "ui", "a.txt"]);
    a.write("b.txt", "not shelved\n");
    // A changelist's description is the default description.
    let out = a.json(&["shelve", "-c", "ui", "--keep"]);
    let id = out["shelf"].as_u64().unwrap();
    assert_eq!(out["files"], 1);
    assert_eq!(a.read("a.txt"), b"first\n");
    let show = a.json(&["shelf", "show", &id.to_string()]);
    assert_eq!(show["description"], "Button work");

    a.write("a.txt", "second\n");
    let out = a.json(&["shelve", "--update", &id.to_string(), "a.txt"]);
    assert_eq!(
        (out["shelf"].clone(), out["version"].clone()),
        (json!(id), json!(2))
    );
    assert_eq!(a.read("a.txt"), b"one\ntwo\nthree\nfour\nfive\n");
    assert_eq!(a.read("b.txt"), b"not shelved\n");
    let show = a.json(&["shelf", "show", &id.to_string()]);
    assert_eq!(
        (show["version"].clone(), show["description"].clone()),
        (json!(2), json!("Button work"))
    );

    // Bob sees the new content, and cannot replace ana's shelf.
    let b = checkout(&t.server, &t.bob);
    b.json(&["unshelve", &id.to_string()]);
    assert_eq!(b.read("a.txt"), b"second\n");
    assert_eq!(b.json(&["shelves", "--mine"])["shelves"], json!([]));
    assert_eq!(
        b.json(&["shelves", "--user", "ana"])["shelves"][0]["id"],
        id
    );
    let (code, _) = b.json_status(&["shelve", "--update", &id.to_string(), "-m", "mine now"]);
    assert_ne!(code, 0);

    // Nothing to shelve is an error; so is a shelf without a description.
    let (code, v) = a.json_status(&["shelve", "-m", "x", "c.txt"]);
    assert_eq!(
        (code, v["error"]["code"].clone()),
        (1, json!("nothing_to_shelve"))
    );
    let (code, _) = a.json_status(&["shelve", "b.txt"]);
    assert_eq!(code, 2);

    let out = a.json(&["shelf", "delete", &id.to_string()]);
    assert_eq!(out["deleted"], id);
    assert_eq!(a.json(&["shelves"])["shelves"], json!([]));
    let (code, v) = a.json_status(&["unshelve", &id.to_string()]);
    assert_eq!((code, v["error"]["code"].clone()), (1, json!("not_found")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shelving_needs_an_up_to_date_workspace() {
    let t = team().await;
    let a = checkout(&t.server, &t.ana);
    let b = checkout(&t.server, &t.bob);
    b.write("a.txt", "bob\n");
    b.json(&["commit", "-m", "bob"]);
    a.write("a.txt", "ana\n");
    let (code, v) = a.json_status(&["shelve", "-m", "late"]);
    assert_eq!(
        (code, v["error"]["code"].clone()),
        (1, json!("out_of_date"))
    );
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("lode update"));
    // Nothing was reverted.
    assert_eq!(a.read("a.txt"), b"ana\n");
}
