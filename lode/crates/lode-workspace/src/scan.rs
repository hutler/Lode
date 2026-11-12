//! The status scan (ARCHITECTURE §10.2): walk the workspace, compare with the `file` rows,
//! hash only what size and mtime cannot vouch for.
//!
//! The walk and the hashing run on a small pool of threads. Symlinks are reported as
//! symlinks and never followed. `.lode/` at the root is never entered.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use lode_core::{DepotPath, EntryKind, ObjectId};

use crate::db::{FileRow, FileState, WorkspaceDb};
use crate::error::Result;
use crate::ignore_rules::IgnoreStack;
use crate::layout::{DOT_LODE, IGNORE_FILE};
use crate::view::View;

/// A file whose mtime is this close to "now" may still be written to within the same
/// timestamp tick; its stat is not trusted to prove it unchanged later.
pub const RACY_WINDOW: Duration = Duration::from_secs(2);

/// What to scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Only these files and directories; empty means the whole workspace.
    pub paths: Vec<DepotPath>,
    /// Also report ignored untracked files, as [`Status::Ignored`] (for `clean --ignored`).
    pub report_ignored: bool,
    /// Worker threads; 0 picks from the machine's parallelism.
    pub threads: usize,
    /// The current time, for the racy-mtime rule.
    pub now: SystemTime,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            report_ignored: false,
            threads: 0,
            now: SystemTime::now(),
        }
    }
}

/// The status of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Tracked and identical to its base (content, kind and executable bit).
    Unchanged,
    /// Tracked and different from its base.
    Modified,
    /// Added locally and present.
    Added,
    /// Marked for deletion.
    Deleted,
    /// Tracked (or added) but gone from disk without `rm`.
    Missing,
    /// On disk, not tracked, not ignored.
    Untracked,
    /// Left in conflict by an update or merge.
    Conflicted,
    /// On disk, not tracked, ignored (only with [`ScanOptions::report_ignored`]).
    Ignored,
}

impl Status {
    /// The one-letter form of `lode status`.
    pub fn letter(self) -> char {
        match self {
            Status::Unchanged => ' ',
            Status::Modified => 'M',
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Missing => '!',
            Status::Untracked => '?',
            Status::Conflicted => 'C',
            Status::Ignored => 'I',
        }
    }
}

/// What the disk holds at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    /// `File` or `Symlink`.
    pub kind: EntryKind,
    /// Executable bit; on platforms without one, the row's value (or false).
    pub executable: bool,
    /// Size in bytes (a symlink: its target's length).
    pub size: u64,
    /// Modification time, ns since the epoch.
    pub mtime_ns: i64,
    /// Content id, when the scan knows it: hashed, or the base id vouched for by stat.
    /// `None` for untracked files (not hashed) and for files that kept changing while being
    /// hashed.
    pub id: Option<ObjectId>,
}

/// One scanned path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    /// Depot path.
    pub path: DepotPath,
    /// Status.
    pub status: Status,
    /// The row, for tracked paths.
    pub row: Option<FileRow>,
    /// The disk, when something is there.
    pub disk: Option<DiskInfo>,
}

/// A local name that cannot be a depot path. Reported, never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidName {
    /// The local path.
    pub local: PathBuf,
    /// The rule it breaks.
    pub reason: String,
}

/// The result of a scan.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    /// Every tracked path in scope, and every untracked (and, if asked, ignored) one, in
    /// path order.
    pub entries: Vec<ScanEntry>,
    /// Local names that are not valid depot paths; their files are skipped.
    pub invalid: Vec<InvalidName>,
    /// Problems that did not stop the scan, such as an unreadable directory.
    pub warnings: Vec<String>,
    /// Rows whose stored size and mtime were refreshed.
    pub refreshed: usize,
}

impl ScanReport {
    /// Entries that are not unchanged.
    pub fn changed(&self) -> impl Iterator<Item = &ScanEntry> {
        self.entries
            .iter()
            .filter(|e| e.status != Status::Unchanged)
    }

