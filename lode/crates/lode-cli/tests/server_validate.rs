//! T205 acceptance: the validation module runs the head's scripts on every shelf version,
//! reports them as `validate/<name>` checks and gates submits on `[checks].branches`. CI runs
//! this on Linux and Windows, whose runners have Python 3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use lode_meta::Reads;
use lode_server::PythonCommand;
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::Value;

const CONFIG: &[u8] = br#"[checks]
branches = ["main"]

[validate]
support = ["tools/validate/**"]

[[validate.script]]
name  = "json"
run   = "tools/validate/json_valid.py"
paths = ["Data/**/*.json"]

[[validate.script]]
name    = "slow"
run     = "tools/validate/slow.py"
paths   = ["Slow/**"]
timeout = "1s"

[[validate.script]]
name   = "local"
run    = "tools/validate/json_valid.py"
paths  = ["Local/**"]
server = false
"#;

const JSON_VALID: &[u8] = br#"import json, sys
from common import emit

data = json.load(sys.stdin)
bad = 0
for f in data["files"]:
    if f["action"] == "delete":
        continue
    try:
        with open(f["path"], encoding="utf-8") as fh:
            json.load(fh)
    except ValueError as e:
        emit(f["path"], getattr(e, "lineno", None), str(e).split(":")[0])
        bad += 1
print("phase", data["phase"])
sys.exit(1 if bad else 0)
"#;

const COMMON: &[u8] = br#"import json

def emit(path, line, message):
    print(json.dumps({"path": path, "line": line, "message": message}))
"#;

const SLOW: &[u8] = b"import time\ntime.sleep(30)\n";

fn python() -> PythonCommand {
    PythonCommand::Program(if cfg!(windows) { "python" } else { "python3" }.to_owned())
}

async fn setup() -> (TestServer, TestWorkspace) {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.modules.checks = true;
            c.modules.validate = true;
            c.validate.python = Some(python());
            c.validate.workers = 1;
        }),
    })
    .await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeconfig", CONFIG),
                ("tools/validate/json_valid.py", JSON_VALID),
                ("tools/validate/common.py", COMMON),
                ("tools/validate/slow.py", SLOW),
            ],
        )
        .await;
    let token = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    (server, ws)
}

