//! T051 acceptance: `lode merge` and `merged_from`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_proto::commit::MergedFrom;
use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::{json, Value};

fn checkout(server: &TestServer) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

async fn branch(server: &TestServer, name: &str) -> u64 {
    server
        .admin()
        .create_branch(
            "game",
            &CreateBranchRequest {
                name: name.into(),
                from_branch: "main".into(),
                from_rev: None,
                stream: None,
            },
        )
        .await
        .unwrap()
        .created_rev
}

fn outcomes(v: &Value) -> BTreeMap<String, String> {
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

fn text(ws: &TestWorkspace, path: &str) -> String {
    String::from_utf8(ws.read(path)).unwrap()
}

fn range(branch: &str, from_rev: u64, to_rev: u64) -> MergedFrom {
    MergedFrom {
        branch: branch.into(),
        from_rev,
        to_rev,
    }
}

async fn merged_from(server: &TestServer, rev: u64) -> Vec<MergedFrom> {
    server
        .admin()
        .revision("game", rev)
        .await
        .unwrap()
        .merged_from
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_branch_merges_and_records_ranges() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files(
            "game",
            "main",
            &[("a.txt", b"1\n2\n3\n"), ("keep.txt", b"keep\n")],
        )
        .await;
    branch(&server, "feat").await;
    server
        .commit_files("game", "feat", &[("a.txt", b"1\n2\nTHREE\n")])
        .await;
    server
        .commit_files("game", "feat", &[("new.txt", b"new\n")])
        .await;
    let f3 = server
        .commit_files("game", "feat", &[("new.txt", b"newer\n")])
        .await;
    let ws = checkout(&server);
    // A local edit to another region of a.txt is merged with the branch's.
    ws.write("a.txt", "ONE\n2\n3\n");

    let m = ws.json(&["merge", "feat"]);
    assert_eq!(m["from_rev"], r1 + 1);
    assert_eq!(m["to_rev"], f3);
    assert_eq!(m["base"], r1);
    let o = outcomes(&m);
    assert_eq!(o.get("a.txt").map(String::as_str), Some("merged"), "{o:?}");
    assert_eq!(o.get("new.txt").map(String::as_str), Some("applied"));
    assert!(!o.contains_key("keep.txt"));
    assert_eq!(text(&ws, "a.txt"), "ONE\n2\nTHREE\n");
    assert_eq!(text(&ws, "new.txt"), "newer\n");

    let st = ws.json(&["status"]);
    assert_eq!(
        st["merges"],
        json!([{ "branch": "feat", "from_rev": r1 + 1, "to_rev": f3 }])
    );
    // A second merge waits for this one to be committed.
    let (code, err) = ws.json_status(&["merge", "feat"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "merge_pending");
    // The merge goes in whole.
    let (code, _) = ws.json_status(&["commit", "-m", "part", "a.txt"]);
    assert_ne!(code, 0);

    let c = ws.json(&["commit", "-m", "Merge feat"]);
    let rev = c["rev"].as_u64().unwrap();
    assert_eq!(
        merged_from(&server, rev).await,
        vec![range("feat", r1 + 1, f3)]
    );
    assert_eq!(ws.json(&["status"])["merges"], json!([]));
    let log = ws
        .lode(&["log", "-v", "-n", "1"])
        .get_output()
        .stdout
        .clone();
    let log = String::from_utf8(log).unwrap();
    assert!(log.contains("feat"), "{log}");

    // Nothing new on the source: nothing to do.
    let again = ws.json(&["merge", "feat"]);
    assert_eq!(again["nothing_to_do"], true);
    assert_eq!(again["base"], f3);

    // New source commits after the merge: only those come over.
    let f4 = server
        .commit_files("game", "feat", &[("later.txt", b"later\n")])
        .await;
    ws.json(&["update"]);
    let next = ws.json(&["merge", "feat"]);
    assert_eq!(next["from_rev"], f3 + 1);
    assert_eq!(next["to_rev"], f4);
    assert_eq!(
        outcomes(&next),
        BTreeMap::from([("later.txt".to_owned(), "applied".to_owned())])
    );
    let c = ws.json(&["commit", "-m", "Merge feat again"]);
    let rev = c["rev"].as_u64().unwrap();
    assert_eq!(
        merged_from(&server, rev).await,
        vec![range("feat", f3 + 1, f4)]
    );

    // The other way round: main's merge commits come back to feat as content.
    let info = server
        .admin()
        .merge_info("game", "feat", "main")
        .await
        .unwrap();
    assert_eq!(info.branch_point, r1);
    assert!(info.pending > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn divergent_edits_conflict() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[("t.txt", b"base\n"), ("bin.dat", b"\x00\x01\x02")],
        )
        .await;
    branch(&server, "feat").await;
    let f = server
        .commit_files(
            "game",
            "feat",
            &[("t.txt", b"feat\n"), ("bin.dat", b"\x00\x09")],
        )
        .await;
    server
        .commit_files(
            "game",
            "main",
            &[("t.txt", b"main\n"), ("bin.dat", b"\x00\x08")],
        )
        .await;
    let ws = checkout(&server);

    let m = ws.json(&["merge", "feat"]);
    let o = outcomes(&m);
    assert_eq!(o.get("t.txt").map(String::as_str), Some("conflicted"));
    assert_eq!(o.get("bin.dat").map(String::as_str), Some("conflicted"));
    let t = text(&ws, "t.txt");
    assert!(
        t.contains("<<<<<<< mine") && t.contains(&format!(">>>>>>> feat r{f}")),
        "{t}"
    );
    assert_eq!(ws.read(&format!("bin.dat.theirs-r{f}")), b"\x00\x09");
    assert_eq!(ws.read("bin.dat"), b"\x00\x08");

    // Nothing is committed while conflicts remain.
    let (code, _) = ws.json_status(&["commit", "-m", "x"]);
    assert_ne!(code, 0);
    ws.json(&["resolve", "t.txt", "--theirs"]);
    assert_eq!(text(&ws, "t.txt"), "feat\n");
    ws.json(&["resolve", "bin.dat", "--mine"]);
    assert!(!ws.exists(&format!("bin.dat.theirs-r{f}")));
    let c = ws.json(&["commit", "-m", "Merge feat"]);
    let rev = c["rev"].as_u64().unwrap();
    let view = server.admin().revision("game", rev).await.unwrap();
    assert_eq!(view.merged_from.len(), 1);
    let file = server
        .admin()
        .file("game", rev, &"t.txt".parse().unwrap())
        .await
        .unwrap();
    let mut body = Vec::new();
    file.write_to(&mut body).await.unwrap();
    assert_eq!(body, b"feat\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_revision_pick_shelve_and_abandon() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("a.txt", b"a\n")])
        .await;
    branch(&server, "feat").await;
    server
        .commit_files("game", "feat", &[("x.txt", b"x\n")])
        .await;
    let f2 = server
        .commit_files("game", "feat", &[("y.txt", b"y\n"), ("a.txt", b"A\n")])
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
    let ws = TestWorkspace::new(&server).with_token(&ana);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    // -r N takes exactly that revision's changes.
    let m = ws.json(&["merge", "feat", "-r", &f2.to_string()]);
    assert_eq!(
        (m["from_rev"].as_u64(), m["to_rev"].as_u64()),
        (Some(f2), Some(f2))
    );
    assert_eq!(
        outcomes(&m).keys().cloned().collect::<Vec<_>>(),
        vec!["a.txt", "y.txt"]
    );
    assert!(!ws.exists("x.txt"));

    // revert --merge forgets the merge and leaves the files; --all clears both.
    let r = ws.json(&["revert", "--merge"]);
    assert_eq!(r["merges_abandoned"][0]["branch"], "feat");
    assert_eq!(ws.json(&["status"])["merges"], json!([]));
    assert!(ws.exists("y.txt"));
    ws.json(&["merge", "feat", "-r", &f2.to_string()]);
    ws.json(&["revert", "--all"]);
    assert_eq!(ws.json(&["status"])["merges"], json!([]));
    assert_eq!(text(&ws, "a.txt"), "a\n");
    ws.json(&["clean"]);

    // Shelving carries the merge to the shelf; unshelving brings it back.
    ws.json(&["merge", "feat", "-r", &f2.to_string()]);
    let s = ws.json(&["shelve", "-m", "pick"]);
    let id = s["shelf"].as_u64().unwrap();
    assert_eq!(
        s["merged_from"],
        json!([{ "branch": "feat", "from_rev": f2, "to_rev": f2 }])
    );
    let shelf = server.admin().shelf("game", id, false).await.unwrap();
    assert_eq!(shelf.merged_from, vec![range("feat", f2, f2)]);
    assert_eq!(ws.json(&["status"])["merges"], json!([]));
    ws.json(&["unshelve", &id.to_string()]);
    assert_eq!(
        ws.json(&["status"])["merges"],
        json!([{ "branch": "feat", "from_rev": f2, "to_rev": f2 }])
    );
    let c = ws.json(&["commit", "-m", "Pick"]);
    let rev = c["rev"].as_u64().unwrap();
    assert_eq!(merged_from(&server, rev).await, vec![range("feat", f2, f2)]);

    // Known limit: the pick leaves a gap, so a full merge starts at the branch point again
    // and re-applies the picked change without effect.
    let full = ws.json(&["merge", "feat"]);
    assert_eq!(full["base"], r1);
    let o = outcomes(&full);
    assert_eq!(o.get("x.txt").map(String::as_str), Some("applied"));
    assert_eq!(o.get("y.txt").map(String::as_str), Some("unchanged"));
    assert_eq!(o.get("a.txt").map(String::as_str), Some("unchanged"));
}