    /// The entry for `path`.
    pub fn get(&self, path: &DepotPath) -> Option<&ScanEntry> {
        self.entries
            .binary_search_by(|e| e.path.cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }
}

/// Size, mtime and kind of a `symlink_metadata` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    kind: EntryKind,
    exec: Option<bool>,
    size: u64,
    mtime_ns: i64,
}

fn mtime_ns(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

fn stat_of(meta: &Metadata) -> Stat {
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    Stat {
        kind,
        exec: exec_bit(meta, kind),
        size: meta.len(),
        mtime_ns: meta.modified().map(mtime_ns).unwrap_or(0),
    }
}

#[cfg(unix)]
fn exec_bit(meta: &Metadata, kind: EntryKind) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    match kind {
        EntryKind::File => Some(meta.permissions().mode() & 0o100 != 0),
        _ => Some(false),
    }
}

#[cfg(not(unix))]
fn exec_bit(_meta: &Metadata, _kind: EntryKind) -> Option<bool> {
    None
}

/// The stored mtime for a file seen unchanged at `stat`: the real one, or 0 when it is too
/// recent to trust (so the next scan hashes it again).
pub fn trusted_mtime(mtime: i64, now: SystemTime) -> i64 {
    let limit = mtime_ns(now) - i64::try_from(RACY_WINDOW.as_nanos()).unwrap_or(0);
    if mtime > limit {
        0
    } else {
        mtime
    }
}

/// The size and trusted mtime to record for the file at `local` (after writing it, for
/// example).
pub fn stat_for_row(local: &Path, now: SystemTime) -> io::Result<(u64, i64)> {
    let s = stat_of(&fs::symlink_metadata(local)?);
    Ok((s.size, trusted_mtime(s.mtime_ns, now)))
}

/// Content id of a symlink: BLAKE3 of its target with `/` separators.
pub fn symlink_id(local: &Path) -> io::Result<(ObjectId, u64)> {
    let target = fs::read_link(local)?;
    let text = target
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "symlink target is not UTF-8"))?;
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_owned()
    };
    Ok((lode_core::blob_id(text.as_bytes())?, text.len() as u64))
}

/// Which paths a scan covers.
struct Scope {
    prefixes: Vec<String>,
}

impl Scope {
    fn file(&self, p: &str) -> bool {
        self.prefixes.is_empty()
            || self
                .prefixes
                .iter()
                .any(|s| p == s || (p.starts_with(s.as_str()) && p.as_bytes()[s.len()] == b'/'))
    }

    /// Whether a directory may hold something in scope.
    fn dir(&self, d: &str) -> bool {
        self.file(d)
            || self
                .prefixes
                .iter()
                .any(|s| s.starts_with(d) && s.as_bytes().get(d.len()) == Some(&b'/'))
    }
}

struct Found {
    path: DepotPath,
    abs: PathBuf,
    stat: Stat,
    ignored: bool,
}

#[derive(Default)]
struct WalkOut {
    found: Vec<Found>,
    invalid: Vec<InvalidName>,
    warnings: Vec<String>,
}

struct DirJob {
    abs: PathBuf,
    rel: String,
    ignore: Arc<IgnoreStack>,
    ignored: bool,
}

struct Walk<'a> {
    scope: &'a Scope,
    tracked_files: &'a HashSet<&'a str>,
    tracked_dirs: &'a HashSet<&'a str>,
    report_ignored: bool,
}

