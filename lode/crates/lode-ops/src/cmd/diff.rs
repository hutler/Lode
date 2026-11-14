//! `lode diff [paths] [--stat]`: unified diff of local changes against their base content.
//!
//! The output is a git-style unified diff (`--- a/x`, `+++ b/x`, `/dev/null` for adds and
//! deletes) that `patch -p1` applies. Base content comes from the blob cache or the server.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use lode_core::{DepotPath, EntryKind, LodeConfig, ObjectId};
use lode_workspace::classify::{class_by_config, class_by_content};
use lode_workspace::{FileClass, FileRow, FileState, ScanEntry, Status};
use serde_json::{json, Value};

use crate::cmd::history::{self, Source};
use crate::cmd::status::status_name;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, Bases};

/// Files larger than this are reported like binary files, with sizes only.
pub const TEXT_LIMIT: u64 = 16 * 1024 * 1024;

/// `lode diff` options.
#[derive(Debug, Default, clap::Args)]
pub struct DiffArgs {
    /// Only these files and directories.
    pub paths: Vec<PathBuf>,
    /// Show changed line counts per file instead of the diff.
    #[arg(long)]
    pub stat: bool,
}

/// One side of a diff.
struct Side {
    path: String,
    kind: EntryKind,
    executable: bool,
    /// Content, unless the file is too large to diff.
    bytes: Option<Vec<u8>>,
    size: u64,
}

impl Side {
    fn mode(&self) -> &'static str {
        match (self.kind, self.executable) {
            (EntryKind::Symlink, _) => "120000",
            (_, true) => "100755",
            _ => "100644",
        }
    }
}

/// One file's diff.
struct FileDiff {
    path: String,
    status: Status,
    old: Option<Side>,
    new: Option<Side>,
    copied: Option<bool>,
    binary: bool,
    insertions: usize,
    deletions: usize,
    patch: Vec<u8>,
}

fn read_disk(local: &std::path::Path, kind: EntryKind) -> OpResult<(Option<Vec<u8>>, u64)> {
    if kind == EntryKind::Symlink {
        let target = fs::read_link(local)?;
        let text = target.to_string_lossy().replace('\\', "/").into_bytes();
        let n = text.len() as u64;
        return Ok((Some(text), n));
    }
    let size = fs::metadata(local)?.len();
    if size > TEXT_LIMIT {
        return Ok((None, size));
    }
    let bytes = fs::read(local)?;
    let n = bytes.len() as u64;
    Ok((Some(bytes), n))
}

async fn base_side(
    bases: &mut Bases<'_>,
    path: &str,
    row: &FileRow,
    id: ObjectId,
    fetch: bool,
) -> OpResult<Side> {
    let (bytes, size) = if fetch {
        let b = bases.read(&id).await?;
        let n = b.len() as u64;
        (Some(b), n)
    } else {
        (None, row.size)
    };
    Ok(Side {
        path: path.to_owned(),
        kind: row.kind,
        executable: row.executable,
        bytes,
        size,
    })
}

fn is_binary(config: Option<&LodeConfig>, e: &ScanEntry, sides: &[Option<&Side>]) -> bool {
    if let Some(c) = class_by_config(&e.path, config) {
        return c == FileClass::Binary;
    }
    sides.iter().flatten().any(|s| match &s.bytes {
        None => true,
        Some(b) => class_by_content(b) == FileClass::Binary,
    })
}

