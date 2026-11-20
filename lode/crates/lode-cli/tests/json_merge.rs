//! T163 acceptance: JSON files merge key by key.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_keys_added_to_a_uproject_merge_on_update() {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    let base = "{\n\t\"FileVersion\": 3,\n\t\"Modules\": []\n}\n";
    server
        .commit_files("game", "main", &[("Game.uproject", base.as_bytes())])
        .await;
    let url = format!("{}/game", server.url());
    let a = TestWorkspace::new(&server).with_token(server.admin_token());
    a.json(&["checkout", &url, "."]);
    let b = TestWorkspace::new(&server).with_token(server.admin_token());
    b.json(&["checkout", &url, "."]);

    a.write(
        "Game.uproject",
        "{\n\t\"FileVersion\": 3,\n\t\"EngineAssociation\": \"5.8\",\n\t\"Modules\": []\n}\n",
    );
    a.json(&["commit", "-m", "engine"]);
    b.write(
        "Game.uproject",
        "{\n\t\"FileVersion\": 3,\n\t\"Modules\": [],\n\t\"Plugins\": [{\"Name\": \"Lode\"}]\n}\n",
    );
    let u = b.json(&["update"]);
    assert!(
        u["files"].to_string().contains("merged") || u.to_string().contains("merged"),
        "{u}"
    );
    assert_eq!(
        String::from_utf8(b.read("Game.uproject")).unwrap(),
        "{\n\t\"FileVersion\": 3,\n\t\"EngineAssociation\": \"5.8\",\n\t\"Modules\": [],\n\t\"Plugins\": [\n\t\t{\n\t\t\t\"Name\": \"Lode\"\n\t\t}\n\t]\n}\n"
    );
    assert!(b.json(&["status"])["files"]
        .to_string()
        .contains("modified"));

    // With `[merge] json = []` the same edits conflict.
    let cfg = b"[merge]\njson = []\n";
    a.json(&["update"]);
    a.write(".lodeconfig", cfg);
    a.json(&["add", ".lodeconfig"]);
    a.json(&["commit", "-m", "cfg"]);
    b.json(&["revert", "--all"]);
    b.json(&["update"]);
    a.write(
        "Game.uproject",
        "{\n\t\"FileVersion\": 4,\n\t\"EngineAssociation\": \"5.8\",\n\t\"Modules\": []\n}\n",
    );
    a.json(&["commit", "-m", "v4"]);
    b.write(
        "Game.uproject",
        "{\n\t\"FileVersion\": 3,\n\t\"EngineAssociation\": \"5.9\",\n\t\"Modules\": []\n}\n",
    );
    let u = b.json(&["update"]);
    assert!(u.to_string().contains("conflicted"), "{u}");
}