struct Queue {
    jobs: Vec<DirJob>,
    active: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A worker that panicked cannot leave the queue half-updated in a way that matters.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Walk<'_> {
    fn run(&self, root: &Path, threads: usize) -> WalkOut {
        let queue = Mutex::new(Queue {
            jobs: vec![DirJob {
                abs: root.to_path_buf(),
                rel: String::new(),
                ignore: IgnoreStack::empty(),
                ignored: false,
            }],
            active: 0,
        });
        let cv = Condvar::new();
        let outs = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut out = WalkOut::default();
                    loop {
                        let job = {
                            let mut q = lock(&queue);
                            loop {
                                if let Some(j) = q.jobs.pop() {
                                    q.active += 1;
                                    break Some(j);
                                }
                                if q.active == 0 {
                                    break None;
                                }
                                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                            }
                        };
                        let Some(job) = job else {
                            cv.notify_all();
                            break;
                        };
                        let subdirs = self.dir(job, &mut out);
                        let mut q = lock(&queue);
                        q.jobs.extend(subdirs);
                        q.active -= 1;
                        cv.notify_all();
                    }
                    lock(&outs).push(out);
                });
            }
        });
        let mut all = WalkOut::default();
        for o in outs.into_inner().unwrap_or_else(|e| e.into_inner()) {
            all.found.extend(o.found);
            all.invalid.extend(o.invalid);
            all.warnings.extend(o.warnings);
        }
        all
    }

    fn dir(&self, job: DirJob, out: &mut WalkOut) -> Vec<DirJob> {
        let read = match fs::read_dir(&job.abs) {
            Ok(r) => r,
            Err(e) => {
                out.warnings
                    .push(format!("cannot read {}: {e}", job.abs.display()));
                return Vec::new();
            }
        };
        let mut entries = Vec::new();
        let mut has_ignore_file = false;
        for e in read {
            match e {
                Ok(e) => {
                    if e.file_name() == IGNORE_FILE {
                        has_ignore_file = true;
                    }
                    entries.push(e);
                }
                Err(e) => out
                    .warnings
                    .push(format!("cannot read {}: {e}", job.abs.display())),
            }
        }
        let stack = if job.ignored {
            job.ignore.clone()
        } else {
            let (stack, problem) = IgnoreStack::enter(&job.ignore, &job.abs, has_ignore_file);
            out.warnings.extend(problem);
            stack
        };
        let mut subdirs = Vec::new();
        for e in entries {
            let name = e.file_name();
            if job.rel.is_empty() && name == DOT_LODE {
                continue;
            }
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(err) => {
                    out.warnings
                        .push(format!("cannot stat {}: {err}", e.path().display()));
                    continue;
                }
            };
            let abs = e.path();
            let is_dir = ft.is_dir();
            if !is_dir && !ft.is_file() && !ft.is_symlink() {
                // Sockets, pipes and devices are not content.
                continue;
            }
            let ignored = job.ignored || stack.is_ignored(&abs, is_dir);
            let Some(name) = name.to_str() else {
                if !ignored {
                    out.invalid.push(InvalidName {
                        local: abs,
                        reason: "the name is not valid UTF-8".into(),
                    });
                }
                continue;
            };
            let rel = if job.rel.is_empty() {
                name.to_owned()
            } else {
                format!("{}/{name}", job.rel)
            };
            let path = match DepotPath::parse(&rel) {
                Ok(p) => p,
                Err(err) => {
                    if !ignored {
                        out.invalid.push(InvalidName {
                            local: abs,
                            reason: err.to_string(),
                        });
                    }
                    continue;
                }
            };
            if is_dir {
                let tracked_below = self.tracked_dirs.contains(path.as_str());
                if !self.scope.dir(path.as_str())
                    || (ignored && !self.report_ignored && !tracked_below)
                {
                    continue;
                }
                subdirs.push(DirJob {
                    abs,
                    rel: path.as_str().to_owned(),
                    ignore: stack.clone(),
                    ignored,
                });
                continue;
            }
            if !self.scope.file(path.as_str()) {
                continue;
            }
            if ignored && !self.report_ignored && !self.tracked_files.contains(path.as_str()) {
                continue;
            }
            match e.metadata() {
                Ok(meta) => out.found.push(Found {
                    path,
                    abs,
                    stat: stat_of(&meta),
                    ignored,
                }),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => out
                    .warnings
                    .push(format!("cannot stat {}: {err}", abs.display())),
            }
        }
        subdirs
    }
}

/// A tracked file whose content must be hashed to know its status.
struct HashJob {
    entry: usize,
    abs: PathBuf,
    stat: Stat,
}

enum Hashed {
    Id(ObjectId, Stat),
    /// The file kept changing while it was hashed.
    Unstable(Stat),
    Gone,
    Failed(String),
}