fn render(d: &mut FileDiff, binary: bool) {
    let mut out = Vec::new();
    let a = d.old.as_ref().map_or(d.path.as_str(), |s| s.path.as_str());
    let b = d.new.as_ref().map_or(d.path.as_str(), |s| s.path.as_str());
    let _ = writeln!(out, "diff --lode a/{a} b/{b}");
    match (&d.old, &d.new) {
        (None, Some(n)) => {
            let _ = writeln!(out, "new file mode {}", n.mode());
        }
        (Some(o), None) => {
            let _ = writeln!(out, "deleted file mode {}", o.mode());
        }
        (Some(o), Some(n)) => {
            if o.mode() != n.mode() {
                let _ = writeln!(out, "old mode {}\nnew mode {}", o.mode(), n.mode());
            }
            if let Some(copied) = d.copied {
                let verb = if copied { "copy" } else { "rename" };
                let _ = writeln!(out, "{verb} from {a}\n{verb} to {b}");
            }
        }
        (None, None) => {}
    }
    let from = d
        .old
        .as_ref()
        .map_or("/dev/null".to_owned(), |_| format!("a/{a}"));
    let to = d
        .new
        .as_ref()
        .map_or("/dev/null".to_owned(), |_| format!("b/{b}"));
    if binary {
        let size = |s: &Option<Side>| s.as_ref().map_or(0, |s| s.size);
        let same = d.old.as_ref().and_then(|s| s.bytes.as_ref())
            == d.new.as_ref().and_then(|s| s.bytes.as_ref())
            && d.old.as_ref().is_some_and(|s| s.bytes.is_some());
        if !same {
            let _ = writeln!(
                out,
                "Binary files {from} and {to} differ ({} -> {} bytes)",
                size(&d.old),
                size(&d.new)
            );
        }
        d.patch = out;
        return;
    }
    let empty = Vec::new();
    let old = d
        .old
        .as_ref()
        .and_then(|s| s.bytes.as_ref())
        .unwrap_or(&empty);
    let new = d
        .new
        .as_ref()
        .and_then(|s| s.bytes.as_ref())
        .unwrap_or(&empty);
    let mut opts = diffy::DiffOptions::new();
    opts.set_original_filename(from).set_modified_filename(to);
    let patch = opts.create_patch_bytes(old, new);
    for h in patch.hunks() {
        for l in h.lines() {
            match l {
                diffy::Line::Insert(_) => d.insertions += 1,
                diffy::Line::Delete(_) => d.deletions += 1,
                diffy::Line::Context(_) => {}
            }
        }
    }
    if !patch.hunks().is_empty() {
        let _ = diffy::PatchFormatter::new()
            .suppress_blank_empty(false)
            .write_patch_into(&patch, &mut out);
    }
    d.patch = out;
}

/// The `.lodeconfig` on disk, if it parses.
fn local_config(root: &std::path::Path) -> Option<LodeConfig> {
    let bytes = fs::read(root.join(".lodeconfig")).ok()?;
    LodeConfig::parse(&bytes).ok()
}

/// Runs `lode diff`.
pub async fn diff(ctx: &Ctx, args: DiffArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    let report = local::scan(ctx, &mut ws, &paths, false)?;
    let rows: HashMap<String, FileRow> = ws
        .db()
        .files()?
        .into_iter()
        .map(|r| (r.path.as_str().to_owned(), r))
        .collect();
    let config = local_config(ws.root());
    let mut bases = Bases::new(ctx, &ws)?;
    let mut diffs = Vec::new();
    for e in report.changed() {
        let Some(row) = &e.row else { continue };
        let path = e.path.as_str().to_owned();
        let new = match (&e.disk, e.status) {
            (Some(d), s) if s != Status::Deleted => {
                let (bytes, size) = read_disk(&ws.local_path(&e.path), d.kind)?;
                Some(Side {
                    path: path.clone(),
                    kind: d.kind,
                    executable: d.executable,
                    bytes,
                    size,
                })
            }
            _ => None,
        };
        let fetch = !matches!(&new, Some(n) if n.bytes.is_none());
        let mut copied = None;
        let old = if row.state == FileState::Added {
            match row
                .copy_from
                .as_ref()
                .and_then(|c| rows.get(c.path.as_str()))
            {
                Some(src) => match src.base_id {
                    Some(id) => {
                        copied = Some(src.state != FileState::Deleted);
                        Some(base_side(&mut bases, src.path.as_str(), src, id, fetch).await?)
                    }
                    None => None,
                },
                None => None,
            }
        } else {
            match row.base_id {
                Some(id) => Some(base_side(&mut bases, &path, row, id, fetch).await?),
                None => None,
            }
        };
        let binary = is_binary(config.as_ref(), e, &[old.as_ref(), new.as_ref()]);
        let mut d = FileDiff {
            path,
            status: e.status,
            old,
            new,
            copied,
            binary,
            insertions: 0,
            deletions: 0,
            patch: Vec::new(),
        };
        render(&mut d, binary);
        diffs.push(d);
    }

    let (ins, del): (usize, usize) = diffs
        .iter()
        .fold((0, 0), |(i, d), f| (i + f.insertions, d + f.deletions));
    if !ctx.g.json {
        let mut stdout = Vec::new();
        if args.stat {
            let width = diffs.iter().map(|d| d.path.len()).max().unwrap_or(0);
            for d in &diffs {
                let detail = if d.binary {
                    let size = |s: &Option<Side>| s.as_ref().map_or(0, |s| s.size);
                    format!("Bin {} -> {} bytes", size(&d.old), size(&d.new))
                } else {
                    let n = d.insertions + d.deletions;
                    let scale = |x: usize| if n > 50 { (x * 50).div_ceil(n) } else { x };
                    format!(
                        "{n} {}{}",
                        "+".repeat(scale(d.insertions)),
                        "-".repeat(scale(d.deletions))
                    )
                };
                writeln!(stdout, " {:width$} | {detail}", d.path)?;
            }
            writeln!(
                stdout,
                " {} file{} changed, {ins} insertion{}(+), {del} deletion{}(-)",
                diffs.len(),
                if diffs.len() == 1 { "" } else { "s" },
                if ins == 1 { "" } else { "s" },
                if del == 1 { "" } else { "s" },
            )?;
        } else {
            for d in &diffs {
                stdout.extend_from_slice(&d.patch);
            }
        }
        ctx.out.raw(&stdout)?;
    }
    let files: Vec<Value> = diffs.iter().map(|d| entry(d, !args.stat)).collect();
    Ok(json!({ "files": files, "insertions": ins, "deletions": del }))
}

