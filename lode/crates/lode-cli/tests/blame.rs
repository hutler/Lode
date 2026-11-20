//! T052 acceptance: `lode blame`, across a rename and a branch point.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn attribution(v: &Value) -> Vec<(u64, String)> {
    v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["rev"].as_u64().unwrap(),
                l["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn rev_of(v: &Value) -> u64 {
    v["rev"].as_u64().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_follows_renames_and_branches() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"one\ntwo\nthree\n"),
                ("bin.dat", b"\x00\x01\x02"),
            ],
        )
        .await;
    let r2 = server
        .commit_files("game", "main", &[("a.txt", b"one\nTWO\nthree\n")])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.json(&["mv", "a.txt", "b.txt"]);
    let r3 = rev_of(&ws.json(&["commit", "-m", "Rename a to b"]));
    ws.write("b.txt", "one\nTWO\nthree\nfour\r\n");
    let r4 = rev_of(&ws.json(&["commit", "-m", "Add four\nwith a body"]));
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
    let r6 = server
        .commit_files("game", "feat", &[("b.txt", b"ONE\nTWO\nthree\nfour\r\n")])
        .await;
    // A later change on main is not in feat's history.
    server
        .commit_files("game", "main", &[("b.txt", b"one\nTWO\nTHREE\nfour\r\n")])
        .await;

    ws.json(&["switch", "feat"]);
    let b = ws.json(&["blame", "b.txt"]);
    assert_eq!(b["rev"], r6);
    assert_eq!(
        attribution(&b),
        vec![
            (r6, "ONE".to_owned()),
            (r2, "TWO".to_owned()),
            (r1, "three".to_owned()),
            (r4, "four".to_owned()),
        ]
    );
    assert!(b.get("truncated").is_none(), "{b}");
    let revs: Vec<(u64, String)> = b["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["rev"].as_u64().unwrap(),
                r["branch"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        revs,
        vec![
            (r6, "feat".to_owned()),
            (r4, "main".to_owned()),
            (r2, "main".to_owned()),
            (r1, "main".to_owned()),
        ]
    );
    assert_eq!(b["revisions"][1]["summary"], "Add four");

    // At an older revision, by depot path without a workspace, and in human form.
    let old = ws.json(&["blame", &format!("b.txt@{r3}")]);
    assert_eq!(
        attribution(&old)
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>(),
        vec![r1, r2, r1]
    );
    let outside = TestWorkspace::new(&server).with_token(server.admin_token());
    let far = outside.json(&[
        "--server",
        server.url(),
        "blame",
        "--depot",
        "game",
        &format!("a.txt@{r1}"),
    ]);
    assert_eq!(attribution(&far).len(), 3);
    let out = ws.lode(&["blame", "b.txt"]).get_output().stdout.clone();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(&format!("r{r6}")) && out.contains("| ONE"),
        "{out}"
    );

    // Binary files and directories have no blame.
    let (code, err) = ws.json_status(&["blame", "bin.dat"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "invalid_request");
    let (code, err) = ws.json_status(&["blame", "missing.txt"]);
    assert_ne!(code, 0);
    assert_eq!(err["error"]["code"], "not_found");
}