fn hash_one(abs: &Path, mut stat: Stat) -> Hashed {
    if stat.kind == EntryKind::Symlink {
        return match symlink_id(abs) {
            Ok((id, _)) => Hashed::Id(id, stat),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Hashed::Gone,
            Err(e) => Hashed::Failed(format!("{}: {e}", abs.display())),
        };
    }
    for _ in 0..3 {
        let result = File::open(abs)
            .and_then(|f| lode_core::blob_id(&f))
            .and_then(|id| Ok((id, stat_of(&fs::symlink_metadata(abs)?))));
        match result {
            Ok((id, after)) => {
                if after == stat {
                    return Hashed::Id(id, stat);
                }
                // Changed while being read: the hash may mix old and new bytes.
                stat = after;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Hashed::Gone,
            Err(e) => return Hashed::Failed(format!("{}: {e}", abs.display())),
        }
    }
    Hashed::Unstable(stat)
}

fn hash_all(jobs: &[HashJob], threads: usize) -> Vec<Hashed> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Hashed)>> = Mutex::new(Vec::with_capacity(jobs.len()));
    std::thread::scope(|s| {
        for _ in 0..threads.min(jobs.len()).max(1) {
            s.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    local.push((i, hash_one(&job.abs, job.stat)));
                }
                lock(&results).extend(local);
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|r| r.0);
    results.into_iter().map(|r| r.1).collect()
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16)
}

fn disk_info(stat: &Stat, row: Option<&FileRow>, id: Option<ObjectId>) -> DiskInfo {
    DiskInfo {
        kind: stat.kind,
        executable: stat
            .exec
            .unwrap_or_else(|| row.is_some_and(|r| r.executable)),
        size: stat.size,
        mtime_ns: stat.mtime_ns,
        id,
    }
}