/// One file of `lode diff --json`.
fn entry(d: &FileDiff, patch: bool) -> Value {
    let mut v = json!({
        "path": d.path,
        "status": status_name(d.status),
        "old_path": d.old.as_ref().map(|s| &s.path),
        "binary": d.binary,
        "old_size": d.old.as_ref().map(|s| s.size),
        "new_size": d.new.as_ref().map(|s| s.size),
        "old_mode": d.old.as_ref().map(Side::mode),
        "new_mode": d.new.as_ref().map(Side::mode),
        "insertions": d.insertions,
        "deletions": d.deletions,
    });
    if patch {
        v["patch"] = json!(String::from_utf8_lossy(&d.patch));
    }
    v
}

/// Two stored versions of a file to compare. Each is `path@rev` (`@12` or `@r12`; without a
/// revision, the workspace's), and either may be absent: no old version is an added file, no
/// new version a deleted one.
#[derive(Debug, Default)]
pub struct VersionsArgs {
    pub old: Option<String>,
    pub new: Option<String>,
    pub source: Source,
}

/// The diff of two stored versions of a file, as one file of `lode diff --json` (with the
/// patch). Lode Desk shows a revision's changes with it; no command uses it yet.
pub async fn diff_versions(ctx: &Ctx, args: VersionsArgs) -> OpResult<Value> {
    let t = history::target(ctx, &args.source)?;
    let mut old = None;
    let mut new = None;
    let mut depot_path = None;
    if let Some(spec) = &args.old {
        let (side, p) = stored(ctx, &t, spec).await?;
        old = Some(side);
        depot_path = Some(p);
    }
    if let Some(spec) = &args.new {
        let (side, p) = stored(ctx, &t, spec).await?;
        new = Some(side);
        depot_path = Some(p);
    }
    let Some(depot_path) = depot_path else {
        return Err(OpError::usage("name the old or the new version of a file"));
    };
    let config = t.root.as_deref().and_then(local_config);
    let binary = match class_by_config(&depot_path, config.as_ref()) {
        Some(c) => c == FileClass::Binary,
        None => [old.as_ref(), new.as_ref()].iter().flatten().any(|s| {
            s.bytes
                .as_ref()
                .is_none_or(|b| class_by_content(b) == FileClass::Binary)
        }),
    };
    let status = match (&old, &new) {
        (None, _) => Status::Added,
        (_, None) => Status::Deleted,
        _ => Status::Modified,
    };
    let mut d = FileDiff {
        path: depot_path.as_str().to_owned(),
        status,
        old,
        new,
        copied: None,
        binary,
        insertions: 0,
        deletions: 0,
        patch: Vec::new(),
    };
    render(&mut d, binary);
    Ok(entry(&d, true))
}

