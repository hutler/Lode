//! T202 acceptance: validation scripts before `lode shelve` and `lode commit`, and
//! `lode validate`. CI runs this on Linux and Windows, whose runners have Python 3; a missing
//! Python fails these tests rather than skipping them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

const CONFIG: &[u8] = br#"[validate]
python  = "3.8"
support = ["tools/validate/**"]

[[validate.script]]
name  = "json"
run   = "tools/validate/json_valid.py"
paths = ["Data/**/*.json"]

[[validate.script]]
name  = "deleted"
run   = "tools/validate/deleted.py"
paths = ["Gone/**"]

[[validate.script]]
name  = "broken"
run   = "tools/validate/broken.py"
paths = ["Broken/**"]
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
print("checked", len(data["files"]), "phase", data["phase"])
sys.exit(1 if bad else 0)
"#;

const COMMON: &[u8] = br#"import json

def emit(path, line, message):
    print(json.dumps({"path": path, "line": line, "message": message}))
"#;

const DELETED: &[u8] = br#"import json, os, sys
data = json.load(sys.stdin)
for f in data["files"]:
    assert f["action"] == "delete", f
    assert not os.path.exists(f["path"]), f["path"]
"#;

const BROKEN: &[u8] = b"import sys\nsys.stderr.write('cannot read config\\n')\nsys.exit(3)\n";

async fn setup() -> (TestServer, TestWorkspace) {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[
                (".lodeconfig", CONFIG),
                ("tools/validate/json_valid.py", JSON_VALID),
                ("tools/validate/common.py", COMMON),
                ("tools/validate/deleted.py", DELETED),
                ("tools/validate/broken.py", BROKEN),
                ("Gone/old.txt", b"old\n"),
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

fn err(v: &Value) -> &Value {
    &v["error"]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripts_run_before_shelve_and_commit() {
    let (_server, ws) = setup().await;
    ws.write("Data/a.json", "{\n  \"a\": 1,\n  \"b\": \n}\n");
    ws.json(&["add", "Data/a.json"]);

    // Not trusted yet, and nobody to ask.
    let (code, v) = ws.json_status(&["shelve", "-m", "a"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(err(&v)["code"], "validation_untrusted", "{v}");
    assert_eq!(
        err(&v)["details"]["scripts"],
        serde_json::json!(["tools/validate/json_valid.py"])
    );

    // `lode validate --trust` trusts and runs them.
    let (code, v) = ws.json_status(&["validate", "--trust"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(err(&v)["code"], "validation_failed", "{v}");
    let results = &err(&v)["details"]["results"];
    let json = results
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "json")
        .unwrap();
    assert_eq!(json["outcome"], "failed");
    assert_eq!(json["problems"][0]["path"], "Data/a.json");
    assert_eq!(json["problems"][0]["line"], 4);
    assert_eq!(json["text"], "checked 1 phase manual");
    assert_eq!(
        results
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "deleted")
            .unwrap()["outcome"],
        "skipped"
    );
    let settings = std::fs::read_to_string(ws.config_dir().join("settings.toml")).unwrap();
    assert!(
        settings.contains("tools/validate/json_valid.py@"),
        "{settings}"
    );

    // Shelve and commit now run it and stop.
    let (code, v) = ws.json_status(&["shelve", "-m", "a"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(err(&v)["code"], "validation_failed", "{v}");
    let out = ws.cmd().args(["commit", "-m", "a"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("json failed:"), "{stderr}");
    assert!(stderr.contains("Data/a.json"), "{stderr}");
    assert!(stderr.contains("line 4:"), "{stderr}");
    assert!(stderr.contains("nothing was committed"), "{stderr}");

    // --no-verify skips it; the shelf goes up.
    let v = ws.json(&["shelve", "-m", "a", "--no-verify"]);
    assert!(v["shelf"].as_u64().is_some(), "{v}");

    // A valid file and a file no script looks at commit fine.
    ws.write("Data/b.json", "{\"b\": [1, 2]}\n");
    ws.write("notes.txt", "hello\n");
    ws.json(&["add", "Data/b.json", "notes.txt"]);
    let out = ws.cmd().args(["commit", "-m", "b"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Checking json (1 file)"), "{stderr}");
    assert!(stderr.contains("Validation passed: json."), "{stderr}");

    // Deleted files are listed but absent.
    ws.json(&["rm", "Gone/old.txt"]);
    let v = ws.json(&["validate"]);
    let deleted = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "deleted")
        .unwrap()
        .clone();
    assert_eq!(deleted["outcome"], "passed", "{v}");
    assert_eq!(deleted["files"], 1);
    ws.json(&["commit", "-m", "gone"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_script_asks_again_and_a_broken_one_stops() {
    let (server, ws) = setup().await;
    ws.json(&["validate", "--trust"]);
    server
        .commit_files(
            "game",
            "main",
            &[("tools/validate/json_valid.py", b"import sys\nsys.exit(0)\n")],
        )
        .await;
    ws.json(&["update"]);
    ws.write("Data/c.json", "{}\n");
    ws.json(&["add", "Data/c.json"]);
    let (code, v) = ws.json_status(&["commit", "-m", "c"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(err(&v)["code"], "validation_untrusted", "{v}");
    ws.json(&["validate", "--trust"]);
    ws.json(&["commit", "-m", "c"]);

    // A script that breaks is not "bad content", and it still stops the commit.
    ws.write("Broken/x.txt", "x\n");
    ws.json(&["add", "Broken/x.txt"]);
    let (code, v) = ws.json_status(&["commit", "-m", "x"]);
    assert_eq!(code, 1, "{v}");
    let r = err(&v)["details"]["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "broken")
        .unwrap()
        .clone();
    assert_eq!(r["outcome"], "error", "{r}");
    assert_eq!(r["reason"], "exit code 3");
    assert!(r["text"].as_str().unwrap().contains("cannot read config"));

    // `--script` runs one.
    let v = ws.json_status(&["validate", "--script", "json"]).1;
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "{v}");
    let (code, v) = ws.json_status(&["validate", "--script", "nope"]);
    assert_eq!(code, 2, "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_python_warns_or_refuses() {
    let (_server, ws) = setup().await;
    ws.json(&["validate", "--trust"]);
    ws.write("Data/d.json", "not json\n");
    ws.json(&["add", "Data/d.json"]);

    let out = ws
        .cmd()
        .env("LODE_PYTHON", "no-such-python-anywhere")
        .args(["--json", "shelve", "-m", "d", "--keep"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("need Python 3.8 or newer"), "{stderr}");
    assert!(stderr.contains("no-such-python-anywhere"), "{stderr}");

    let settings_file = ws.config_dir().join("settings.toml");
    let mut settings = std::fs::read_to_string(&settings_file).unwrap();
    // Lode wrote `[validate.trusted]`; the setting goes into `[validate]` before it.
    let at = settings.find("[validate").unwrap();
    settings.insert_str(at, "[validate]\nmissing_python = \"refuse\"\n\n");
    std::fs::write(&settings_file, settings).unwrap();
    let out = ws
        .cmd()
        .env("LODE_PYTHON", "no-such-python-anywhere")
        .args(["--json", "commit", "-m", "d"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(err(&v)["code"], "python_missing", "{v}");
}
