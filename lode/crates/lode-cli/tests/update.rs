//! T025 acceptance: every row of the update table (ARCHITECTURE §10.4), `resolve`, an
//! interrupted update, older revisions, and the access-epoch full sync.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

const LINES: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";

async fn server() -> TestServer {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a.txt", b"apple\n"),
                ("b.txt", b"banana\n"),
                ("lines.txt", LINES.as_bytes()),
                ("art/pic.bin", b"\x00\x01pixels"),
                ("old/only.txt", b"only\n"),
                ("secret/s.txt", b"secret\n"),
                ("secret/m.txt", b"secret too\n"),
            ],
        )
        .await;
    server
}

fn checkout_as(server: &TestServer, token: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(server).with_token(token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws
}

fn checkout(server: &TestServer) -> TestWorkspace {
    checkout_as(server, server.admin_token())
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

fn status(ws: &TestWorkspace) -> BTreeMap<String, String> {
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

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".lode" && dir == base {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(base, &e.path(), out);
            } else {
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmodified_files_are_replaced_and_deleted() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    other.write("a.txt", "apple 2\n");
    other.json(&["rm", "old/only.txt"]);
    other.write("new/n.txt", "new\n");
    other.json(&["add", "new"]);
    other.json(&["commit", "-m", "theirs"]);

    let v = mine.json(&["update"]);
    assert_eq!(
        outcomes(&v),
        map(&[
            ("a.txt", "updated"),
            ("new/n.txt", "added"),
            ("old/only.txt", "deleted")
        ])
    );
    assert_eq!(mine.read("a.txt"), b"apple 2\n");
    assert!(
        !mine.exists("old"),
        "a directory left empty was not removed"
    );
    assert_eq!(mine.read("new/n.txt"), b"new\n");
    assert!(status(&mine).is_empty());
    assert_eq!(snapshot(mine.root()), snapshot(other.root()));
    // Nothing new: nothing changes.
    let v = mine.json(&["update"]);
    assert!(outcomes(&v).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_already_at_the_incoming_content_is_left_alone() {
    let server = server().await;
    let mine = checkout(&server);
    mine.write("a.txt", "mine, committed\n");
    mine.json(&["commit", "-m", "mine"]);
    // The workspace's synced revision is older than its committed a.txt: the delta brings
    // a.txt with the id it already has.
    let v = mine.json(&["update"]);
    assert!(outcomes(&v).is_empty(), "{v}");
    assert_eq!(mine.read("a.txt"), b"mine, committed\n");
    assert!(status(&mine).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_edits_merge_or_conflict() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    other.write("lines.txt", LINES.replace("9\n", "nine\n"));
    other.write("a.txt", "theirs\n");
    other.json(&["commit", "-m", "theirs"]);
    let rev = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;

    mine.write("lines.txt", LINES.replace("1\n", "one\n"));
    mine.write("a.txt", "mine\n");
    let v = mine.json(&["update"]);
    assert_eq!(
        outcomes(&v),
        map(&[("a.txt", "conflicted"), ("lines.txt", "merged")])
    );
    assert_eq!(
        String::from_utf8(mine.read("lines.txt")).unwrap(),
        LINES.replace("1\n", "one\n").replace("9\n", "nine\n")
    );
    assert_eq!(
        String::from_utf8(mine.read("a.txt")).unwrap(),
        format!("<<<<<<< mine\nmine\n=======\ntheirs\n>>>>>>> theirs r{rev}\n")
    );
    assert_eq!(
        status(&mine),
        map(&[("a.txt", "conflicted"), ("lines.txt", "modified")])
    );
    // Conflicts block commit, and a further update.
    let (code, err) = mine.json_status(&["commit", "-m", "x"]);
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("conflicted"))
    );
    let (code, _) = mine.json_status(&["update"]);
    assert_eq!(code, 1);

    // --mine brings back the local version from before the merge.
    mine.json(&["resolve", "a.txt", "--mine"]);
    assert_eq!(mine.read("a.txt"), b"mine\n");
    let v = mine.json(&["commit", "-m", "mine wins"]);
    assert_eq!(v["rev"].as_u64(), Some(rev + 1));
    other.json(&["update"]);
    assert_eq!(other.read("a.txt"), b"mine\n");
    assert_eq!(
        String::from_utf8(other.read("lines.txt")).unwrap(),
        LINES.replace("1\n", "one\n").replace("9\n", "nine\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_conflict_keeps_the_local_file_and_writes_theirs() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    other.write("art/pic.bin", b"\x00theirs".as_slice());
    other.json(&["commit", "-m", "theirs"]);
    let rev = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;

    mine.write("art/pic.bin", b"\x00mine".as_slice());
    let v = mine.json(&["update"]);
    assert_eq!(outcomes(&v), map(&[("art/pic.bin", "conflicted")]));
    assert_eq!(mine.read("art/pic.bin"), b"\x00mine");
    let theirs = format!("art/pic.bin.theirs-r{rev}");
    assert_eq!(mine.read(&theirs), b"\x00theirs");

    // --merged keeps the file as it is now; the .theirs file goes.
    mine.write("art/pic.bin", b"\x00both".as_slice());
    mine.json(&["resolve", "art/pic.bin", "--merged"]);
    assert!(!mine.exists(&theirs));
    mine.json(&["commit", "-m", "both"]);
    assert!(status(&mine).is_empty());
    assert_eq!(
        server
            .admin()
            .file(
                "game",
                rev + 1,
                &lode_core::DepotPath::parse("art/pic.bin").unwrap()
            )
            .await
            .unwrap()
            .id,
        lode_core::blob_id(&b"\x00both"[..]).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletes_against_changes_conflict() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    // There: b.txt deleted, a.txt changed. Here: b.txt changed, a.txt deleted.
    other.json(&["rm", "b.txt"]);
    other.write("a.txt", "changed there\n");
    other.json(&["commit", "-m", "theirs"]);
    mine.write("b.txt", "changed here\n");
    mine.json(&["rm", "a.txt"]);

    let v = mine.json(&["update"]);
    assert_eq!(
        outcomes(&v),
        map(&[("a.txt", "conflicted"), ("b.txt", "conflicted")])
    );
    assert_eq!(mine.read("b.txt"), b"changed here\n");
    assert!(!mine.exists("a.txt"));

    // --theirs: a.txt comes back as changed there; b.txt goes.
    mine.json(&["resolve", "a.txt", "b.txt", "--theirs"]);
    assert_eq!(mine.read("a.txt"), b"changed there\n");
    assert!(!mine.exists("b.txt"));
    assert!(status(&mine).is_empty());
    assert_eq!(snapshot(mine.root()), snapshot(other.root()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kept_local_changes_against_a_delete_become_an_add() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    other.json(&["rm", "b.txt"]);
    other.json(&["commit", "-m", "theirs"]);
    mine.write("b.txt", "keep me\n");
    mine.json(&["update"]);
    mine.json(&["resolve", "b.txt", "--mine"]);
    assert_eq!(status(&mine), map(&[("b.txt", "added")]));
    mine.json(&["commit", "-m", "back"]);
    other.json(&["update"]);
    assert_eq!(other.read("b.txt"), b"keep me\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untracked_file_in_the_way_is_kept() {
    let server = server().await;
    let mine = checkout(&server);
    let other = checkout(&server);
    other.write("x.txt", "theirs\n");
    other.write("same.txt", "same\n");
    other.json(&["add", "x.txt", "same.txt"]);
    other.json(&["commit", "-m", "theirs"]);
    mine.write("x.txt", "mine\n");
    mine.write("same.txt", "same\n");

    let v = mine.json(&["update"]);
    assert_eq!(
        outcomes(&v),
        map(&[("same.txt", "added"), ("x.txt", "conflicted")])
    );
    assert_eq!(mine.read("x.txt"), b"mine\n");
    assert_eq!(status(&mine), map(&[("x.txt", "conflicted")]));
    mine.json(&["resolve", "x.txt", "--theirs"]);
    assert_eq!(mine.read("x.txt"), b"theirs\n");
    assert!(status(&mine).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_update_converges() {
    let server = server().await;
    let a = checkout(&server);
    let b = checkout(&server);
    let other = checkout(&server);
    let mut files: Vec<(String, String)> = (0..12)
        .map(|i| (format!("many/f{i:02}.txt"), format!("file {i}\n")))
        .collect();
    files.push(("lines.txt".into(), LINES.replace("9\n", "nine\n")));
    for (p, c) in &files {
        other.write(p, c);
    }
    other.json(&["add", "many"]);
    other.json(&["rm", "old/only.txt"]);
    other.json(&["commit", "-m", "many"]);
    for ws in [&a, &b] {
        ws.write("lines.txt", LINES.replace("1\n", "one\n"));
        ws.write("art/pic.bin", b"\x00local".as_slice());
    }
    let other_bin = b"\x00theirs";
    other.write("art/pic.bin", other_bin.as_slice());
    other.json(&["commit", "-m", "pic"]);

    let full = a.json(&["update"]);
    let out = b
        .cmd()
        .env("LODE_TEST_EXIT_AFTER_FILES", "3")
        .args(["update"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resumed = b.json(&["update"]);
    assert_eq!(snapshot(a.root()), snapshot(b.root()));
    assert_eq!(status(&a), status(&b));
    assert_eq!(full["rev"], resumed["rev"]);
    assert_eq!(
        status(&b).get("art/pic.bin").map(String::as_str),
        Some("conflicted")
    );
    assert_eq!(
        status(&b).get("lines.txt").map(String::as_str),
        Some("modified")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn going_back_needs_clean_files() {
    let server = server().await;
    let start = server
        .admin()
        .branch("game", "main")
        .await
        .unwrap()
        .head_rev;
    let mine = checkout(&server);
    mine.write("a.txt", "newer\n");
    mine.json(&["commit", "-m", "newer"]);
    mine.json(&["update"]);
    mine.write("a.txt", "local edit\n");
    let (code, err) = mine.json_status(&["update", "-r", &start.to_string()]);
    assert_eq!(
        (code, err["error"]["code"].as_str()),
        (1, Some("local_changes"))
    );
    mine.json(&["revert", "a.txt"]);
    let v = mine.json(&["update", "-r", &start.to_string()]);
    assert_eq!(outcomes(&v), map(&[("a.txt", "updated")]));
    assert_eq!(mine.read("a.txt"), b"apple\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acl_change_hides_files_on_the_next_update() {
    let server = server().await;
    let dana = server.create_user("dana", &["read", "write"]).await;
    server
        .acl(
            "game",
            "dana",
            "*",
            "**",
            AclEffect::Allow,
            AccessLevel::Write,
        )
        .await;
    let ws = checkout_as(&server, &dana);
    assert!(ws.exists("secret/s.txt"));
    ws.write("secret/m.txt", "my edit\n");
    server
        .acl(
            "game",
            "dana",
            "*",
            "secret/**",
            AclEffect::Deny,
            AccessLevel::Read,
        )
        .await;

    let v = ws.json(&["update"]);
    assert_eq!(v["full"], true);
    assert_eq!(
        outcomes(&v),
        map(&[("secret/m.txt", "kept"), ("secret/s.txt", "deleted")])
    );
    assert!(!ws.exists("secret/s.txt"));
    assert_eq!(ws.read("secret/m.txt"), b"my edit\n");
    assert_eq!(status(&ws), map(&[("secret/m.txt", "untracked")]));
    // The next update is a delta again.
    let v = ws.json(&["update"]);
    assert_eq!(v["full"], false);
}
