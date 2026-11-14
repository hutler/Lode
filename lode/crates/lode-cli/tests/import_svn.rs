//! T121 acceptance: `lode import svn` replays the revisions that change a Subversion
//! directory, one Lode revision each, with every tree matching `svn export` of that revision;
//! an import stopped halfway resumes without duplicates. Skipped where `svn` is missing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use futures::TryStreamExt;
use lode_client::Client;
use lode_core::{blob_id, EntryKind, ObjectId};
use lode_testkit::{TestServer, TestWorkspace};

/// A repository made with `svnadmin`, and a working copy of its trunk.
struct Repo {
    dir: tempfile::TempDir,
    /// `svn` changes made from the working copy, numbered as the repository numbers them.
    revs: Vec<u64>,
}

fn svn_ok() -> bool {
    Command::new("svnadmin")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
        && Command::new("svn")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

impl Repo {
    fn url(&self) -> String {
        let p = self.dir.path().join("repo");
        let s = p.to_str().unwrap().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    fn wc(&self) -> PathBuf {
        self.dir.path().join("wc")
    }

    fn svn(&self, args: &[&str]) -> String {
        let out = Command::new("svn")
            .current_dir(self.wc())
            .env("HOME", self.dir.path())
            .args(["--non-interactive", "--config-dir"])
            .arg(self.dir.path().join("svnconfig"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "svn {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.wc().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Commits the working copy and records the revision.
    fn commit(&mut self, message: &str) -> u64 {
        self.svn(&["update", "-q", "--ignore-externals"]);
        let out = self.svn(&["commit", "-m", message]);
        let rev = out
            .lines()
            .filter_map(|l| l.strip_prefix("Committed revision "))
            .next()
            .unwrap()
            .trim_end_matches('.')
            .parse()
            .unwrap();
        self.revs.push(rev);
        rev
    }

    /// Every file of trunk at `rev`, as `svn export` writes it.
    fn tree(&self, rev: u64) -> BTreeMap<String, (EntryKind, bool, ObjectId)> {
        let dest = self.dir.path().join(format!("export-{rev}"));
        let url = format!("{}/trunk", self.url());
        self.svn(&[
            "export",
            "-q",
            "--ignore-externals",
            "-r",
            &rev.to_string(),
            &url,
            dest.to_str().unwrap(),
        ]);
        let mut out = BTreeMap::new();
        walk(&dest, &dest, &mut out);
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (EntryKind, bool, ObjectId)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let meta = std::fs::symlink_metadata(&p).unwrap();
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&p).unwrap();
            let id = blob_id(target.to_str().unwrap().as_bytes()).unwrap();
            out.insert(rel, (EntryKind::Symlink, false, id));
        } else if meta.is_dir() {
            walk(root, &p, out);
        } else {
            let id = blob_id(std::fs::File::open(&p).unwrap()).unwrap();
            out.insert(rel, (EntryKind::File, executable(&meta), id));
        }
    }
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// A history with every kind of change the importer handles.
fn build_history() -> Option<Repo> {
    if !svn_ok() {
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let st = Command::new("svnadmin")
        .arg("create")
        .arg(dir.path().join("repo"))
        .status()
        .unwrap();
    assert!(st.success());
    let mut r = Repo {
        dir,
        revs: Vec::new(),
    };
    std::fs::create_dir(r.wc()).unwrap();
    let url = r.url();
    r.svn(&[
        "mkdir",
        "-m",
        "Layout",
        &format!("{url}/trunk"),
        &format!("{url}/branches"),
    ]);
    r.svn(&["checkout", "-q", &format!("{url}/trunk"), "."]);

    // Files, an executable, a folder, a symlink, a name that needs encoding.
    r.write("a.txt", "one\n");
    r.write("run.sh", "#!/bin/sh\necho hi\n");
    r.write("dir/b.txt", "bee\n");
    r.write("dir/sub/c.txt", "sea\n");
    r.write("odd name #1.txt", "odd\n");
    r.svn(&["add", "-q", "a.txt", "run.sh", "dir", "odd name #1.txt"]);
    r.svn(&["propset", "-q", "svn:executable", "*", "run.sh"]);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a.txt", r.wc().join("link")).unwrap();
        r.svn(&["add", "-q", "link"]);
    }
    r.commit("First files\n\nWith a body.");

    // An edit, and a change outside trunk (not in trunk's history).
    r.write("a.txt", "one\ntwo\n");
    r.commit("Edit a");
    r.svn(&["mkdir", "-m", "Outside", &format!("{url}/branches/x")]);

    // A rename, a folder rename, a delete.
    r.svn(&["mv", "-q", "a.txt", "renamed.txt"]);
    r.svn(&["mv", "-q", "dir/sub", "dir/moved"]);
    r.svn(&["rm", "-q", "run.sh"]);
    r.commit("Move things");

    // A folder copied inside trunk, with an edit in the copy.
    r.svn(&["cp", "-q", "dir", "dir2"]);
    r.write("dir2/b.txt", "bee two\n");
    r.commit("Copy dir");

    // Properties only: nothing a depot holds changes.
    r.svn(&["propset", "-q", "custom", "x", "renamed.txt"]);
    r.commit("A property");

    // An external, which is not imported.
    r.svn(&[
        "propset",
        "-q",
        "svn:externals",
        "ext file:///nowhere/x",
        ".",
    ]);
    r.commit("An external");

    // Executable bit set on an existing file, and a replaced file.
    r.svn(&["propset", "-q", "svn:executable", "*", "renamed.txt"]);
    r.svn(&["rm", "-q", "dir/b.txt"]);
    r.write("dir/b.txt", "new bee\n");
    r.svn(&["add", "-q", "dir/b.txt"]);
    r.commit("Replace b");

    for n in 0..6 {
        r.write(&format!("tail/{n}.txt"), &format!("{n}\n"));
        r.svn(&["add", "-q", "--parents", &format!("tail/{n}.txt")]);
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
        let exec = cfg!(unix) && e.executable;
        out.insert(e.path.to_string(), (e.kind, exec, e.id));
    }
    out
}

/// The imported revisions, oldest first: (Lode rev, SVN rev).
async fn imported(client: &Client, depot: &str) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let head = client.branch(depot, "main").await.unwrap().head_rev;
    for rev in 2..=head {
        let r = client.revision(depot, rev).await.unwrap();
        let i = r.provenance.unwrap().imported.unwrap();
        assert_eq!(i.system, "svn");
        out.push((rev, i.id.parse().unwrap()));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_svn_history_imports_with_matching_trees() {
    let Some(repo) = build_history() else {
        eprintln!("skipped: svn is not available");
        return;
    };
    let server = TestServer::start().await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let trunk = format!("{}/trunk", repo.url());
    let done = ws.json(&["import", "svn", &trunk, "game"]);
    // r1 makes trunk itself, then 13 commits from the working copy.
    assert_eq!(done["total"], 14, "{done}");
    assert_eq!(done["already"], 0);
    // Making the empty trunk, the property and the external change nothing a depot holds.
    assert_eq!(done["skipped"], 3, "{done}");
    assert_eq!(done["imported"], 11);
    assert_eq!(done["last_revision"], *repo.revs.last().unwrap());
    assert_eq!(done["externals"], serde_json::json!(["."]));

    let client = server.admin();
    let revs = imported(&client, "game").await;
    assert_eq!(revs.len(), 11);
    for (rev, svn_rev) in &revs {
        assert_eq!(
            lode_tree(&client, "game", *rev).await,
            repo.tree(*svn_rev),
            "r{rev} = svn r{svn_rev}"
        );
    }
    // Message, author and date are kept; renames are copies in Lode.
    let first = client.revision("game", 2).await.unwrap();
    assert_eq!(first.message, "First files\n\nWith a body.");
    let i = first.provenance.unwrap().imported.unwrap();
    assert_eq!(i.id, repo.revs[0].to_string());
    assert!(i.author.is_some());
    assert!(i.date.unwrap().ends_with('Z'));
    let log = ws.json(&["log", "--depot", "game", "-v", "-n", "100"]);
    let copies: Vec<(String, String)> = log["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["changes"].as_array().cloned().unwrap_or_default())
        .filter(|c| !c["copy_from"].is_null())
        .map(|c| {
            (
                c["copy_from"]["path"].as_str().unwrap().to_owned(),
                c["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for pair in [
        ("a.txt", "renamed.txt"),
        ("dir/sub/c.txt", "dir/moved/c.txt"),
        ("dir/moved/c.txt", "dir2/moved/c.txt"),
    ] {
        let pair = (pair.0.to_owned(), pair.1.to_owned());
        assert!(copies.contains(&pair), "{pair:?} in {copies:?}");
    }
    // An edited file in a copied folder is new content, not a copy.
    assert!(
        !copies.iter().any(|(_, to)| to == "dir2/b.txt"),
        "{copies:?}"
    );

    let human = ws
        .cmd()
        .args(["log", "--depot", "game", "-n", "1"])
        .output()
        .unwrap();
    let human = String::from_utf8(human.stdout).unwrap();
    let last = repo.revs.last().unwrap();
    assert!(
        human.contains(&format!("imported from svn r{last}")),
        "{human}"
    );

    // Nothing new: a rerun does nothing.
    let again = ws.json(&["import", "svn", &trunk, "game"]);
    assert_eq!(again["imported"], 0, "{again}");
    assert_eq!(again["already"], 14);

    // A depot with its own revisions is refused.
    server
        .commit_files("game", "main", &[("own.txt", b"x")])
        .await;
    let (status, v) = ws.json_status(&["import", "svn", &trunk, "game"]);
    assert_eq!(status, 1, "{v}");
    assert_eq!(v["error"]["code"], "depot_not_empty", "{v}");

    // Not a repository.
    let (status, v) = ws.json_status(&["import", "svn", "file:///no/such/repo", "other"]);
    assert_eq!(status, 1, "{v}");
    assert_eq!(v["error"]["code"], "not_an_svn_directory", "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_svn_import_stopped_halfway_resumes_without_duplicates() {
    let Some(repo) = build_history() else {
        eprintln!("skipped: svn is not available");
        return;
    };
    let server = TestServer::start().await;
    let ws = TestWorkspace::new(&server).with_token(server.admin_token());
    let trunk = format!("{}/trunk", repo.url());
    let out = ws
        .cmd()
        .args(["import", "svn", &trunk, "half"])
        .env("LODE_TEST_CRASH_AT", "import.after.6")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(86), "the crash point stops it");
    let client = server.admin();
    let before = imported(&client, "half").await;
    assert!(!before.is_empty() && before.len() < 11, "{}", before.len());

    let done = ws.json(&["import", "svn", &trunk, "half"]);
    // Stopped after the property-only revision, which made no Lode revision: the resume
    // point is the last revision that did, and the skipped one is looked at again.
    assert_eq!(done["already"], 5, "{done}");
    let after = imported(&client, "half").await;
    assert_eq!(after.len(), 11);
    let mut ids: Vec<u64> = after.iter().map(|(_, s)| *s).collect();
    ids.dedup();
    assert_eq!(ids.len(), 11, "no duplicates");
    assert_eq!(&after[..before.len()], &before[..]);
}
