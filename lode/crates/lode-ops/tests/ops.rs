//! T090 acceptance: operations run through `lode-ops` alone, without the `lode` binary, return
//! the documented JSON and report through the `Ui` they were given.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lode_ops::cmd::{checkout, commit, history, status};
use lode_ops::context::{Ctx, Globals};
use lode_ops::output::{ProgressEvent, Silent, Ui};
use lode_testkit::TestServer;

/// Everything an operation reported.
#[derive(Debug, Default)]
struct Collected {
    lines: Vec<String>,
    warnings: Vec<String>,
    progress: Vec<(String, u64, u64, bool)>,
}

#[derive(Debug, Default)]
struct Collector(Mutex<Collected>);

impl Ui for Collector {
    fn line(&self, text: &str) {
        self.0.lock().unwrap().lines.push(text.to_owned());
    }
    fn warn(&self, text: &str) {
        self.0.lock().unwrap().warnings.push(text.to_owned());
    }
    fn progress(&self, e: &ProgressEvent<'_>) {
        self.0
            .lock()
            .unwrap()
            .progress
            .push((e.label.to_owned(), e.done, e.total, e.finished));
    }
}

struct Setup {
    server: TestServer,
    _home: tempfile::TempDir,
    config: PathBuf,
    ws: PathBuf,
    /// The revision checked out.
    base: u64,
}

impl Setup {
    fn ctx(&self, dir: &Path, json: bool, ui: Arc<dyn Ui>) -> Ctx {
        Ctx::in_dir(
            Globals {
                json,
                ..Globals::default()
            },
            dir,
            ui,
        )
        .unwrap()
        .with_config_dir(&self.config)
    }
}

/// A server with a depot of two files, the admin signed in, and a workspace checked out.
async fn setup() -> Setup {
    let server = TestServer::start().await;
    server.create_depot("game").await;
    server
        .commit_files(
            "game",
            "main",
            &[("a.txt", b"apple\n"), ("b.txt", b"banana\n")],
        )
        .await;
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let mut s = Setup {
        ws: home.path().join("ws"),
        server,
        _home: home,
        config,
        base: 0,
    };
    let base = s._home.path().to_path_buf();
    let ctx = s.ctx(&base, true, Arc::new(Silent));
    let url = lode_client::parse_server_url(s.server.url(), false).unwrap();
    ctx.credentials()
        .save(&url, s.server.admin_token())
        .unwrap();
    let out = checkout::checkout(
        &ctx,
        checkout::CheckoutArgs {
            target: format!("{}/game", s.server.url()),
            dir: Some("ws".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out["files"], 2, "{out}");
    s.base = out["rev"].as_u64().unwrap();
    s.ws = lode_workspace::paths::canonical(&s.ws).unwrap();
    s
}

#[tokio::test]
async fn status_commit_and_log_without_the_binary() {
    let s = setup().await;
    std::fs::write(s.ws.join("a.txt"), "apricot\n").unwrap();
    std::fs::write(s.ws.join("c.txt"), "cherry\n").unwrap();

    let ui = Arc::new(Collector::default());
    let ctx = s.ctx(&s.ws, false, ui.clone());

    let st = status::status(&ctx, status::StatusArgs::default())
        .await
        .unwrap();
    assert_eq!(st["branch"], "main");
    assert_eq!(st["rev"], s.base);
    let files: Vec<(String, String)> = st["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            ("a.txt".to_owned(), "modified".to_owned()),
            ("c.txt".to_owned(), "untracked".to_owned())
        ]
    );
    assert!(
        ui.0.lock()
            .unwrap()
            .lines
            .contains(&format!("On branch main at r{}", s.base)),
        "{:?}",
        ui.0.lock().unwrap().lines
    );

    let done = commit::commit(
        &ctx,
        commit::CommitArgs {
            message: Some("Fruit".into()),
            add_all: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(done["rev"], s.base + 1, "{done}");
    assert_eq!(done["files"], 2);
    assert_eq!(done["resumed"], false);
    {
        let seen = ui.0.lock().unwrap();
        let committed = format!("Committed r{} (2 files).", s.base + 1);
        assert!(seen.lines.contains(&committed), "{:?}", seen.lines);
        let uploads: Vec<_> = seen
            .progress
            .iter()
            .filter(|(label, ..)| label == "Uploading")
            .collect();
        assert!(
            !uploads.is_empty(),
            "no upload progress: {:?}",
            seen.progress
        );
        let last = uploads.last().unwrap();
        assert!(last.3, "the counter ends with a finished event");
        assert_eq!(last.1, last.2, "every upload counted");
        assert!(seen.warnings.is_empty(), "{:?}", seen.warnings);
    }

    let log = history::log(&ctx, history::LogArgs::default())
        .await
        .unwrap();
    let revs = log["revisions"].as_array().unwrap();
    assert_eq!(revs[0]["rev"], s.base + 1);
    assert_eq!(revs[0]["message"], "Fruit");
    assert_eq!(revs[1]["rev"], s.base);

    // Paging: one revision at a time, older each time.
    let page = history::log(
        &ctx,
        history::LogArgs {
            max: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page["revisions"][0]["rev"], s.base + 1);
    assert_eq!(page["more"], true);
    let next = history::log(
        &ctx,
        history::LogArgs {
            max: 1,
            before: page["revisions"][0]["rev"].as_u64(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(next["revisions"][0]["rev"], s.base);
}

#[tokio::test]
async fn questions_fail_cleanly_without_anyone_to_ask() {
    let s = setup().await;
    std::fs::write(s.ws.join("a.txt"), "apricot\n").unwrap();
    let ctx = s.ctx(&s.ws, false, Arc::new(Silent));

    // No message and nobody to open an editor for.
    let err = commit::commit(&ctx, commit::CommitArgs::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "usage", "{err:?}");

    // `cat` without --output or --json has nowhere to print here.
    let err = history::cat(
        &ctx,
        history::CatArgs {
            path: "b.txt".into(),
            output: None,
            source: Default::default(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "usage", "{err:?}");

    // With --json the content comes back in the document.
    let json_ctx = s.ctx(&s.ws, true, Arc::new(Silent));
    let v = history::cat(
        &json_ctx,
        history::CatArgs {
            path: "b.txt".into(),
            output: None,
            source: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(v["content"], "banana\n");
}

/// Operations never talk to the terminal themselves; that is the caller's `Ui`.
#[test]
fn sources_do_not_touch_the_terminal() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "std::io::stdin",
        "io::stdin()",
        "io::stdout()",
        "io::stderr()",
        "println!",
        "print!(",
        "eprintln!",
        "eprint!(",
        "rpassword",
        "is_terminal",
    ];
    let mut stack = vec![src];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for f in forbidden {
                    assert!(
                        !code.contains(f),
                        "{}:{}: `{f}` in lode-ops; go through the Ui instead",
                        p.display(),
                        n + 1
                    );
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 20, "only {checked} files checked");
}
