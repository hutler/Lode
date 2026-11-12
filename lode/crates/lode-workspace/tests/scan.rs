//! T021 acceptance: statuses, touch refresh, ignore rules, the view, invalid names, the
//! workspace lock and the 200,000-file benchmark.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use lode_core::{blob_id, DepotPath, EntryKind};
use lode_workspace::scan::stat_for_row;
use lode_workspace::{
    FileRow, FileState, ScanOptions, Status, ViewSpec, Workspace, WorkspaceError, WorkspaceInit,
};

fn p(s: &str) -> DepotPath {
    DepotPath::parse(s).unwrap()
}

fn init(view: ViewSpec) -> WorkspaceInit {
    WorkspaceInit {
        server_url: "https://lode.example.com".into(),
        depot: "game".into(),
        branch: "main".into(),
        view,
    }
}

struct Ws {
    _dir: tempfile::TempDir,
    ws: Workspace,
}

fn workspace(view: ViewSpec) -> Ws {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::create(&dir.path().join("ws"), &init(view)).unwrap();
    Ws { _dir: dir, ws }
}

impl Ws {
    fn write(&self, path: &str, content: &[u8]) {
        let local = self.ws.root().join(path);
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(local, content).unwrap();
    }

    /// Writes a file and records it as checked out, as checkout would.
    fn checked_out(&mut self, path: &str, content: &[u8]) {
        self.write(path, content);
        let local = self.ws.root().join(path);
        let (size, mtime) = stat_for_row(&local, SystemTime::now()).unwrap();
        let mut row = FileRow::synced(p(path), blob_id(content).unwrap(), EntryKind::File, false);
        row.size = size;
        row.mtime_ns = mtime;
        self.ws.db_mut().write(|tx| tx.put_file(&row)).unwrap();
    }

    fn set_state(&mut self, path: &str, state: FileState) {
        let mut row = self.ws.db().file(&p(path)).unwrap().unwrap();
        row.state = state;
        self.ws.db_mut().write(|tx| tx.put_file(&row)).unwrap();
    }

    fn add_row(&mut self, path: &str) {
        let row = FileRow {
            base_id: None,
            state: FileState::Added,
            ..FileRow::synced(p(path), blob_id(&b""[..]).unwrap(), EntryKind::File, false)
        };
        self.ws.db_mut().write(|tx| tx.put_file(&row)).unwrap();
    }

    fn statuses(&mut self, opts: &ScanOptions) -> Vec<(String, Status)> {
        let report = self.ws.scan(opts).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        report
            .changed()
            .map(|e| (e.path.as_str().to_owned(), e.status))
            .collect()
    }

    fn changed(&mut self) -> Vec<(String, Status)> {
        self.statuses(&ScanOptions::default())
    }
}

fn s(v: &[(&str, Status)]) -> Vec<(String, Status)> {
    v.iter().map(|(a, b)| ((*a).to_owned(), *b)).collect()
}

fn age(path: &Path, by: Duration) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

#[test]
fn every_status_has_its_scenario() {
    let mut w = workspace(ViewSpec::default());
    w.checked_out("same.txt", b"same");
    w.checked_out("dir/edited.txt", b"before");
    w.checked_out("gone.txt", b"gone");
    w.checked_out("removed.txt", b"removed");
    w.checked_out("clash.txt", b"clash");
    w.write("new.txt", b"new");
    w.write("dir/untracked.txt", b"?");
    w.add_row("new.txt");
    w.add_row("added-then-lost.txt");
    w.set_state("removed.txt", FileState::Deleted);
    w.set_state("clash.txt", FileState::Conflicted);
    w.write("dir/edited.txt", b"after!");
    fs::remove_file(w.ws.root().join("gone.txt")).unwrap();
    fs::remove_file(w.ws.root().join("removed.txt")).unwrap();

    assert_eq!(
        w.changed(),
        s(&[
            ("added-then-lost.txt", Status::Missing),
            ("clash.txt", Status::Conflicted),
            ("dir/edited.txt", Status::Modified),
            ("dir/untracked.txt", Status::Untracked),
            ("gone.txt", Status::Missing),
            ("new.txt", Status::Added),
            ("removed.txt", Status::Deleted),
        ])
    );
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    let same = report.get(&p("same.txt")).unwrap();
    assert_eq!(same.status, Status::Unchanged);
    let edited = report.get(&p("dir/edited.txt")).unwrap();
    assert_eq!(
        edited.disk.as_ref().unwrap().id,
        Some(blob_id(&b"after!"[..]).unwrap())
    );

    // Same size, different content: found by hashing.
    w.write("same.txt", b"SAME");
    assert!(w
        .changed()
        .contains(&("same.txt".to_owned(), Status::Modified)));

    // A limited scan sees only its paths.
    let only_dir = ScanOptions {
        paths: vec![p("dir")],
        ..Default::default()
    };
    assert_eq!(
        w.statuses(&only_dir),
        s(&[
            ("dir/edited.txt", Status::Modified),
            ("dir/untracked.txt", Status::Untracked),
        ])
    );
}

