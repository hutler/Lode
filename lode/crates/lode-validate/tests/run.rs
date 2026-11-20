//! The script contract against a real Python. CI runs this on Linux and Windows, where the
//! runners have Python 3; a missing Python fails the test rather than skipping it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use lode_validate::{
    default_candidates, find_python, input_files, matching, parse_output, run, FindError, Input,
    InputFile, Level, Outcome, Python,
};

async fn python() -> Python {
    match find_python(&default_candidates(), Some((3, 8, 0))).await {
        Ok(p) => p,
        Err(e) => panic!("these tests need Python 3.8 or newer: {e:?}"),
    }
}

fn input(files: &[&str]) -> Input {
    Input {
        version: 1,
        phase: "manual".into(),
        depot: "game".into(),
        branch: "main".into(),
        files: files
            .iter()
            .map(|f| InputFile {
                path: (*f).into(),
                action: "edit".into(),
                from: None,
            })
            .collect(),
    }
}

fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

const SECS: Duration = Duration::from_secs(30);

#[tokio::test]
async fn pass_fail_text_and_error() {
    let py = python().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let ok = script(
        root,
        "t/ok.py",
        "import sys, json\nd = json.load(sys.stdin)\nprint('checked', len(d['files']))\n",
    );
    let r = run(&py, "ok", root, &ok, &input(&["a.json"]), SECS).await;
    assert_eq!(r.outcome, Outcome::Passed, "{r:?}");
    assert_eq!(r.text.as_deref(), Some("checked 1"));
    assert_eq!(r.files, 1);

    let bad = script(
        root,
        "t/bad.py",
        r#"import json
print(json.dumps({"path": "a.json", "line": 3, "message": "expected ,"}))
print(json.dumps({"path": "b.json", "level": "warning", "message": "slow"}))
print("plain words")
raise SystemExit(1)
"#,
    );
    let r = run(&py, "bad", root, &bad, &input(&["a.json", "b.json"]), SECS).await;
    assert_eq!(r.outcome, Outcome::Failed, "{r:?}");
    assert_eq!(r.problems.len(), 2);
    assert_eq!(r.problems[0].line, Some(3));
    assert_eq!(r.problems[0].level, Level::Error);
    assert_eq!(r.problems[1].level, Level::Warning);
    assert_eq!(r.text.as_deref(), Some("plain words"));
    assert_eq!(r.summary(200), "a.json:3: expected ,");

    let plain = script(
        root,
        "t/plain.py",
        "import sys\nprint('just text')\nsys.exit(1)\n",
    );
    let r = run(&py, "plain", root, &plain, &input(&[]), SECS).await;
    assert_eq!(r.outcome, Outcome::Failed);
    assert!(r.problems.is_empty());
    assert_eq!(r.summary(200), "just text");

    let broke = script(
        root,
        "t/broke.py",
        "import sys\nsys.stderr.write('Traceback: boom\\n')\nsys.exit(2)\n",
    );
    let r = run(&py, "broke", root, &broke, &input(&[]), SECS).await;
    assert_eq!(r.outcome, Outcome::Error, "{r:?}");
    assert_eq!(r.reason.as_deref(), Some("exit code 2"));
    assert!(r.text.unwrap().contains("Traceback: boom"));

    let crash = script(root, "t/crash.py", "raise ValueError('nope')\n");
    let r = run(&py, "crash", root, &crash, &input(&[]), SECS).await;
    assert_eq!(r.outcome, Outcome::Error, "{r:?}");
    assert!(r.text.unwrap().contains("ValueError: nope"));

    let missing = root.join("t/none.py");
    let r = run(&py, "none", root, &missing, &input(&[]), SECS).await;
    assert_eq!(r.outcome, Outcome::Error, "{r:?}");
}

#[tokio::test]
async fn timeout_stops_the_script() {
    let py = python().await;
    let dir = tempfile::tempdir().unwrap();
    let slow = script(dir.path(), "slow.py", "import time\ntime.sleep(60)\n");
    let started = std::time::Instant::now();
    let r = run(
        &py,
        "slow",
        dir.path(),
        &slow,
        &input(&[]),
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(r.outcome, Outcome::Error);
    assert_eq!(r.reason.as_deref(), Some("timed out after 1 s"));
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn environment_working_directory_and_odd_names() {
    let py = python().await;
    let dir = tempfile::tempdir().unwrap();
    // A root whose name a shell would split or interpret.
    let root = dir.path().join("my depot & 'quotes'");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("data file & co.json"), "{}").unwrap();
    std::env::set_var("LODE_TOKEN", "secret-token");
    let s = script(
        &root,
        "tools/env check.py",
        r#"import os, sys, json
d = json.load(sys.stdin)
assert os.environ.get("LODE_TOKEN") is None, "token leaked"
assert os.environ["LODE_VALIDATE"] == "1"
for f in d["files"]:
    assert os.path.exists(f["path"]), f["path"]
print(d["files"][0]["path"])
"#,
    );
    let r = run(
        &py,
        "env",
        &root,
        &s,
        &input(&["data file & co.json"]),
        SECS,
    )
    .await;
    assert_eq!(r.outcome, Outcome::Passed, "{r:?}");
    assert_eq!(r.text.as_deref(), Some("data file & co.json"));
}

#[tokio::test]
async fn missing_and_old_python() {
    let e = find_python(&[vec!["no-such-python-here".into()]], None)
        .await
        .unwrap_err();
    assert_eq!(
        e,
        FindError::Missing {
            tried: vec!["no-such-python-here".into()]
        }
    );
    let py = python().await;
    match find_python(std::slice::from_ref(&py.command), Some((3, 999, 0))).await {
        Err(FindError::TooOld { found }) => assert_eq!(found.version, py.version),
        other => panic!("{other:?}"),
    }
}

#[test]
fn output_and_matching() {
    let (p, t) = parse_output("{\"message\": \"m\"}\n{not json\n\n{\"nomessage\": 1}\n");
    assert_eq!(p.len(), 1);
    assert_eq!(t, "{not json\n\n{\"nomessage\": 1}");

    use lode_core::{Change, ChangeAction, CopyFrom, DepotPath};
    let path = |s: &str| DepotPath::parse(s).unwrap();
    let change = |p: &str, a: ChangeAction, from: Option<&str>| Change {
        path: path(p),
        action: a,
        base_id: None,
        new_id: None,
        kind: None,
        executable: false,
        size: None,
        copy_from: from.map(|f| CopyFrom {
            path: path(f),
            rev: 1,
        }),
    };
    let files = input_files(&[
        change("Data/a.json", ChangeAction::Modify, None),
        change("Data/new.json", ChangeAction::Add, Some("Old/b.json")),
        change("Old/b.json", ChangeAction::Delete, None),
        change("Data/c.json", ChangeAction::Add, Some("Data/a.json")),
        change("x.txt", ChangeAction::Delete, None),
    ]);
    let got: Vec<(String, String, Option<String>)> = files
        .iter()
        .map(|f| (f.path.clone(), f.action.clone(), f.from.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("Data/a.json".into(), "edit".into(), None),
            (
                "Data/new.json".into(),
                "move".into(),
                Some("Old/b.json".into())
            ),
            ("Data/c.json".into(), "add".into(), None),
            ("x.txt".into(), "delete".into(), None),
        ]
    );
    let only_old = lode_core::PathGlobSet::new(["Old/**"]).unwrap();
    assert_eq!(matching(&files, &only_old).len(), 1);
}
