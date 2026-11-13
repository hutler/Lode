//! T086 acceptance: `lode import git` replays a branch's first-parent history, one revision
//! per commit, and every revision's tree matches the Git tree; an import stopped halfway
//! resumes without duplicates.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use futures::TryStreamExt;
use lode_client::Client;
use lode_core::{EntryKind, ObjectId};
use lode_testkit::{TestServer, TestWorkspace};

/// A Git repository built by the test, with its own empty configuration.
struct Repo {
    dir: tempfile::TempDir,
    config: PathBuf,
    tick: u64,
    /// Files written since the last commit, added by it.
    pending: Vec<String>,
}

impl Repo {
    fn new() -> Option<Repo> {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("gitconfig");
        std::fs::write(&config, "").unwrap();
        let repo = Repo {
            config,
            dir,
            tick: 0,
            pending: Vec::new(),
        };
        let work = repo.work();
        std::fs::create_dir(&work).unwrap();
        // Git is needed; a machine without it skips the test.
        repo.try_git(&["init", "-q", "-b", "main"])?;
        Some(repo)
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        let date = format!("{} +0300", 1_790_000_000 + self.tick * 3600);
        c.current_dir(self.work())
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ada Lovelace")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada Lovelace")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
            .args(args);
        c
    }

    fn try_git(&self, args: &[&str]) -> Option<Vec<u8>> {
        let out = self.command(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn write(&mut self, path: &str, content: impl AsRef<[u8]>) {
        self.pending.push(path.to_owned());
        let p = self.work().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Stores `content` as a blob and puts it in the index at `path` with `mode`.
    fn index(&self, mode: &str, path: &str, content: &[u8]) {
        let mut c = self.command(&["hash-object", "-w", "--stdin"]);
        c.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), content).unwrap();
        let out = child.wait_with_output().unwrap();
        let sha = String::from_utf8(out.stdout).unwrap();
        self.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{},{path}", sha.trim()),
        ]);
    }

    fn commit(&mut self, message: &str) {
        self.tick += 1;
        // Only what was written: index-only entries (symlink, submodule) have no file.
        let pending = std::mem::take(&mut self.pending);
        if !pending.is_empty() {
            let mut args = vec!["add", "--"];
            args.extend(pending.iter().map(String::as_str));
            self.git(&args);
        }
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }

    fn rev_list(&self) -> Vec<String> {
        String::from_utf8(self.git(&["rev-list", "--first-parent", "--reverse", "main"]))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The tree of `commit` as Lode would hold it: path → (kind, executable, blob id).
    fn tree(&self, commit: &str) -> BTreeMap<String, (EntryKind, bool, ObjectId)> {
        let out = self.git(&["ls-tree", "-r", "-z", commit]);
        let mut tree = BTreeMap::new();
        for entry in String::from_utf8(out).unwrap().split('\0') {
            if entry.is_empty() {
                continue;
            }
            let (head, path) = entry.split_once('\t').unwrap();
            let parts: Vec<&str> = head.split(' ').collect();
            let (mode, sha) = (parts[0], parts[2]);
            if mode == "160000" {
                continue;
            }
            let mut content = self.git(&["cat-file", "blob", sha]);
            if let Some(oid) = lfs_oid(&content) {
                let local = self.lfs_object(&oid);
                if local.is_file() {
                    content = std::fs::read(local).unwrap();
                }
            }
            let kind = if mode == "120000" {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            let id = lode_core::blob_id(&content[..]).unwrap();
            tree.insert(path.to_owned(), (kind, mode == "100755", id));
        }
        tree
    }

    fn lfs_object(&self, oid: &str) -> PathBuf {
        self.work()
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(oid)
    }

    /// Commits an LFS pointer for `content` at `path`, with the content in the local store
    /// when `stored`.
    fn lfs(&mut self, path: &str, content: &[u8], stored: bool) {
        let oid = format!("{:064x}", content.len() as u128 * 7919 + path.len() as u128);
        self.write(
            path,
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
                content.len()
            ),
        );
        if stored {
            let p = self.lfs_object(&oid);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
}

fn lfs_oid(content: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(content).ok()?;
    let rest = text.strip_prefix("version https://git-lfs.github.com/spec/v1\noid sha256:")?;
    Some(rest[..64].to_owned())
}

/// A small deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

fn text(seed: usize, lines: usize) -> String {
    (0..lines)
        .map(|i| format!("line {i} of file {seed}: the quick brown fox\n"))
        .collect()
}

fn binary(seed: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i * 31 + seed * 17) % 256) as u8)
        .collect()
}