#[test]
fn touching_without_changing_refreshes_the_stored_mtime() {
    let mut w = workspace(ViewSpec::default());
    w.checked_out("a.txt", b"content");
    let local = w.ws.root().join("a.txt");
    // Just written, so its mtime is too recent to trust: stored as 0, hashed next time.
    assert_eq!(w.ws.db().file(&p("a.txt")).unwrap().unwrap().mtime_ns, 0);

    age(&local, Duration::from_secs(3600));
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    assert_eq!(report.get(&p("a.txt")).unwrap().status, Status::Unchanged);
    assert_eq!(report.refreshed, 1);
    let row = w.ws.db().file(&p("a.txt")).unwrap().unwrap();
    let disk = fs::metadata(&local).unwrap().modified().unwrap();
    let disk_ns = disk
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    assert_eq!(row.mtime_ns, disk_ns);

    // A second scan trusts the stat and has nothing to refresh.
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    assert_eq!(report.refreshed, 0);
    assert_eq!(report.get(&p("a.txt")).unwrap().status, Status::Unchanged);

    // Touched again (new mtime, same bytes): still unchanged, refreshed again.
    age(&local, Duration::from_secs(1800));
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    assert_eq!(report.get(&p("a.txt")).unwrap().status, Status::Unchanged);
    assert_eq!(report.refreshed, 1);
}

#[test]
fn ignore_files_nest_and_tracked_files_stay_tracked() {
    let mut w = workspace(ViewSpec::default());
    w.write(".lodeignore", b"*.log\nbuild/\n");
    w.write("sub/.lodeignore", b"!keep.log\n*.tmp\n");
    w.write("a.log", b"x");
    w.write("sub/keep.log", b"x");
    w.write("sub/x.tmp", b"x");
    w.write("x.tmp", b"x");
    w.write("build/out.o", b"x");
    w.write("build/deep/out.o", b"x");
    w.checked_out("build/tracked.txt", b"tracked");
    w.write(".lode/stray", b"never seen");
    assert_eq!(
        w.changed(),
        s(&[
            (".lodeignore", Status::Untracked),
            ("sub/.lodeignore", Status::Untracked),
            ("sub/keep.log", Status::Untracked),
            ("x.tmp", Status::Untracked),
        ])
    );
    // A tracked file under an ignored directory is still checked.
    w.write("build/tracked.txt", b"changed");
    assert!(w
        .changed()
        .contains(&("build/tracked.txt".to_owned(), Status::Modified)));

    // `clean --ignored` needs the ignored files too.
    let with_ignored = ScanOptions {
        report_ignored: true,
        ..Default::default()
    };
    let ignored: Vec<String> = w
        .statuses(&with_ignored)
        .into_iter()
        .filter(|e| e.1 == Status::Ignored)
        .map(|e| e.0)
        .collect();
    assert_eq!(
        ignored,
        vec!["a.log", "build/deep/out.o", "build/out.o", "sub/x.tmp"]
    );
}

#[test]
fn the_view_hides_untracked_files_outside_it() {
    let mut w = workspace(ViewSpec {
        include: vec![],
        exclude: vec!["art/**".into()],
    });
    w.write("art/big.psd", b"x");
    w.write("code/main.rs", b"x");
    assert_eq!(w.changed(), s(&[("code/main.rs", Status::Untracked)]));
    w.ws.set_view_spec(&ViewSpec {
        include: vec!["art/**".into()],
        exclude: vec![],
    })
    .unwrap();
    assert_eq!(w.changed(), s(&[("art/big.psd", Status::Untracked)]));
}

#[cfg(unix)]
#[test]
fn bad_names_are_reported_not_fatal() {
    let mut w = workspace(ViewSpec::default());
    w.write("ok.txt", b"x");
    w.write("what?.txt", b"x");
    w.write("bad:dir/inner.txt", b"x");
    w.write(".lodeignore", b"*.ignored\n");
    w.write("skip:me.ignored", b"x");
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    let names: Vec<String> = report
        .invalid
        .iter()
        .map(|i| i.local.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["bad:dir", "what?.txt"]);
    assert!(report.invalid[1].reason.contains("Windows"));
    assert!(report.get(&p("ok.txt")).is_some());
}

