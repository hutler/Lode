//! Putting server content on disk safely (ARCHITECTURE §7).
//!
//! Every path is re-validated, may not touch `.lode/`, and is written by checking each
//! directory component with `symlink_metadata` and creating missing ones with `create_dir`,
//! so a pre-existing symlink is never followed. Content is downloaded into `.lode/tmp`,
//! verified against its id, then renamed into place.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures::StreamExt;
use lode_client::Client;
use lode_core::{DepotPath, EntryKind, ObjectId};
use lode_proto::read::ManifestEntry;
use lode_workspace::scan::stat_for_row;
use lode_workspace::{BlobCache, FileRow, Workspace};
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::output::Out;

/// Parallel downloads.
pub const PARALLEL_DOWNLOADS: usize = 8;

/// Largest symlink target accepted.
const MAX_SYMLINK: u64 = 64 * 1024;

/// Default cap of the shared download cache (`LODE_CACHE_DIR`).
const SHARED_CACHE_CAP: u64 = 10 * 1024 * 1024 * 1024;

fn unsafe_path(path: &DepotPath, why: &str) -> CliError {
    CliError::failed("unsafe_path", format!("refusing to write {path}: {why}"))
        .with_details(json!({ "paths": [path.as_str()] }))
}

/// The local path for `path`, with every parent directory present as a real directory
/// (created if missing). Fails if a parent is a symlink or a file, or if the path is inside
/// `.lode`.
pub fn prepare_target(root: &Path, path: &DepotPath) -> CliResult<PathBuf> {
    let segs: Vec<&str> = path.segments().collect();
    if segs
        .first()
        .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
    {
        return Err(unsafe_path(path, "it is inside .lode"));
    }
    let mut dir = root.to_path_buf();
    for seg in &segs[..segs.len().saturating_sub(1)] {
        dir.push(seg);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(unsafe_path(
                    path,
                    &format!("{} is a symlink", dir.display()),
                ))
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(CliError::failed(
                    "conflict",
                    format!("cannot write {path}: {} is a file", dir.display()),
                )
                .with_details(json!({ "paths": [path.as_str()] })))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&dir)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let target = dir.join(segs.last().copied().unwrap_or_default());
    if let Ok(m) = fs::symlink_metadata(&target) {
        if m.is_dir() && !m.file_type().is_symlink() {
            return Err(CliError::failed(
                "conflict",
                format!("cannot write {path}: a directory is in the way"),
            )
            .with_details(json!({ "paths": [path.as_str()] })));
        }
    }
    Ok(target)
}

/// Sets a file's executable bit (no-op where there is none).
#[cfg(unix)]
pub fn set_mode(file: &Path, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(file, fs::Permissions::from_mode(mode))
}

/// Sets a file's executable bit (no-op where there is none).
#[cfg(not(unix))]
pub fn set_mode(_file: &Path, _executable: bool) -> io::Result<()> {
    Ok(())
}

/// Moves a verified temporary file into place. Replaces a file or symlink there (the link
/// itself, never its target).
pub fn place_file(tmp: &Path, target: &Path, executable: bool) -> io::Result<()> {
    set_mode(tmp, executable)?;
    if let Ok(m) = fs::symlink_metadata(target) {
        if m.file_type().is_symlink() {
            fs::remove_file(target)?;
        } else {
            writable(target, &m)?;
        }
    }
    fs::rename(tmp, target)
}

/// Clears the read-only bit of a file about to be replaced or removed: Windows refuses to
/// do either to a read-only file, and lock-required files are kept read-only.
fn writable(file: &Path, meta: &fs::Metadata) -> io::Result<()> {
    if meta.is_file() && meta.permissions().readonly() {
        crate::cmd::locks::set_read_only(file, false)?;
    }
    Ok(())
}

/// Creates a symlink at `target` pointing to `link_to`, replacing a file or link there.
pub fn place_symlink(link_to: &str, target: &Path) -> io::Result<()> {
    if let Ok(m) = fs::symlink_metadata(target) {
        writable(target, &m)?;
        fs::remove_file(target)?;
    }
    make_symlink(link_to, target)
}

#[cfg(unix)]
fn make_symlink(link_to: &str, target: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link_to, target)
}

#[cfg(windows)]
fn make_symlink(link_to: &str, target: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(link_to.replace('/', "\\"), target).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{e} (creating symlinks on Windows needs Developer Mode or administrator rights)"
            ),
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn make_symlink(_link_to: &str, _target: &Path) -> io::Result<()> {
    Err(io::Error::other("symlinks are not supported here"))
}

/// Removes `path` from disk and then its parent directories that became empty, up to the
/// root. A missing file is fine.
pub fn remove_and_prune(root: &Path, path: &DepotPath) -> io::Result<()> {
    let local = lode_workspace::paths::to_local(root, path);
    if let Ok(m) = fs::symlink_metadata(&local) {
        writable(&local, &m)?;
    }
    match fs::remove_file(&local) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut dir = local.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            break;
        }
        // Only empty directories go; anything else stops the walk.
        if fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    Ok(())
}

/// How a sync went.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fetched {
    /// Files placed.
    pub files: u64,
    /// Bytes placed.
    pub bytes: u64,
    /// Distinct blobs downloaded from the server (not counting the shared cache).
    pub downloaded: u64,
}

