//! T071 acceptance: command-line support for editor integrations. `status --unchanged` and
//! `--remote`, `cat -o`, `--paths-from` and absolute paths.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_proto::admin::{AccessLevel, AclEffect};
use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

const CONFIG: &[u8] = b"[locks]\nrequired = [\"**/*.uasset\"]\n";

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
                (".lodeconfig", CONFIG),
                ("Content/Hero.uasset", b"hero 1"),
                ("Content/Other.uasset", b"other 1"),
                ("Config/a.ini", b"a=1\n"),
                ("notes.txt", b"notes\n"),
            ],
        )
        .await;
    let ana = server.create_user("ana", &["read", "write", "lock"]).await;
    let bob = server.create_user("bob", &["read", "write", "lock"]).await;
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

fn entry(list: &Value, path: &str) -> Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .cloned()
        .unwrap_or(Value::Null)
}

fn paths(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_for_named_files_and_the_server() {
    let t = team().await;
    let ana = checkout(&t.server, &t.ana);
    ana.json(&["lock", "Content/Hero.uasset"]);
    ana.json(&["changelist", "new", "ui"]);
    ana.write("Config/a.ini", "a=2\n");
    ana.json(&["changelist", "move", "ui", "Config/a.ini"]);

    // Every named file, changed or not, with its lock state.
    let st = ana.json(&["status", "--unchanged", "Content", "Config/a.ini"]);
    let files = &st["files"];
    assert_eq!(
        paths(files),
        [
            "Config/a.ini",
            "Content/Hero.uasset",
            "Content/Other.uasset"
        ]
    );
    let hero = entry(files, "Content/Hero.uasset");
    assert_eq!(hero["status"], "unchanged");
    assert_eq!(hero["locked"], true);
    assert_eq!(hero["lock_required"], true);
    let other = entry(files, "Content/Other.uasset");
    assert_eq!(
        (other["locked"].clone(), other["lock_required"].clone()),
        (false.into(), true.into())
    );
    let ini = entry(files, "Config/a.ini");
    assert_eq!(ini["status"], "modified");
    assert_eq!(ini["changelist"], "ui");
    assert_eq!(ini["lock_required"], false);
    // Without --unchanged, only changes.
    assert_eq!(
        paths(&ana.json(&["status", "Content", "Config"])["files"]),
        ["Config/a.ini"]
    );
    assert!(ana.json(&["status"]).get("remote").is_none());

    // Ana commits her change: her own commit after her revision is not "newer".
    ana.json(&["commit", "-m", "ini", "Config/a.ini"]);

    // Bob commits a newer notes.txt and locks another asset.
    let bob = checkout(&t.server, &t.bob);
    bob.write("notes.txt", "newer\n");
    bob.json(&["commit", "-m", "notes", "notes.txt"]);
    bob.json(&["lock", "Content/Other.uasset"]);
    t.server.requests().clear();
    let st = ana.json(&["status", "--remote"]);
    assert_eq!(t.server.requests().count("GET", "/manifest"), 1);
    assert_eq!(t.server.requests().count("GET", "/locks"), 1);
    let remote = &st["remote"];
    assert!(remote["head_rev"].as_u64().unwrap() > st["rev"].as_u64().unwrap());
    assert_eq!(
        paths(&remote["files"]),
        ["Content/Hero.uasset", "Content/Other.uasset", "notes.txt"]
    );
    let notes = entry(&remote["files"], "notes.txt");
    assert_eq!(notes["newer"], true);
    assert_eq!(notes["locked_by"], Value::Null);
    let other = entry(&remote["files"], "Content/Other.uasset");
    assert_eq!(other["locked_by"]["name"], "bob");
    assert_eq!(other["mine"], false);
    assert_eq!(other["newer"], false);
    let hero = entry(&remote["files"], "Content/Hero.uasset");
    assert_eq!(hero["locked_by"]["name"], "ana");
    assert_eq!(hero["mine"], true);

    // Scoped to a folder, the rest of the depot is left out.
    let st = ana.json(&["status", "--remote", "Content"]);
    assert_eq!(
        paths(&st["remote"]["files"]),
        ["Content/Hero.uasset", "Content/Other.uasset"]
    );

    // Once up to date there is nothing newer, and no manifest request is needed.
    ana.json(&["update"]);
    t.server.requests().clear();
    let st = ana.json(&["status", "--remote", "notes.txt"]);
    assert_eq!(t.server.requests().count("GET", "/manifest"), 0);
    assert_eq!(st["remote"]["files"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_to_a_file() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let data: Vec<u8> = (0..=255u8)
        .cycle()
        .take(70_000)
        .chain(*b"\r\n\0end")
        .collect();
    let rev = server
        .commit_files("game", "main", &[("bin/data.bin", &data)])
        .await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    let out = ws
        .cmd()
        .args(["cat", &format!("bin/data.bin@{rev}"), "-o", "copy.bin"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "nothing goes to standard output");
    assert_eq!(ws.read("copy.bin"), data);

    let v = ws.json(&["cat", "bin/data.bin", "-o", "copy2.bin"]);
    assert_eq!(v["size"], data.len() as u64);
    assert_eq!(v["rev"], rev);
    assert!(v.get("content").is_none() && v.get("content_base64").is_none());
    assert!(v["output"].as_str().unwrap().ends_with("copy2.bin"));
    assert_eq!(ws.read("copy2.bin"), data);
    // No temporary file is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(ws.root())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".lode-cat-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paths_from_a_file_and_absolute_paths() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let names = ["a.txt", "b.txt", "dir/c d.txt", "e.txt", "f.txt"];
    let files: Vec<(&str, &[u8])> = names.iter().map(|n| (*n, &b"1\n"[..])).collect();
    server.commit_files("game", "main", &files).await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    for n in names {
        ws.write(n, "2\n");
    }

    // Three of five changed files, with a byte-order mark and \r\n line ends.
    let list = ws.home().join("list.txt");
    std::fs::write(&list, "\u{feff}a.txt\r\ndir/c d.txt\r\n\r\ne.txt\r\n").unwrap();
    let c = ws.json(&[
        "commit",
        "-m",
        "three",
        "--paths-from",
        list.to_str().unwrap(),
    ]);
    assert!(c["rev"].as_u64().is_some(), "{c}");
    assert_eq!(paths(&ws.json(&["status"])["files"]), ["b.txt", "f.txt"]);

    // From standard input.
    let out = ws
        .cmd()
        .args(["--json", "commit", "-m", "one", "--paths-from", "-"])
        .write_stdin("b.txt\n")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(paths(&ws.json(&["status"])["files"]), ["f.txt"]);

    // An empty list is refused rather than read as "everything".
    let empty = ws.home().join("empty.txt");
    std::fs::write(&empty, "\n").unwrap();
    let (code, v) = ws.json_status(&["commit", "-m", "x", "--paths-from", empty.to_str().unwrap()]);
    assert_eq!(code, 2, "{v}");
    let (code, _) = ws.json_status(&["log", "--paths-from", list.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert_eq!(paths(&ws.json(&["status"])["files"]), ["f.txt"]);

    // An absolute path written with forward slashes.
    let abs = ws.root().join("f.txt").to_string_lossy().replace('\\', "/");
    assert_eq!(paths(&ws.json(&["status", &abs])["files"]), ["f.txt"]);
    let abs_a = ws.root().join("a.txt").to_string_lossy().replace('\\', "/");
    let st = ws.json(&["status", "--unchanged", &abs_a]);
    assert_eq!(entry(&st["files"], "a.txt")["status"], "unchanged");
}
