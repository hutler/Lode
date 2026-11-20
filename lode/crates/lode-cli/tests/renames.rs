//! T162 acceptance: merges follow renames.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

async fn setup(files: &[(&str, &[u8])]) -> (TestServer, TestWorkspace, TestWorkspace) {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server.commit_files("game", "main", files).await;
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
    let url = format!("{}/game", server.url());
    let main = TestWorkspace::new(&server).with_token(server.admin_token());
    main.json(&["checkout", &url, "."]);
    let feat = TestWorkspace::new(&server).with_token(server.admin_token());
    feat.json(&["checkout", &url, "."]);
    feat.json(&["switch", "feat"]);
    (server, main, feat)
}

/// path → (outcome, from)
fn files(v: &Value) -> BTreeMap<String, (String, Option<String>)> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                (
                    f["outcome"].as_str().unwrap().to_owned(),
                    f["from"].as_str().map(str::to_owned),
                ),
            )
        })
        .collect()
}

fn text(ws: &TestWorkspace, p: &str) -> String {
    String::from_utf8(ws.read(p)).unwrap()
}

fn o(outcome: &str, from: Option<&str>) -> (String, Option<String>) {
    (outcome.to_owned(), from.map(str::to_owned))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renames_on_either_side_are_followed() {
    let (server, main, feat) = setup(&[
        ("a.txt", b"1\n2\n3\n"),
        ("c.txt", b"x\ny\nz\n"),
        ("bin.dat", b"\x00\x01\x02binary"),
        ("x.txt", b"p\nq\nr\n"),
        ("e.txt", b"e1\ne2\ne3\n"),
    ])
    .await;

    // There: rename and edit a, edit c and e, rename the binary, rename x twice.
    feat.json(&["mv", "a.txt", "b.txt"]);
    feat.write("b.txt", "1\n2\nTHREE\n");
    feat.write("c.txt", "x\ny\nZED\n");
    feat.write("e.txt", "e1\ne2\nE3\n");
    feat.json(&["mv", "bin.dat", "bin2.dat"]);
    feat.json(&["mv", "x.txt", "y.txt"]);
    feat.json(&["commit", "-m", "one"]);
    // `mv` records the synced revision as the source; sync past our own commit first.
    feat.json(&["update"]);
    feat.json(&["mv", "y.txt", "z.txt"]);
    feat.json(&["commit", "-m", "two"]);

    // Here: a committed rename of e, an uncommitted one of c, edits to a and x.
    main.json(&["mv", "e.txt", "f.txt"]);
    main.json(&["commit", "-m", "rename e"]);
    main.write("a.txt", "ONE\n2\n3\n");
    main.write("x.txt", "P\nq\nr\n");
    main.json(&["mv", "c.txt", "d.txt"]);

    let m = main.json(&["merge", "feat"]);
    let f = files(&m);
    assert_eq!(f["b.txt"], o("merged", Some("a.txt")), "{m}");
    assert_eq!(f["a.txt"].0, "applied", "{m}");
    assert_eq!(f["d.txt"], o("applied", Some("c.txt")), "{m}");
    assert_eq!(f["f.txt"], o("applied", Some("e.txt")), "{m}");
    assert_eq!(f["bin2.dat"], o("applied", Some("bin.dat")), "{m}");
    assert_eq!(f["z.txt"], o("merged", Some("x.txt")), "{m}");
    assert!(
        !m["counts"].as_object().unwrap().contains_key("conflicted"),
        "{m}"
    );

    assert_eq!(text(&main, "b.txt"), "ONE\n2\nTHREE\n");
    assert!(!main.exists("a.txt"));
    assert_eq!(text(&main, "d.txt"), "x\ny\nZED\n");
    assert!(!main.exists("c.txt"));
    assert_eq!(text(&main, "f.txt"), "e1\ne2\nE3\n");
    assert!(!main.exists("e.txt"));
    assert_eq!(main.read("bin2.dat"), b"\x00\x01\x02binary");
    assert!(!main.exists("bin.dat"));
    assert_eq!(text(&main, "z.txt"), "P\nq\nr\n");
    assert!(!main.exists("x.txt") && !main.exists("y.txt"));

    // The commit records the renames as renames.
    let c = main.json(&["commit", "-m", "merge feat"]);
    let rev = c["rev"].as_u64().unwrap();
    let r = server
        .admin()
        .revisions(
            "game",
            &lode_proto::read::RevisionListQuery {
                after: Some(rev - 1),
                changes: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let changes = r.revisions[0].changes.clone().unwrap();
    let copied: BTreeMap<String, String> = changes
        .iter()
        .filter_map(|c| {
            c.copy_from
                .as_ref()
                .map(|cf| (c.path.as_str().to_owned(), cf.path.as_str().to_owned()))
        })
        .collect();
    assert_eq!(
        copied.get("b.txt").map(String::as_str),
        Some("a.txt"),
        "{copied:?}"
    );
    assert_eq!(
        copied.get("z.txt").map(String::as_str),
        Some("x.txt"),
        "{copied:?}"
    );
    assert_eq!(
        copied.get("d.txt").map(String::as_str),
        Some("c.txt"),
        "{copied:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renamed_differently_on_both_sides_is_a_conflict() {
    let (_server, main, feat) = setup(&[("g.txt", b"g1\ng2\ng3\n")]).await;
    feat.json(&["mv", "g.txt", "i.txt"]);
    feat.write("i.txt", "g1\ng2\nG3\n");
    feat.json(&["commit", "-m", "to i"]);
    main.json(&["mv", "g.txt", "h.txt"]);
    main.json(&["commit", "-m", "to h"]);

    let m = main.json(&["merge", "feat"]);
    let f = files(&m);
    assert_eq!(f["i.txt"], o("rename_conflict", Some("g.txt")), "{m}");
    assert_eq!(f["h.txt"], o("applied", Some("g.txt")), "{m}");
    assert_eq!(text(&main, "h.txt"), "g1\ng2\nG3\n");
    assert!(!main.exists("i.txt"));
    let (code, _) = main.json_status(&["commit", "-m", "x"]);
    assert_ne!(code, 0, "a conflict waits");
    main.json(&["resolve", "i.txt", "--mine"]);
    main.json(&["commit", "-m", "merge feat"]);
    assert!(!main.exists("i.txt") && main.exists("h.txt"));
}