/// The shared download cache, if `LODE_CACHE_DIR` names one.
fn shared_cache() -> Option<BlobCache> {
    std::env::var_os("LODE_CACHE_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| BlobCache::new(PathBuf::from(d), SHARED_CACHE_CAP))
}

/// Test hook: exit abruptly after placing this many files, as if killed.
fn exit_after() -> Option<u64> {
    std::env::var("LODE_TEST_EXIT_AFTER_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
}

/// Whether the file on disk already is `entry` (so a resumed sync can skip it).
fn already_there(ws: &Workspace, entry: &ManifestEntry) -> CliResult<bool> {
    let Some(row) = ws.db().file(&entry.path)? else {
        return Ok(false);
    };
    if row.base_id != Some(entry.id) || row.kind != entry.kind {
        return Ok(false);
    }
    let local = ws.local_path(&entry.path);
    let Ok(meta) = fs::symlink_metadata(&local) else {
        return Ok(false);
    };
    if entry.kind == EntryKind::Symlink {
        return Ok(meta.file_type().is_symlink());
    }
    if !meta.is_file() || meta.len() != entry.size {
        return Ok(false);
    }
    Ok(lode_core::blob_id(fs::File::open(&local)?)? == entry.id)
}

/// Downloads and places `entries`, recording a row for each as soon as it is on disk, so
/// an interrupted run resumes where it stopped. Entries already present are skipped.
pub async fn fetch_entries(
    ws: &mut Workspace,
    client: &Client,
    depot: &str,
    entries: Vec<ManifestEntry>,
    out: &Out,
) -> CliResult<Fetched> {
    let root = ws.root().to_path_buf();
    let tmp_dir = ws.layout().tmp();
    fs::create_dir_all(&tmp_dir)?;
    let cache = shared_cache();
    let mut todo: BTreeMap<ObjectId, Vec<ManifestEntry>> = BTreeMap::new();
    let mut links = Vec::new();
    let mut seen = HashSet::new();
    for e in entries {
        if e.kind == EntryKind::Dir || !seen.insert(e.path.clone()) {
            continue;
        }
        // Check the path before downloading anything for it.
        if e.path
            .segments()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
        {
            return Err(unsafe_path(&e.path, "it is inside .lode"));
        }
        if already_there(ws, &e)? {
            continue;
        }
        match e.kind {
            EntryKind::Symlink => links.push(e),
            _ => todo.entry(e.id).or_default().push(e),
        }
    }
    let total = todo.values().map(|v| v.len() as u64).sum::<u64>() + links.len() as u64;
    let mut progress = out.progress("Fetching", total);
    let mut stats = Fetched::default();
    let stop_after = exit_after();
    let now = SystemTime::now();

    let record = |ws: &mut Workspace,
                  e: &ManifestEntry,
                  target: &Path,
                  stats: &mut Fetched|
     -> CliResult<()> {
        let mut row = FileRow::synced(e.path.clone(), e.id, e.kind, e.executable);
        let (size, mtime) = stat_for_row(target, now)?;
        row.size = size;
        row.mtime_ns = mtime;
        ws.db_mut().write(|tx| tx.put_file(&row))?;
        stats.files += 1;
        stats.bytes += e.size;
        if stop_after.is_some_and(|n| stats.files >= n) {
            // Simulates a kill for the resume test: no cleanup, no destructors.
            std::process::exit(9);
        }
        Ok(())
    };

    for e in links {
        let mut target_text = Vec::new();
        if e.size > MAX_SYMLINK {
            return Err(CliError::failed(
                "protocol",
                format!("symlink {} is too long", e.path),
            ));
        }
        client.get_blob_to(depot, &e.id, &mut target_text).await?;
        let text = String::from_utf8(target_text).map_err(|_| {
            CliError::failed("protocol", format!("symlink {} is not UTF-8", e.path))
        })?;
        let target = prepare_target(&root, &e.path)?;
        place_symlink(&text, &target)?;
        record(ws, &e, &target, &mut stats)?;
        progress.step(e.size);
    }

    let downloads = futures::stream::iter(todo.into_iter().map(|(id, group)| {
        let client = client.clone();
        let tmp_dir = tmp_dir.clone();
        let cache = cache.clone();
        let depot = depot.to_owned();
        async move {
            let tmp = tmp_dir.join(format!("{}-{}", id.to_hex(), std::process::id()));
            let mut from_server = false;
            let hit = match &cache {
                Some(c) => c.get(&id)?,
                None => None,
            };
            match hit {
                Some(cached) => {
                    fs::copy(&cached, &tmp)?;
                }
                None => {
                    client.download_blob(&depot, &id, &tmp).await?;
                    from_server = true;
                    if let Some(c) = &cache {
                        if let Err(e) = c.insert_file(&id, &tmp) {
                            eprintln!("warning: shared cache: {e}");
                        }
                    }
                }
            }
            Ok::<_, CliError>((tmp, group, from_server))
        }
    }))
    .buffer_unordered(PARALLEL_DOWNLOADS);
    futures::pin_mut!(downloads);
    while let Some(done) = downloads.next().await {
        let (tmp, group, from_server) = done?;
        if from_server {
            stats.downloaded += 1;
        }
        let last = group.len().saturating_sub(1);
        for (i, e) in group.iter().enumerate() {
            let target = prepare_target(&root, &e.path)?;
            if i == last {
                place_file(&tmp, &target, e.executable)?;
            } else {
                let copy = tmp.with_extension(format!("copy{i}"));
                fs::copy(&tmp, &copy)?;
                place_file(&copy, &target, e.executable)?;
            }
            record(ws, e, &target, &mut stats)?;
            progress.step(e.size);
        }
    }
    progress.finish();
    Ok(stats)
}