/// A 50-commit history on `main` with adds, edits, deletes, renames, binary files, an
/// executable bit, a symlink, LFS pointers, a submodule, an empty commit and a merge.
fn build_history() -> Option<Repo> {
    let mut r = Repo::new()?;
    let mut rng = Lcg(7);
    let mut files: Vec<String> = Vec::new();
    for i in 0..10 {
        let p = format!("src/mod{i}/file{i}.txt");
        r.write(&p, text(i, 20));
        files.push(p);
    }
    r.write("assets/logo.bin", binary(1, 5000));
    r.commit("Initial commit\n\nWith a body.");
    let mut next = 100;
    while r.rev_list().len() < 45 {
        let n = r.rev_list().len();
        match n {
            5 => {
                r.write("tools/build.sh", "#!/bin/sh\necho build\n");
                r.pending.clear();
                r.git(&["add", "tools/build.sh"]);
                r.git(&["update-index", "--chmod=+x", "tools/build.sh"]);
                r.git(&["commit", "-q", "-m", "Add an executable script"]);
                r.tick += 1;
                continue;
            }
            8 => {
                r.index("120000", "docs/latest", b"../src/mod1/file1.txt");
                r.git(&["commit", "-q", "-m", "Add a symlink"]);
                r.tick += 1;
                continue;
            }
            11 => {
                r.lfs("art/hero.psd", &binary(9, 20_000), true);
                r.lfs("art/missing.psd", &binary(10, 30_000), false);
                r.commit("Add LFS files");
                continue;
            }
            14 => {
                let head = String::from_utf8(r.git(&["rev-parse", "HEAD"])).unwrap();
                // A gitlink names a commit of another repository; any id will do here.
                r.git(&[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{},vendor/lib", head.trim()),
                ]);
                r.git(&["commit", "-q", "-m", "Add a submodule"]);
                r.tick += 1;
                continue;
            }
            17 => {
                r.commit("An empty commit");
                continue;
            }
            20 => {
                // A side branch merged back: first-parent sees one commit with its changes.
                r.git(&["checkout", "-q", "-b", "side"]);
                r.write("side/feature.txt", text(500, 10));
                r.commit("Side work 1");
                r.write("side/more.txt", text(501, 10));
                r.commit("Side work 2");
                r.git(&["checkout", "-q", "main"]);
                r.write("main-only.txt", "main\n");
                r.commit("Main work during the side branch");
                r.tick += 1;
                r.git(&["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);
                continue;
            }
            30 => {
                r.git(&["rm", "-q", "docs/latest"]);
                r.index("100644", "docs/latest", b"now a file\n");
                r.git(&["commit", "-q", "-m", "Symlink becomes a file"]);
                r.tick += 1;
                continue;
            }
            _ => {}
        }
        match rng.next(4) {
            0 => {
                let p = format!("src/new/file{next}.txt");
                r.write(&p, text(next, 15));
                files.push(p);
                next += 1;
            }
            1 => {
                let p = &files[rng.next(files.len())];
                r.write(p, text(next, 22));
                next += 1;
                r.write("assets/logo.bin", binary(next, 5000 + next));
            }
            2 if files.len() > 5 => {
                let p = files.remove(rng.next(files.len()));
                r.git(&["rm", "-q", &p]);
            }
            _ => {
                let from = files.remove(rng.next(files.len()));
                let to = format!("moved/{next}/{}", from.rsplit('/').next().unwrap());
                std::fs::create_dir_all(r.work().join(&to).parent().unwrap()).unwrap();
                r.git(&["mv", &from, &to]);
                files.push(to);
                next += 1;
            }
        }
        let n = r.rev_list().len();
        r.commit(&format!("Change {n}"));
    }
    while r.rev_list().len() < 50 {
        let n = r.rev_list().len();
        r.write(&format!("tail/{n}.txt"), text(n, 3));
        r.commit(&format!("Tail {n}"));
    }
    Some(r)
}

/// Every file of `depot` at `rev`.
async fn lode_tree(
    client: &Client,
    depot: &str,
    rev: u64,
) -> BTreeMap<String, (EntryKind, bool, ObjectId)> {
    let mut out = BTreeMap::new();
    let mut m = client.manifest(depot, rev, &[], &[]).await.unwrap();
    while let Some(e) = m.try_next().await.unwrap() {
        out.insert(e.path.to_string(), (e.kind, e.executable, e.id));
    }
    out
}

/// The imported revisions, oldest first: (rev, Git commit id, root tree).
async fn imported(client: &Client, depot: &str) -> Vec<(u64, String, ObjectId)> {
    let mut out = Vec::new();
    let head = client.branch(depot, "main").await.unwrap().head_rev;
    for rev in 2..=head {
        let r = client.revision(depot, rev).await.unwrap();
        let i = r.provenance.unwrap().imported.unwrap();
        assert_eq!(i.system, "git");
        out.push((rev, i.id, r.root_tree));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_history_imports_with_matching_trees() {
    let Some(repo) = build_history() else {
        eprintln!("skipped: git is not available");
        return;
    };
    let commits = repo.rev_list();
    assert_eq!(commits.len(), 50);
    let server = TestServer::start().await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let work = repo.work();
    let done = ws.json(&[
        "import",
        "git",
        work.to_str().unwrap(),
        "game",
        "--branch",
        "main",
    ]);
    assert_eq!(done["total"], 50, "{done}");
    assert_eq!(done["already"], 0);
    // The empty commit changes nothing, and neither does the submodule-only one.
    assert_eq!(done["skipped"], 2, "{done}");
    assert_eq!(done["imported"], 48);
    assert_eq!(done["rev"], 49);
    assert_eq!(done["last_commit"], commits[49].as_str());
    assert_eq!(done["lfs_replaced"], 1);
    assert_eq!(done["lfs_missing"], serde_json::json!(["art/missing.psd"]));
    assert_eq!(done["submodules"], serde_json::json!(["vendor/lib"]));

    // Every revision's tree is its Git commit's tree.
    let client = server.admin();
    let revs = imported(&client, "game").await;
    assert_eq!(revs.len(), 48);
    let mut at = 0;
    for (rev, id, _) in &revs {
        let index = commits.iter().position(|c| c == id).unwrap();
        assert!(index >= at, "in order");
        at = index;
        assert_eq!(
            lode_tree(&client, "game", *rev).await,
            repo.tree(id),
            "r{rev} = {id}"
        );
    }

    // The original author, date and message are kept; renames are renames.
    let r = client.revision("game", 2).await.unwrap();
    let i = r.provenance.unwrap().imported.unwrap();
    assert_eq!(i.author.as_deref(), Some("Ada Lovelace <ada@example.com>"));
    assert_eq!(i.date.as_deref(), Some("2026-09-21T18:13:20+03:00"));
    assert_eq!(r.message, "Initial commit\n\nWith a body.");
    let log = ws.json(&["log", "--depot", "game", "-v", "-n", "100"]);
    let renamed = log["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["changes"].as_array().cloned().unwrap_or_default())
        .filter(|c| !c["copy_from"].is_null())
        .count();
    assert!(renamed > 0, "{log}");
    let human = ws
        .cmd()
        .args(["log", "--depot", "game", "-n", "1"])
        .output()
        .unwrap();
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("imported from git"), "{human}");

    // Nothing more to do on a rerun.
    let again = ws.json(&[
        "import",
        "git",
        work.to_str().unwrap(),
        "game",
        "--branch",
        "main",
    ]);
    assert_eq!(
        (again["imported"].as_u64(), again["rev"].as_u64()),
        (Some(0), Some(49))
    );

    // Refusals.
    let check = |args: &[&str], code: &str| {
        let (status, v) = ws.json_status(args);
        assert_eq!(
            (status, v["error"]["code"].as_str()),
            (1, Some(code)),
            "{v}"
        );
    };
    check(
        &[
            "import",
            "git",
            work.to_str().unwrap(),
            "other",
            "--branch",
            "nope",
        ],
        "unknown_git_branch",
    );
    let not_repo = tempfile::tempdir().unwrap();
    check(
        &[
            "import",
            "git",
            not_repo.path().to_str().unwrap(),
            "other",
            "--branch",
            "main",
        ],
        "not_a_git_repository",
    );
    server.create_depot("used").await;
    server
        .commit_files("used", "main", &[("a.txt", b"a")])
        .await;
    check(
        &[
            "import",
            "git",
            work.to_str().unwrap(),
            "used",
            "--branch",
            "main",
        ],
        "depot_not_empty",
    );
    let mut repo = repo;
    repo.git(&["checkout", "-q", "-f", "-b", "elsewhere", &commits[3]]);
    repo.write("x.txt", "x");
    repo.commit("Elsewhere");
    check(
        &[
            "import",
            "git",
            work.to_str().unwrap(),
            "game",
            "--branch",
            "elsewhere",
        ],
        "import_diverged",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_stopped_halfway_resumes_without_duplicates() {
    let Some(repo) = build_history() else {
        eprintln!("skipped: git is not available");
        return;
    };
    let server = TestServer::start().await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let work = repo.work();
    let args = [
        "import",
        "git",
        work.to_str().unwrap(),
        "half",
        "--branch",
        "main",
    ];
    let out = ws
        .cmd()
        .args(args)
        .env("LODE_TEST_CRASH_AT", "import.after.25")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(86), "the crash point stops it");
    let client = server.admin();
    let before = imported(&client, "half").await;
    assert!(before.len() >= 20 && before.len() < 48, "{}", before.len());

    let done = ws.json(&args);
    assert_eq!(done["already"], 25, "{done}");
    assert_eq!(done["rev"], 49, "{done}");

    // Each Git commit is in the depot once, and the trees are those of a clean import.
    let after = imported(&client, "half").await;
    assert_eq!(after.len(), 48);
    let mut ids: Vec<&String> = after.iter().map(|(_, id, _)| id).collect();
    ids.dedup();
    assert_eq!(ids.len(), 48, "no duplicates");
    assert_eq!(&after[..before.len()], &before[..]);
    let clean = ws.json(&[
        "import",
        "git",
        work.to_str().unwrap(),
        "clean",
        "--branch",
        "main",
    ]);
    assert_eq!(clean["rev"], 49);
    let clean = imported(&client, "clean").await;
    let trees = |v: &[(u64, String, ObjectId)]| v.iter().map(|(_, _, t)| *t).collect::<Vec<_>>();
    assert_eq!(trees(&after), trees(&clean));
}
