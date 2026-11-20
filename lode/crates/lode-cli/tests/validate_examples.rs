//! T206 acceptance: the scripts in `examples/validate/`, end to end: on the client before a
//! shelve, and on the server before a submit. Needs Python 3.11 (CI runners have it).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lode_meta::Reads;
use lode_server::PythonCommand;
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::Value;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/validate")
}

/// The example's files as `(depot path, content)`, without its README.
fn example_files() -> Vec<(String, Vec<u8>)> {
    let root = examples();
    let mut out = vec![(
        ".lodeconfig".to_owned(),
        std::fs::read(root.join(".lodeconfig")).unwrap(),
    )];
    for e in std::fs::read_dir(root.join("tools/validate")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        out.push((
            format!("tools/validate/{name}"),
            std::fs::read(e.path()).unwrap(),
        ));
    }
    out
}

async fn server() -> TestServer {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| {
            c.modules.checks = true;
            c.modules.validate = true;
            c.validate.python = Some(PythonCommand::Program(python.to_owned()));
        }),
    })
    .await;
    server.create_depot("game").await;
    let files = example_files();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    server.commit_files("game", "main", &refs).await;
    server
}

async fn server_results(server: &TestServer, shelf: u64, version: u64) -> Vec<Value> {
    for _ in 0..300 {
        let row = server
            .state()
            .meta()
            .read(move |tx| {
                tx.db().query_opt(
                    "SELECT results FROM mod_validate_run
                     WHERE shelf_id = ?1 AND shelf_version = ?2 AND state = 'done'",
                    lode_meta::vals![shelf as i64, version as i64],
                    |r| r.get::<Option<String>>(0),
                )
            })
            .await
            .unwrap();
        if let Some(text) = row {
            return serde_json::from_str(&text.unwrap_or_default()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("shelf {shelf} version {version} was not validated");
}

fn outcome<'a>(results: &'a [Value], name: &str) -> &'a Value {
    &results.iter().find(|r| r["name"] == name).unwrap()["outcome"]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_examples_hold_bad_content_on_the_client_and_the_server() {
    let server = server().await;
    let token = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);

    ws.write("Content/Textures/rock.png", vec![0u8; 1024]);
    ws.write("Content/Sounds/S_big.wav", vec![0u8; 33 * 1024 * 1024]);
    ws.write("Data/items.json", "{ \"sword\": 1, }\n");
    ws.json(&[
        "add",
        "Content/Textures/rock.png",
        "Content/Sounds/S_big.wav",
        "Data/items.json",
    ]);

    // On the client.
    let (code, v) = ws.json_status(&["validate", "--trust"]);
    assert_eq!(code, 1, "{v}");
    let results = v["error"]["details"]["results"].as_array().unwrap();
    assert_eq!(outcome(results, "json"), "failed", "{v}");
    assert_eq!(outcome(results, "asset-names"), "failed", "{v}");
    assert_eq!(outcome(results, "max-size"), "failed", "{v}");
    let text = v.to_string();
    assert!(text.contains("start with T_"), "{text}");
    assert!(text.contains("over the 32 MiB limit"), "{text}");

    // Committing directly is not allowed on main; shelving runs the scripts first.
    let (code, v) = ws.json_status(&["commit", "-m", "x"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "shelf_required", "{v}");
    let (code, v) = ws.json_status(&["shelve", "-m", "x", "--keep"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "validation_failed", "{v}");

    // Skipped on the client, the server holds it.
    let id = ws.json(&["shelve", "-m", "x", "--keep", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();
    let results = server_results(&server, id, 1).await;
    assert_eq!(outcome(&results, "json"), "failed", "{results:?}");
    let (code, v) = ws.json_status(&["shelf", "submit", &format!("{id}@1")]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "checks_required", "{v}");

    // Fixed, it passes everywhere and goes in.
    ws.json(&["rm", "--force", "Content/Textures/rock.png"]);
    ws.write("Content/Textures/T_rock.png", vec![0u8; 1024]);
    ws.json(&["add", "Content/Textures/T_rock.png"]);
    ws.write("Content/Sounds/S_big.wav", vec![0u8; 1024]);
    ws.write("Data/items.json", "{ \"sword\": 1 }\n");
    let v = ws.json(&["validate"]);
    assert_eq!(v["passed"], true, "{v}");
    let v = ws.json(&["shelve", "-m", "x", "--update", &id.to_string()]);
    let version = v["version"].as_u64().unwrap();
    let results = server_results(&server, id, version).await;
    for name in ["json", "asset-names", "max-size"] {
        assert_eq!(outcome(&results, name), "passed", "{results:?}");
    }
    let v = ws.json(&["shelf", "submit", &format!("{id}@{version}")]);
    assert!(v["rev"].as_u64().is_some(), "{v}");
}
