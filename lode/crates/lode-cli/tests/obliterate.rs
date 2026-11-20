//! T120 acceptance: `lode admin obliterate` removes content for good and leaves history.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_core::{blob_id, ErrorCode};
use lode_proto::depot::CreateBranchRequest;
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

const SECRET: &[u8] = b"password=hunter2\n";

fn rev_of(v: &Value) -> u64 {
    v["rev"].as_u64().unwrap()
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

/// The ids and uses of a report, as `(rev, path)` per object.
fn uses(v: &Value) -> Vec<Vec<(u64, String)>> {
    v["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            o["uses"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| {
                    (
                        u["rev"].as_u64().unwrap(),
                        u["path"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leaked_secret_is_removed_and_stays_out() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.write("config.ini", SECRET);
    ws.write("old/config.ini", SECRET);
    ws.write("readme.txt", "hello\n");
    ws.json(&["add", "."]);
    let r1 = rev_of(&ws.json(&["commit", "-m", "Add config"]));
    ws.write("config.ini", "password=\n");
    ws.json(&["rm", "old/config.ini"]);
    let r2 = rev_of(&ws.json(&["commit", "-m", "Remove the password"]));
    let secret_id = blob_id(SECRET).unwrap();
    let at_r1 = format!("config.ini@r{r1}");

    // Not for ordinary users.
    let ana = server.create_user("ana", &["read", "write"]).await;
    let (status, v) = TestWorkspace::new(&server).with_token(&ana).json_status(&[
        "admin",
        "obliterate",
        "game",
        &at_r1,
        "--reason",
        "x",
        "--yes",
    ]);
    assert_eq!(status, 3, "{v}");
    assert_eq!(code(&v), "permission_denied", "{v}");

    // A dry run names every use, including the copy at another path, and changes nothing.
    let dry = ws.json(&["admin", "obliterate", "game", "config.ini", "--dry-run"]);
    assert_eq!(dry["dry_run"], true);
    let ids: Vec<&str> = dry["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&secret_id.to_hex().as_str()), "{dry}");
    let secret_uses = &uses(&dry)[ids.iter().position(|i| *i == secret_id.to_hex()).unwrap()];
    assert_eq!(
        secret_uses,
        &[
            (r1, "config.ini".to_owned()),
            (r1, "old/config.ini".to_owned())
        ]
    );
    // Every version of config.ini includes the current one, which is still in main's head.
    assert_eq!(dry["in_use"][0]["branch"], "main", "{dry}");
    assert_eq!(dry["in_use"][0]["path"], "config.ini", "{dry}");
    let mut buf = Vec::new();
    server
        .admin()
        .get_blob_to("game", &secret_id, &mut buf)
        .await
        .unwrap();
    assert_eq!(buf, SECRET);

    // Without a reason, and without --yes when nobody can be asked.
    let (status, v) = ws.json_status(&["admin", "obliterate", "game", &at_r1]);
    assert_eq!((status, code(&v)), (2, "usage"), "{v}");
    let (status, v) = ws.json_status(&[
        "admin",
        "obliterate",
        "game",
        &at_r1,
        "--reason",
        "leaked password",
    ]);
    assert_eq!((status, code(&v)), (2, "usage"), "{v}");

    let done = ws.json(&[
        "admin",
        "obliterate",
        "game",
        &at_r1,
        "--reason",
        "leaked password",
        "--yes",
    ]);
    assert_eq!(done["dry_run"], false);
    assert_eq!(done["objects"].as_array().unwrap().len(), 1, "{done}");
    assert_eq!(done["bytes"], SECRET.len());

    // The content is gone everywhere it would be read.
    let (status, v) = ws.json_status(&["cat", &at_r1]);
    assert_eq!((status, code(&v)), (1, "obliterated"), "{v}");
    let err = server
        .admin()
        .get_blob_to("game", &secret_id, &mut Vec::new())
        .await
        .unwrap_err();
    match err {
        lode_client::Error::Api(a) => {
            assert_eq!(a.status, 410);
            assert!(a.is(ErrorCode::Obliterated));
        }
        other => panic!("{other:?}"),
    }
    // It cannot be stored again, by upload or by commit.
    let err = server
        .admin()
        .put_blob("game", &secret_id, std::io::Cursor::new(SECRET.to_vec()))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, lode_client::Error::Api(a) if a.is(ErrorCode::Obliterated)),
        "{err:?}"
    );
    ws.write("again.ini", SECRET);
    ws.json(&["add", "again.ini"]);
    let (status, v) = ws.json_status(&["commit", "-m", "Oops"]);
    assert_eq!((status, code(&v)), (1, "obliterated"), "{v}");
    assert_eq!(v["error"]["details"]["paths"][0], "again.ini");
    ws.json(&["revert", "again.ini"]);
    ws.remove("again.ini");

    // History is untouched.
    let log = ws.json(&["log", "config.ini"]);
    let revs: Vec<u64> = log["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_u64().unwrap())
        .collect();
    assert_eq!(revs, [r2, r1]);

    // The record says what, who and why.
    let list = ws.json(&["admin", "obliterated", "game"]);
    let rec = &list["obliterated"][0];
    assert_eq!(rec["id"], secret_id.to_hex());
    assert_eq!(rec["path"], "config.ini");
    assert_eq!(rec["reason"], "leaked password");
    assert_eq!(rec["by"], "admin");

    // A repeat finds nothing left, and fsck is clean.
    let again = ws.json(&[
        "admin",
        "obliterate",
        "game",
        &at_r1,
        "--reason",
        "again",
        "--yes",
    ]);
    assert_eq!(again["objects"].as_array().unwrap().len(), 0);
    let fsck = ws.json(&["admin", "fsck", "game"]);
    assert!(fsck.get("problems").is_none(), "{fsck}");

    // Going back to r1 places everything else and leaves the obliterated files out; going
    // forward again brings the fixed file.
    let back = ws.json(&["update", "-r", &r1.to_string()]);
    assert_eq!(
        back["obliterated"],
        serde_json::json!(["config.ini", "old/config.ini"]),
        "{back}"
    );
    assert!(!ws.exists("config.ini"));
    assert!(!ws.exists("old/config.ini"));
    assert_eq!(ws.read("readme.txt"), b"hello\n");
    let status = ws.json(&["status"]);
    assert_eq!(
        status["files"].as_array().map_or(0, Vec::len),
        0,
        "{status}"
    );
    ws.json(&["update"]);
    assert_eq!(ws.read("config.ini"), b"password=\n");

    // A fresh checkout of the head is complete.
    let other = TestWorkspace::new(&server).with_token(server.admin_token());
    let co = other.json(&["checkout", &format!("{}/game", server.url()), "."]);
    assert_eq!(co["obliterated"], serde_json::json!([]), "{co}");
    assert_eq!(other.read("config.ini"), b"password=\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_in_an_active_head_is_refused_until_the_branch_is_archived() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("big.bin", b"\x00huge asset")])
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
    server
        .commit_files("game", "main", &[("big.bin", b"\x00small asset")])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let at_r1 = format!("big.bin@r{r1}");
    let args = [
        "admin",
        "obliterate",
        "game",
        at_r1.as_str(),
        "--reason",
        "too big",
        "--yes",
    ];
    let (status, v) = ws.json_status(&args);
    assert_eq!((status, code(&v)), (1, "conflict"), "{v}");
    assert_eq!(v["error"]["details"]["in_use"][0]["branch"], "feat", "{v}");
    assert_eq!(v["error"]["details"]["in_use"][0]["path"], "big.bin", "{v}");

    let (status, v) = ws.json_status(&["admin", "obliterate", "game", "nothing.txt", "--dry-run"]);
    assert_eq!((status, code(&v)), (1, "not_found"), "{v}");

    let ws2 = TestWorkspace::new(&server).with_token(server.admin_token());
    ws2.json(&[
        "checkout",
        &format!("{}/game", server.url()),
        ".",
        "-b",
        "feat",
    ]);
    ws2.json(&["branch", "archive", "feat"]);
    let done = ws.json(&args);
    assert_eq!(done["objects"].as_array().unwrap().len(), 1, "{done}");
    // Folders select every file under them; what is left is in main's head.
    let (status, v) = ws.json_status(&["admin", "obliterate", "game", "big.bin", "--dry-run"]);
    assert_eq!(status, 0, "{v}");
    assert_eq!(v["in_use"][0]["branch"], "main", "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_stops_at_an_obliterated_version() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("a.txt", b"one\ntwo\n")])
        .await;
    let r2 = server
        .commit_files("game", "main", &[("a.txt", b"one\ntwo\nthree\n")])
        .await;
    let r3 = server
        .commit_files("game", "main", &[("a.txt", b"ONE\ntwo\nthree\n")])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    let before = ws.json(&["blame", "a.txt"]);
    assert_eq!(before["lines"][1]["rev"], r1, "{before}");
    ws.json(&[
        "admin",
        "obliterate",
        "game",
        &format!("a.txt@r{r1}"),
        "--reason",
        "test",
        "--yes",
    ]);
    let after = ws.json(&["blame", "a.txt"]);
    let revs: Vec<u64> = after["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["rev"].as_u64().unwrap())
        .collect();
    assert_eq!(revs, [r3, r2, r2], "{after}");
    assert_eq!(after["truncated"], true, "{after}");
    let (status, v) = ws.json_status(&["blame", &format!("a.txt@r{r1}")]);
    assert_eq!((status, code(&v)), (1, "obliterated"), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gc_finishes_an_interrupted_obliterate_and_fsck_sees_it() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let r1 = server
        .commit_files("game", "main", &[("a.txt", b"secret\n")])
        .await;
    let at_r1 = format!("a.txt@r{r1}");
    server
        .commit_files("game", "main", &[("a.txt", b"fixed\n")])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    server
        .failpoints()
        .arm(lode_server::failpoint::OBLITERATE_AFTER_RECORD, 1);
    let (status, v) = ws.json_status(&[
        "admin",
        "obliterate",
        "game",
        &at_r1,
        "--reason",
        "test",
        "--yes",
    ]);
    assert_ne!(status, 0, "{v}");
    let id = blob_id(&b"secret\n"[..]).unwrap();
    assert!(server
        .state()
        .blobs()
        .contains(lode_core::DepotId(1), &id)
        .await
        .unwrap());
    // Recorded, so it is already refused, though still stored.
    let (_, v) = ws.json_status(&["cat", &at_r1, "--depot", "game"]);
    assert_eq!(code(&v), "obliterated", "{v}");
    let (status, v) = ws.json_status(&["admin", "fsck", "game"]);
    assert_eq!(status, 1, "{v}");
    assert!(
        v["error"]["details"]["problems"][0]
            .as_str()
            .unwrap()
            .contains("obliterated but is still stored"),
        "{v}"
    );
    let gc = ws.json(&["admin", "gc", "game"]);
    assert_eq!(gc["objects_affected"], 1, "{gc}");
    let fsck = ws.json(&["admin", "fsck", "game"]);
    assert!(fsck.get("problems").is_none(), "{fsck}");
}
