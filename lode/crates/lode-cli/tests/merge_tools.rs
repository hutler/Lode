//! T164 acceptance: `lode resolve --tool` and `lode merge-tool list`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use lode_testkit::{TestServer, TestWorkspace};

/// A test tool: writes `result` into its 4th argument (`{merged}`) and exits with `code`.
fn tool(dir: &Path, name: &str, result: Option<&str>, code: i32) -> Vec<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(format!("{name}.sh"));
        let write = result
            .map(|r| format!("printf '%s' '{r}' > \"$4\"\n"))
            .unwrap_or_default();
        std::fs::write(&p, format!("#!/bin/sh\n{write}exit {code}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![
            p.to_string_lossy().into_owned(),
            "{base}".into(),
            "{mine}".into(),
            "{theirs}".into(),
            "{merged}".into(),
        ]
    }
    #[cfg(windows)]
    {
        let p = dir.join(format!("{name}.cmd"));
        let write = result
            .map(|r| format!("<nul set /p =\"{r}\"> \"%~4\"\r\n"))
            .unwrap_or_default();
        std::fs::write(&p, format!("@echo off\r\n{write}exit /b {code}\r\n")).unwrap();
        vec![
            "cmd".into(),
            "/c".into(),
            p.to_string_lossy().into_owned(),
            "{base}".into(),
            "{mine}".into(),
            "{theirs}".into(),
            "{merged}".into(),
        ]
    }
}

fn settings(ws: &TestWorkspace, tools: &[(&str, Vec<String>, bool)]) {
    let mut text = String::new();
    for (name, cmd, trust) in tools {
        let list: Vec<String> = cmd.iter().map(|a| format!("{:?}", a)).collect();
        text.push_str(&format!(
            "[merge_tools.{name}]\ncommand = [{}]\ntrust_exit_code = {trust}\n",
            list.join(", ")
        ));
    }
    std::fs::create_dir_all(ws.config_dir()).unwrap();
    std::fs::write(ws.config_dir().join("settings.toml"), text).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicts_are_resolved_by_the_tools_people_set_up() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (
                    ".lodeconfig",
                    b"[merge.tools]\n\"**/*.uasset\" = \"fake\"\n\"**/*.umap\" = \"stubborn\"\n",
                ),
                ("a.uasset", b"\x00base"),
                ("b.uasset", b"\x00base"),
                ("m.umap", b"\x00base"),
                ("plain.bin", b"\x00base"),
            ],
        )
        .await;
    let url = format!("{}/game", server.url());
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    ws.json(&["checkout", &url, "."]);
    for f in ["a.uasset", "b.uasset", "m.umap", "plain.bin"] {
        ws.write(f, b"\x00mine");
    }
    server
        .commit_files(
            "game",
            "main",
            &[
                ("a.uasset", b"\x00theirs"),
                ("b.uasset", b"\x00theirs"),
                ("m.umap", b"\x00theirs"),
                ("plain.bin", b"\x00theirs"),
            ],
        )
        .await;
    let tools_dir: PathBuf = ws.home().join("tools");
    std::fs::create_dir_all(&tools_dir).unwrap();

    // Nothing runs during the update; conflicts name their tool.
    ws.json(&["update"]);
    let list = ws.json(&["merge-tool", "list"]);
    let names: Vec<(String, bool)> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_owned(),
                t["configured"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![("fake".into(), false), ("stubborn".into(), false)],
        "{list}"
    );

    // A tool without a command is reported with what to add.
    let (code, v) = ws.json_status(&["resolve", "a.uasset", "--tool"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "merge_tool_unknown", "{v}");
    assert!(v["error"]["details"]["snippet"]
        .as_str()
        .unwrap()
        .contains("[merge_tools.fake]"));

    settings(
        &ws,
        &[
            ("fake", tool(&tools_dir, "fake", Some("MERGED"), 0), true),
            ("stubborn", tool(&tools_dir, "stubborn", None, 0), false),
            ("broken", tool(&tools_dir, "broken", Some("X"), 3), true),
        ],
    );
    // A failing tool leaves the conflict.
    let v = ws.json(&["resolve", "b.uasset", "--tool", "broken"]);
    assert_eq!(v["resolved"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["failed"][0]["path"], "b.uasset");
    // Without paths: every conflict with a tool. `stubborn` exits 0 but changes nothing,
    // which does not count without trust_exit_code.
    let v = ws.json(&["resolve", "--tool"]);
    let resolved: Vec<&str> = v["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(resolved, vec!["a.uasset", "b.uasset"], "{v}");
    assert_eq!(v["failed"][0]["path"], "m.umap", "{v}");
    assert_eq!(v["skipped"], serde_json::json!(["plain.bin"]));
    assert_eq!(ws.read("a.uasset"), b"MERGED");
    let st = ws.json(&["status"]);
    let conflicted: Vec<&str> = st["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["status"] == "conflicted")
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(conflicted, vec!["m.umap", "plain.bin"], "{st}");
    let list = ws.json(&["merge-tool", "list"]);
    assert_eq!(list["tools"].as_array().unwrap().len(), 3, "{list}");
}