#[cfg(unix)]
#[test]
fn symlinks_and_the_executable_bit() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let mut w = workspace(ViewSpec::default());
    w.checked_out("run.sh", b"#!/bin/sh\n");
    let root = w.ws.root().to_path_buf();
    fs::create_dir(root.join("real")).unwrap();
    fs::write(root.join("real/file.txt"), b"x").unwrap();
    symlink("real", root.join("link")).unwrap();
    let row = FileRow::synced(
        p("link"),
        blob_id(&b"real"[..]).unwrap(),
        EntryKind::Symlink,
        false,
    );
    w.ws.db_mut().write(|tx| tx.put_file(&row)).unwrap();
    // The directory link is a tracked symlink, not followed into.
    assert_eq!(w.changed(), s(&[("real/file.txt", Status::Untracked)]));

    fs::remove_file(root.join("link")).unwrap();
    symlink("elsewhere", root.join("link")).unwrap();
    let mut perms = fs::metadata(root.join("run.sh")).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(root.join("run.sh"), perms).unwrap();
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    let link = report.get(&p("link")).unwrap();
    assert_eq!(link.status, Status::Modified);
    assert_eq!(link.disk.as_ref().unwrap().kind, EntryKind::Symlink);
    assert_eq!(
        link.disk.as_ref().unwrap().id,
        Some(blob_id(&b"elsewhere"[..]).unwrap())
    );
    let run = report.get(&p("run.sh")).unwrap();
    assert_eq!(run.status, Status::Modified);
    assert!(run.disk.as_ref().unwrap().executable);
}

#[test]
fn discovery_and_reopening() {
    let w = workspace(ViewSpec::default());
    w.write("deep/er/x.txt", b"x");
    let found = Workspace::discover(&w.ws.root().join("deep/er")).unwrap();
    assert_eq!(found.root(), w.ws.root());
    assert!(matches!(
        Workspace::create(w.ws.root(), &init(ViewSpec::default())),
        Err(WorkspaceError::AlreadyAWorkspace(_))
    ));
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        Workspace::discover(outside.path()),
        Err(WorkspaceError::NotAWorkspace(_))
    ));
}

/// Holds the lock of the workspace named by `LODE_TEST_LOCK_ROOT` until stdin closes. Run
/// by `a_second_mutating_process_fails_fast` as a child process.
#[test]
#[ignore = "helper run as a child process"]
fn lock_holder_child() {
    let Ok(root) = std::env::var("LODE_TEST_LOCK_ROOT") else {
        return;
    };
    let ws = Workspace::open(Path::new(&root)).unwrap();
    let _lock = ws.lock().unwrap();
    println!("LOCKED");
    use std::io::Read;
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
}

#[test]
fn a_second_mutating_process_fails_fast() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let w = workspace(ViewSpec::default());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "lock_holder_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LODE_TEST_LOCK_ROOT", w.ws.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        let line = lines.next().expect("child exited before locking").unwrap();
        if line.contains("LOCKED") {
            break;
        }
    }
    let start = Instant::now();
    let err = w.ws.lock().unwrap_err();
    assert!(matches!(err, WorkspaceError::InUse));
    assert!(err.to_string().contains("workspace is in use"));
    assert!(start.elapsed() < Duration::from_secs(2));
    // Readers do not need the lock.
    let mut reader = Workspace::open(w.ws.root()).unwrap();
    reader.scan(&ScanOptions::default()).unwrap();
    // A killed holder releases the lock.
    child.kill().unwrap();
    child.wait().unwrap();
    let _lock = w.ws.lock().unwrap();
}

#[test]
#[ignore = "benchmark: SPEC §10 status of 200,000 unchanged files under 5 s warm"]
fn status_of_200k_unchanged_files() {
    let mut w = workspace(ViewSpec::default());
    let root = w.ws.root().to_path_buf();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let mut rows = Vec::with_capacity(200_000);
    for d in 0..400 {
        let dir = root.join(format!("d{d:03}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..500 {
            let name = format!("d{d:03}/f{f:03}.txt");
            let content = name.as_bytes();
            let local = root.join(&name);
            fs::write(&local, content).unwrap();
            let file = fs::File::options().write(true).open(&local).unwrap();
            file.set_modified(old).unwrap();
            let (size, mtime) = stat_for_row(&local, SystemTime::now()).unwrap();
            let mut row =
                FileRow::synced(p(&name), blob_id(content).unwrap(), EntryKind::File, false);
            row.size = size;
            row.mtime_ns = mtime;
            rows.push(row);
        }
    }
    w.ws.db_mut()
        .write(|tx| {
            for r in &rows {
                tx.put_file(r)?;
            }
            Ok(())
        })
        .unwrap();
    // Warm the file system cache.
    w.ws.scan(&ScanOptions::default()).unwrap();
    let start = Instant::now();
    let report = w.ws.scan(&ScanOptions::default()).unwrap();
    let took = start.elapsed();
    assert_eq!(report.entries.len(), 200_000);
    assert_eq!(report.changed().count(), 0);
    eprintln!("status of 200,000 files: {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
}