/// The run of one shelf version once it is done: `(state, results)`.
async fn done(server: &TestServer, shelf: u64, version: u64) -> Vec<Value> {
    for _ in 0..300 {
        let row = server
            .state()
            .meta()
            .read(move |tx| {
                tx.db().query_opt(
                    "SELECT state, results FROM mod_validate_run
                     WHERE shelf_id = ?1 AND shelf_version = ?2",
                    lode_meta::vals![shelf as i64, version as i64],
                    |r| Ok((r.get::<String>(0)?, r.get::<Option<String>>(1)?)),
                )
            })
            .await
            .unwrap();
        if let Some((state, results)) = row {
            if state == "done" {
                return serde_json::from_str(&results.unwrap_or_default()).unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("shelf {shelf} version {version} was not validated in 30 s");
}

fn result<'a>(results: &'a [Value], name: &str) -> &'a Value {
    results
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no result {name}: {results:?}"))
}

fn check<'a>(checks: &'a Value, name: &str) -> &'a Value {
    checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check {name}: {checks}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_shelf_is_held_until_fixed() {
    let (server, ws) = setup().await;
    ws.write("Data/a.json", "{\n  \"a\": 1,\n  \"b\": \n}\n");
    ws.json(&["add", "Data/a.json"]);
    let id = ws.json(&["shelve", "-m", "a", "--no-verify", "--keep"])["shelf"]
        .as_u64()
        .unwrap();

    let results = done(&server, id, 1).await;
    let json = result(&results, "json");
    assert_eq!(json["outcome"], "failed", "{json}");
    assert_eq!(json["problems"][0]["path"], "Data/a.json");
    assert_eq!(json["text"], "phase server");
    // Scripts with nothing to check, or that only run on clients, are not reported.
    assert_eq!(result(&results, "slow")["outcome"], "skipped");
    assert!(results.iter().all(|r| r["name"] != "local"), "{results:?}");

    let checks = ws.json(&["checks", &format!("{id}@1")]);
    let c = check(&checks, "validate/json");
    assert_eq!(c["state"], "failure", "{checks}");
    assert_eq!(c["reporter"]["name"], "lode-validate", "{checks}");
    assert!(
        c["summary"].as_str().unwrap().contains("Data/a.json"),
        "{checks}"
    );

    let (code, v) = ws.json_status(&["shelf", "submit", &format!("{id}@1")]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "checks_required", "{v}");
    assert_eq!(v["error"]["details"]["missing"][0]["name"], "validate/json");

    // The full results over HTTP.
    let (status, body) = get(&server, &format!("/api/v1/depots/game/validate/{id}/1")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "done");
    let all = body["results"].as_array().unwrap();
    assert_eq!(result(all, "json")["problems"][0]["line"], 4, "{body}");

    // Fixed in a new shelf version, it passes and goes in.
    ws.write("Data/a.json", "{ \"a\": 1 }\n");
    let v = ws.json(&[
        "shelve",
        "-m",
        "a",
        "--no-verify",
        "--update",
        &id.to_string(),
    ]);
    assert_eq!(v["version"], 2, "{v}");
    let results = done(&server, id, 2).await;
    assert_eq!(result(&results, "json")["outcome"], "passed");
    let checks = ws.json(&["checks", &format!("{id}@2")]);
    assert_eq!(check(&checks, "validate/json")["state"], "success");
    let v = ws.json(&["shelf", "submit", &format!("{id}@2")]);
    assert!(v["rev"].as_u64().is_some(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_check_write_keeps_the_whole_run_recoverable() {
    let (mut server, ws) = setup().await;
    // Both backends support a partial unique index. Reject the second terminal check
    // inside a publication, after the first check and its event have been written.
    server
        .state()
        .meta()
        .write(|tx| {
            tx.conn().execute(
                "CREATE UNIQUE INDEX test_one_final ON mod_checks_check (depot_id, target_id)
             WHERE state IN ('success', 'failure')",
                lode_meta::vals![],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    ws.write("Data/a.json", "{}");
    ws.write("Slow/b.txt", "b");
    ws.json(&["add", "Data/a.json", "Slow/b.txt"]);
    let id = ws.json(&["shelve", "-m", "two checks", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();

    // With one worker, finishing the next shelf proves the previous attempt returned.
    ws.write("other.txt", "c");
    ws.json(&["add", "other.txt"]);
    let next = ws.json(&["shelve", "-m", "no matching files"])["shelf"]
        .as_u64()
        .unwrap();
    done(&server, next, 1).await;
    let path = format!("/api/v1/depots/game/validate/{id}/1");
    let (status, run) = get(&server, &path).await;
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["state"], "running", "{run}");
    let target = format!("{id}@1");
    let checks = ws.json(&["checks", &target]);
    for name in ["validate/json", "validate/slow"] {
        assert_eq!(check(&checks, name)["state"], "pending", "{checks}");
    }
    let (_, refusal) = ws.json_status(&["shelf", "submit", &target]);
    assert_eq!(refusal["error"]["code"], "checks_required", "{refusal}");

    server
        .state()
        .meta()
        .write(|tx| {
            tx.conn()
                .execute("DROP INDEX test_one_final", lode_meta::vals![])?;
            Ok(())
        })
        .await
        .unwrap();
    server.restart().await;
    let results = done(&server, id, 1).await;
    assert_eq!(result(&results, "json")["outcome"], "passed");
    assert_eq!(result(&results, "slow")["outcome"], "error");
    let checks = ws.json(&["checks", &target]);
    assert_eq!(check(&checks, "validate/json")["state"], "success");
    assert_eq!(check(&checks, "validate/slow")["state"], "failure");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shelf_cannot_bring_its_own_script() {
    let (server, ws) = setup().await;
    // The shelf replaces the script with one that passes everything; the head's runs.
    ws.write("tools/validate/json_valid.py", "import sys\nsys.exit(0)\n");
    ws.write("Data/b.json", "not json\n");
    ws.json(&["add", "Data/b.json"]);
    let id = ws.json(&["shelve", "-m", "b", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();
    let results = done(&server, id, 1).await;
    assert_eq!(result(&results, "json")["outcome"], "failed", "{results:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_script_that_runs_too_long_is_an_error() {
    let (server, ws) = setup().await;
    ws.write("Slow/x.txt", "x\n");
    ws.json(&["add", "Slow/x.txt"]);
    let id = ws.json(&["shelve", "-m", "x", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();
    let results = done(&server, id, 1).await;
    let slow = result(&results, "slow");
    assert_eq!(slow["outcome"], "error", "{slow}");
    let checks = ws.json(&["checks", &format!("{id}@1")]);
    assert_eq!(check(&checks, "validate/slow")["state"], "failure");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_cut_short_by_a_restart_starts_over() {
    let (mut server, ws) = setup().await;
    ws.write("Slow/y.txt", "y\n");
    ws.write("Data/y.json", "{}\n");
    ws.json(&["add", "Slow/y.txt", "Data/y.json"]);
    let id = ws.json(&["shelve", "-m", "y", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();
    // Stop the server while the slow script still runs.
    for _ in 0..100 {
        let state = server
            .state()
            .meta()
            .read(move |tx| {
                tx.db().query_opt(
                    "SELECT state FROM mod_validate_run WHERE shelf_id = ?1",
                    lode_meta::vals![id as i64],
                    |r| r.get::<String>(0),
                )
            })
            .await
            .unwrap();
        if state.as_deref() == Some("running") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.restart().await;
    let results = done(&server, id, 1).await;
    assert_eq!(result(&results, "json")["outcome"], "passed", "{results:?}");
    assert_eq!(result(&results, "slow")["outcome"], "error", "{results:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_python_every_run_reports_an_error() {
    let server = TestServer::start_with(TestServerOptions {
        configure: Box::new(|c| {
            c.modules.checks = true;
            c.modules.validate = true;
            c.validate.python = Some(PythonCommand::Program("no-such-python-here".into()));
        }),
    })
    .await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeconfig", CONFIG),
                ("tools/validate/json_valid.py", JSON_VALID),
                ("tools/validate/common.py", COMMON),
                ("tools/validate/slow.py", SLOW),
            ],
        )
        .await;
    let token = server
        .token_for("admin", &["read", "write", "shelve", "admin"])
        .await;
    let ws = TestWorkspace::new(&server).with_token(&token);
    ws.json(&["checkout", &format!("{}/game", server.url()), "."]);
    ws.write("Data/c.json", "{}\n");
    ws.json(&["add", "Data/c.json"]);
    let id = ws.json(&["shelve", "-m", "c", "--no-verify"])["shelf"]
        .as_u64()
        .unwrap();
    let results = done(&server, id, 1).await;
    let json = result(&results, "json");
    assert_eq!(json["outcome"], "error", "{json}");
    assert!(json.to_string().contains("no-such-python-here"), "{json}");
    let (code, v) = ws.json_status(&["shelf", "submit", &format!("{id}@1")]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "checks_required", "{v}");
}

async fn get(server: &TestServer, path: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", server.url()))
        .bearer_auth(server.admin_token())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}
