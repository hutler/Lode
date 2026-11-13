//! T085 acceptance: `lode init` turns a folder into a depot with one commit of exactly the
//! files its template does not ignore; `--dry-run` changes nothing; an interrupted init
//! finishes the same commit when rerun.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;

use lode_testkit::{TestServer, TestWorkspace};

/// Every file under `root` except `.lode/`, by relative path with `/`.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".lode" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                todo.push(e.path());
            } else {
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

/// An Unreal-style project of 1,000 files, 200 of them in folders the template ignores.
fn unreal_project(ws: &TestWorkspace) -> usize {
    let mut kept = 0;
    for i in 0..1000 {
        let (path, ignored) = match i % 10 {
            0 => (format!("Binaries/Win64/mod{i}.dll"), true),
            1 => (format!("Intermediate/Build/obj{i}.o"), true),
            2 => (format!("Source/Game/file{i}.cpp"), false),
            3 => (format!("Config/Default{i}.ini"), false),
            _ => (format!("Content/Pack{}/Asset{i}.uasset", i % 7), false),
        };
        let body: Vec<u8> = format!("{path} {i}\n")
            .bytes()
            .cycle()
            .take(100 + i)
            .collect();
        ws.write(&path, body);
        if !ignored {
            kept += 1;
        }
    }
    kept
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_commits_exactly_the_files_the_template_keeps() {
    let server = TestServer::start().await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let kept = unreal_project(&ws);
    assert_eq!(kept, 800);

    // A dry run counts the kept files plus the two template files, and writes nothing.
    let plan = ws.json(&["init", "game", "--template", "unreal", "--dry-run"]);
    assert_eq!(plan["files"], 802, "{plan}");
    assert_eq!(
        plan["writes"],
        serde_json::json!([".lodeignore", ".lodeconfig"])
    );
    assert!(!ws.exists(".lodeignore") && !ws.exists(".lode"));
    let (code, _) = ws.json_status(&["log"]);
    assert_ne!(code, 0, "no workspace yet");

    let done = ws.json(&["init", "game", "--template", "unreal"]);
    assert_eq!(done["rev"], 2, "{done}");
    assert_eq!(done["files"], 802);
    assert_eq!(ws.json(&["status"])["files"], serde_json::json!([]));
    // Lock rules from the template apply.
    let st = ws.json(&["status", "--unchanged", "Content/Pack0/Asset7.uasset"]);
    assert_eq!(st["files"][0]["lock_required"], true, "{st}");

    // A second workspace checks out the same tree, without the ignored folders.
    let other = TestWorkspace::new(&server).with_token(server.admin_token());
    other.json(&["checkout", "game", "."]);
    let mut expected = tree(ws.root());
    expected.retain(|p, _| !p.starts_with("Binaries/") && !p.starts_with("Intermediate/"));
    assert_eq!(expected.len(), 802);
    assert_eq!(tree(other.root()), expected);

    // Running it again, or on a depot that has history, is refused.
    let (code, v) = ws.json_status(&["init", "game"]);
    assert_eq!(code, 1, "{v}");
    let fresh = TestWorkspace::new(&server).with_token(server.admin_token());
    fresh.write("a.txt", "a");
    let (code, v) = fresh.json_status(&["init", "game"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (1, Some("depot_not_empty")),
        "{v}"
    );
    let (code, _) = fresh.json_status(&["init", "other", "--template", "cobol"]);
    assert_eq!(code, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_init_finishes_the_same_commit() {
    let server = TestServer::start().await;
    for point in ["commit.uploaded", "commit.sent"] {
        let depot = format!("d-{}", point.replace('.', "-"));
        let ws = TestWorkspace::new(&server).with_token(server.admin_token());
        for i in 0..20 {
            ws.write(&format!("src/f{i}.rs"), format!("fn f{i}() {{}}\n"));
        }
        ws.write("target/debug/app", "binary");
        let out = ws
            .cmd()
            .args(["init", &depot, "--template", "rust"])
            .env("LODE_TEST_CRASH_AT", point)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{point}: the crash point stops it");

        let done = ws.json(&["init", &depot]);
        assert_eq!(done["resumed"], true, "{done}");
        assert_eq!(done["template"], "rust");
        let log = ws.json(&["log"]);
        let revs = log["revisions"].as_array().unwrap();
        assert_eq!(revs.len(), 2, "{point}: r1 and one import: {log}");
        let other = TestWorkspace::new(&server).with_token(server.admin_token());
        other.json(&["checkout", &depot, "."]);
        assert_eq!(tree(other.root()).len(), 22, "{point}");
        assert!(!other.exists("target/debug/app"));
        assert_eq!(ws.json(&["status"])["files"], serde_json::json!([]));
    }
}