/// One version of a file as bytes, for viewers that need the content itself (Lode Desk's image
/// diff). `bytes` is None when the file is larger than the limit the caller gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// Size in bytes.
    pub size: u64,
    /// The content, when it is within the limit.
    pub bytes: Option<Vec<u8>>,
}

/// The one depot path `path` names in the workspace.
fn one_path(ctx: &Ctx, ws: &lode_workspace::Workspace, path: &str) -> OpResult<DepotPath> {
    local::depot_paths(ctx, ws, &[PathBuf::from(path)])?
        .into_iter()
        .next()
        .ok_or_else(|| OpError::usage(format!("`{path}` is not a file in the workspace")))
}

/// What a working file was before the local change: its base, or for a copied or moved file
/// its source's base. None for a new file.
pub async fn base_content(ctx: &Ctx, path: &str, limit: u64) -> OpResult<Option<Content>> {
    let ws = local::open(ctx)?;
    let p = one_path(ctx, &ws, path)?;
    let Some(row) = ws.db().file(&p)? else {
        return Ok(None);
    };
    let src = match (&row.state, &row.copy_from) {
        (FileState::Added, Some(c)) => ws.db().file(&c.path)?,
        (FileState::Added, None) => None,
        _ => Some(row),
    };
    let Some((id, size)) = src.and_then(|r| r.base_id.map(|id| (id, r.size))) else {
        return Ok(None);
    };
    if size > limit {
        return Ok(Some(Content { size, bytes: None }));
    }
    let bytes = Bases::new(ctx, &ws)?.read(&id).await?;
    Ok(Some(Content {
        size: bytes.len() as u64,
        bytes: Some(bytes),
    }))
}

/// A working file as it is on disk now. None when it is not there (deleted or missing).
pub fn working_content(ctx: &Ctx, path: &str, limit: u64) -> OpResult<Option<Content>> {
    let ws = local::open(ctx)?;
    let p = one_path(ctx, &ws, path)?;
    let local = ws.local_path(&p);
    let meta = match fs::symlink_metadata(&local) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let size = meta.len();
    let bytes = if size > limit {
        None
    } else {
        Some(fs::read(&local)?)
    };
    Ok(Some(Content { size, bytes }))
}

/// A file as stored at a revision: `spec` is `path@rev` as for [`diff_versions`].
pub async fn stored_content(
    ctx: &Ctx,
    source: &Source,
    spec: &str,
    limit: u64,
) -> OpResult<Content> {
    let t = history::target(ctx, source)?;
    let (p, given) = history::split_rev(spec)?;
    let path = t
        .path(ctx, p)?
        .ok_or_else(|| OpError::usage("name a file to read"))?;
    let rev = t.rev(given).await?;
    let file = t.client.file(&t.depot, rev, &path).await?;
    if let Some(size) = file.size.filter(|n| *n > limit) {
        return Ok(Content { size, bytes: None });
    }
    let mut buf = Vec::new();
    file.write_to(&mut buf).await?;
    let size = buf.len() as u64;
    Ok(Content {
        size,
        bytes: (size <= limit).then_some(buf),
    })
}

/// A file as stored at a revision; files over [`TEXT_LIMIT`] are left unread.
async fn stored(ctx: &Ctx, t: &history::Target, spec: &str) -> OpResult<(Side, DepotPath)> {
    let (p, given) = history::split_rev(spec)?;
    let path = t
        .path(ctx, p)?
        .ok_or_else(|| OpError::usage("name a file to compare"))?;
    let rev = t.rev(given).await?;
    let file = t.client.file(&t.depot, rev, &path).await?;
    let (kind, executable, known) = (file.kind, file.executable, file.size);
    let bytes = if known.is_some_and(|n| n > TEXT_LIMIT) {
        None
    } else {
        let mut buf = Vec::new();
        file.write_to(&mut buf).await?;
        Some(buf)
    };
    let size = bytes
        .as_ref()
        .map_or(known.unwrap_or(0), |b| b.len() as u64);
    let bytes = bytes.filter(|b| b.len() as u64 <= TEXT_LIMIT);
    Ok((
        Side {
            path: path.as_str().to_owned(),
            kind,
            executable,
            bytes,
            size,
        },
        path,
    ))
}