/// Scans the workspace at `root` against `db`, honouring `.lodeignore` files and `view`.
/// Refreshes the stored size and mtime of files found unchanged after hashing.
pub fn scan(
    root: &Path,
    db: &mut WorkspaceDb,
    view: &View,
    opts: &ScanOptions,
) -> Result<ScanReport> {
    let threads = if opts.threads == 0 {
        default_threads()
    } else {
        opts.threads
    };
    let scope = Scope {
        prefixes: opts.paths.iter().map(|p| p.as_str().to_owned()).collect(),
    };
    let rows: Vec<FileRow> = db
        .files()?
        .into_iter()
        .filter(|r| scope.file(r.path.as_str()))
        .collect();
    let tracked_files: HashSet<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    let tracked_dirs: HashSet<&str> = rows.iter().flat_map(|r| r.path.ancestors()).collect();

    let walk = Walk {
        scope: &scope,
        tracked_files: &tracked_files,
        tracked_dirs: &tracked_dirs,
        report_ignored: opts.report_ignored,
    };
    let out = walk.run(root, threads);
    let mut report = ScanReport {
        invalid: out.invalid,
        warnings: out.warnings,
        ..Default::default()
    };
    let mut found: HashMap<DepotPath, Found> = HashMap::with_capacity(out.found.len());
    for f in out.found {
        if let Some(prev) = found.get(&f.path) {
            // Two local names that normalize to one depot path (NFC and NFD forms).
            report.invalid.push(InvalidName {
                local: f.abs.clone(),
                reason: format!("same depot path as {}", prev.abs.display()),
            });
            continue;
        }
        found.insert(f.path.clone(), f);
    }

    let mut entries: Vec<ScanEntry> = Vec::with_capacity(rows.len() + 16);
    let mut jobs = Vec::new();
    for row in rows {
        let disk = found.remove(&row.path);
        let (status, disk_info_opt) = match (row.state, &disk) {
            (FileState::Deleted, d) => (
                Status::Deleted,
                d.as_ref().map(|d| disk_info(&d.stat, Some(&row), None)),
            ),
            (FileState::Conflicted, d) => (
                Status::Conflicted,
                d.as_ref().map(|d| disk_info(&d.stat, Some(&row), None)),
            ),
            (FileState::Added, None) | (FileState::Normal, None) => (Status::Missing, None),
            // Added content is hashed by commit, not by every status.
            (FileState::Added, Some(d)) => {
                (Status::Added, Some(disk_info(&d.stat, Some(&row), None)))
            }
            (FileState::Normal, Some(d)) => {
                let same_stat = row.mtime_ns != 0
                    && d.stat.kind == row.kind
                    && d.stat.size == row.size
                    && d.stat.mtime_ns == row.mtime_ns;
                if same_stat {
                    let info = disk_info(&d.stat, Some(&row), row.base_id);
                    let status = if info.executable == row.executable {
                        Status::Unchanged
                    } else {
                        Status::Modified
                    };
                    (status, Some(info))
                } else {
                    jobs.push(HashJob {
                        entry: entries.len(),
                        abs: d.abs.clone(),
                        stat: d.stat,
                    });
                    // Decided after hashing.
                    (Status::Modified, Some(disk_info(&d.stat, Some(&row), None)))
                }
            }
        };
        entries.push(ScanEntry {
            path: row.path.clone(),
            status,
            row: Some(row),
            disk: disk_info_opt,
        });
    }

    let mut refresh: Vec<(DepotPath, u64, i64)> = Vec::new();
    let hashed = hash_all(&jobs, threads);
    for (job, h) in jobs.iter().zip(hashed) {
        let e = &mut entries[job.entry];
        let Some(row) = e.row.as_ref() else { continue };
        match h {
            Hashed::Gone => {
                e.status = Status::Missing;
                e.disk = None;
            }
            Hashed::Failed(msg) => report.warnings.push(msg),
            Hashed::Unstable(stat) => {
                e.disk = Some(disk_info(&stat, Some(row), None));
                report
                    .warnings
                    .push(format!("{} kept changing while it was read", e.path));
            }
            Hashed::Id(id, stat) => {
                let info = disk_info(&stat, Some(row), Some(id));
                if row.state == FileState::Normal {
                    let same_content = row.base_id == Some(id) && stat.kind == row.kind;
                    e.status = if same_content && info.executable == row.executable {
                        Status::Unchanged
                    } else {
                        Status::Modified
                    };
                    if same_content {
                        let mtime = trusted_mtime(stat.mtime_ns, opts.now);
                        if (stat.size, mtime) != (row.size, row.mtime_ns) {
                            refresh.push((e.path.clone(), stat.size, mtime));
                        }
                    }
                }
                e.disk = Some(info);
            }
        }
    }

    for (path, f) in found {
        let in_view = view.contains(&path);
        let status = if f.ignored {
            Status::Ignored
        } else if in_view {
            Status::Untracked
        } else {
            continue;
        };
        if status == Status::Ignored && !opts.report_ignored {
            continue;
        }
        entries.push(ScanEntry {
            disk: Some(disk_info(&f.stat, None, None)),
            path,
            status,
            row: None,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    report.entries = entries;
    report.invalid.sort_by(|a, b| a.local.cmp(&b.local));
    report.warnings.sort();

    if !refresh.is_empty() {
        db.write(|tx| {
            for (p, size, mtime) in &refresh {
                tx.set_stat(p, *size, *mtime)?;
            }
            Ok(())
        })?;
        report.refreshed = refresh.len();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_file_that_changed_while_hashed_is_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        fs::write(&f, b"now").unwrap();
        let real = stat_of(&fs::symlink_metadata(&f).unwrap());
        // As if the file had been stat'ed before a write: the size and mtime differ.
        let stale = Stat {
            size: 1,
            mtime_ns: 5,
            ..real
        };
        match hash_one(&f, stale) {
            Hashed::Id(id, stat) => {
                assert_eq!(id, lode_core::blob_id(&b"now"[..]).unwrap());
                assert_eq!(stat, real);
            }
            _ => panic!("expected an id"),
        }
        fs::remove_file(&f).unwrap();
        assert!(matches!(hash_one(&f, real), Hashed::Gone));
    }

    #[test]
    fn scope_matches_whole_segments() {
        let s = Scope {
            prefixes: vec!["a/b".into()],
        };
        assert!(s.file("a/b"));
        assert!(s.file("a/b/c"));
        assert!(!s.file("a/bc"));
        assert!(s.dir("a"));
        assert!(!s.dir("ab"));
        assert!(!s.dir("x"));
    }
}
