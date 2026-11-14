//! `lode add`, `rm`, `mv`, `cp`: changes to what is tracked.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lode_core::{CopyFrom, DepotPath, EntryKind};
use lode_workspace::scan::{stat_for_row, trusted_mtime};
use lode_workspace::{FileRow, FileState, Status, Workspace};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, under};
use crate::materialize::{place_symlink, prepare_target, remove_and_prune};

/// `lode add` options.
#[derive(Debug, Default, clap::Args)]
pub struct AddArgs {
    /// Files and directories (directories are added recursively).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Also add ignored files.
    #[arg(long, short = 'f')]
    pub force: bool,
    /// Put the added files into this changelist.
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
}

/// `lode rm` options.
#[derive(Debug, Default, clap::Args)]
pub struct RmArgs {
    /// Files and directories (directories recursively).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Stop tracking but leave the files on disk.
    #[arg(long)]
    pub keep: bool,
    /// Also remove files with local changes or that were only added.
    #[arg(long, short = 'f')]
    pub force: bool,
    /// Put the deletions into this changelist.
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
}

/// `lode mv` and `lode cp` options.
#[derive(Debug, Default, clap::Args)]
pub struct MoveArgs {
    /// Sources, then the destination. With several sources the destination is a directory.
    #[arg(required = true, num_args = 2..)]
    pub paths: Vec<PathBuf>,
    /// Only record a move or copy already done on disk.
    #[arg(long)]
    pub after: bool,
    /// Put the result into this changelist (by default it stays in the source's).
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
}

/// The changelist id `-c NAME` names, or the default one when `-c` is absent.
fn changelist_arg(ws: &Workspace, name: Option<&str>) -> OpResult<Option<Option<i64>>> {
    name.map(|n| local::changelist(ws, n).map(|c| c.map(|c| c.id)))
        .transpose()
}

/// An `added` row for the file at `local`, with its kind, executable bit, size and mtime.
pub fn added_row(local: &Path, path: DepotPath) -> OpResult<FileRow> {
    let meta = fs::symlink_metadata(local)?;
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Ok(FileRow {
        path,
        base_id: None,
        kind,
        executable: kind == EntryKind::File && is_executable(&meta),
        size: meta.len(),
        mtime_ns: trusted_mtime(mtime, SystemTime::now()),
        state: FileState::Added,
        copy_from: None,
        changelist_id: None,
        lock_held: false,
    })
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

/// Runs `lode add`.
pub async fn add(ctx: &Ctx, args: AddArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let named = local::depot_paths(ctx, &ws, &args.paths)?;
    let list = changelist_arg(&ws, args.changelist.as_deref())?;
    let report = local::scan(ctx, &mut ws, &named, true)?;
    let view = ws.view_spec()?.compile()?;

    // Every named path must be something to add, already tracked, or a directory.
    let mut ignored_named = Vec::new();
    let mut missing = Vec::new();
    for p in &named {
        let has_entry = report.entries.iter().any(|e| under(&e.path, p));
        if report.get(p).is_some_and(|e| e.status == Status::Ignored) && !args.force {
            ignored_named.push(p.as_str().to_owned());
        }
        if !has_entry {
            let local = ws.local_path(p);
            match fs::symlink_metadata(&local) {
                Ok(m) if m.is_dir() => {}
                Ok(_) if !view.contains(p) => {
                    return Err(OpError::failed(
                        "outside_view",
                        format!("{p} is outside this workspace's view; see `lode view`"),
                    ))
                }
                Ok(_) => {}
                Err(_) => missing.push(p.as_str().to_owned()),
            }
        }
    }
    if !missing.is_empty() {
        return Err(local::not_found("no such file", &missing));
    }
    if !ignored_named.is_empty() {
        return Err(OpError::failed(
            "ignored",
            format!(
                "{} {} ignored; use --force to add anyway",
                ignored_named.join(", "),
                if ignored_named.len() == 1 {
                    "is"
                } else {
                    "are"
                }
            ),
        )
        .with_details(json!({ "paths": ignored_named })));
    }

    let mut rows = Vec::new();
    let mut readded = Vec::new();
    for e in &report.entries {
        match e.status {
            Status::Untracked => {}
            Status::Ignored if args.force => {}
            // A file deleted with `rm --keep` (or re-created) is tracked again as it is.
            Status::Deleted if e.disk.is_some() => {
                if let Some(row) = &e.row {
                    readded.push(row.clone());
                }
                continue;
            }
            _ => continue,
        }
        let mut row = added_row(&ws.local_path(&e.path), e.path.clone())?;
        row.changelist_id = list.flatten();
        rows.push(row);
    }
    ws.db_mut().write(|tx| {
        for r in &rows {
            tx.put_file(r)?;
        }
        for r in &readded {
            let mut r = r.clone();
            r.state = FileState::Normal;
            r.mtime_ns = 0;
            if let Some(l) = list {
                r.changelist_id = l;
            }
            tx.put_file(&r)?;
        }
        Ok(())
    })?;
    let mut added: Vec<&str> = rows
        .iter()
        .chain(readded.iter())
        .map(|r| r.path.as_str())
        .collect();
    added.sort_unstable();
    for p in &added {
        ctx.out.line(format!("A  {p}"));
    }
    Ok(json!({ "added": added }))
}

/// Runs `lode rm`.
pub async fn rm(ctx: &Ctx, args: RmArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let named = local::depot_paths(ctx, &ws, &args.paths)?;
    let list = changelist_arg(&ws, args.changelist.as_deref())?;
    let report = local::scan(ctx, &mut ws, &named, false)?;
    let tracked: Vec<_> = report.entries.iter().filter(|e| e.row.is_some()).collect();
    let untracked_named: Vec<String> = named
        .iter()
        .filter(|p| !tracked.iter().any(|e| under(&e.path, p)))
        .map(|p| p.as_str().to_owned())
        .collect();
    if !untracked_named.is_empty() {
        return Err(local::not_found("not tracked", &untracked_named));
    }
    // Content that exists nowhere else needs --force (or --keep, which loses nothing).
    let precious: Vec<String> = tracked
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                Status::Modified | Status::Added | Status::Conflicted
            )
        })
        .map(|e| e.path.as_str().to_owned())
        .collect();
    if !precious.is_empty() && !args.force && !args.keep {
        return Err(OpError::failed(
            "local_changes",
            format!(
                "{} {} local changes; use --keep to stop tracking only, or --force",
                precious.join(", "),
                if precious.len() == 1 { "has" } else { "have" }
            ),
        )
        .with_details(json!({ "paths": precious })));
    }

    let root = ws.root().to_path_buf();
    let mut removed = Vec::new();
    for e in tracked {
        let Some(row) = &e.row else { continue };
        if row.state == FileState::Deleted {
            continue;
        }
        if !args.keep {
            remove_and_prune(&root, &e.path)?;
        }
        let conflict = ws.db().conflicts()?.into_iter().find(|c| c.path == e.path);
        if let Some(c) = &conflict {
            local::clear_conflict(&mut ws, c)?;
        }
        ws.db_mut().write(|tx| {
            if row.state == FileState::Added || row.base_id.is_none() {
                tx.remove_file(&e.path)?;
            } else {
                let mut r = row.clone();
                r.state = FileState::Deleted;
                if let Some(l) = list {
                    r.changelist_id = l;
                }
                tx.put_file(&r)?;
            }
            Ok(())
        })?;
        removed.push(e.path.as_str().to_owned());
    }
    for p in &removed {
        ctx.out.line(format!("D  {p}"));
    }
    Ok(json!({ "removed": removed, "kept_on_disk": args.keep }))
}

/// One file to move or copy.
struct Pair {
    from: FileRow,
    to: DepotPath,
}

/// Where each named source goes.
fn plan(ctx: &Ctx, ws: &Workspace, args: &MoveArgs, verb: &str) -> OpResult<Vec<Pair>> {
    let (dest_arg, src_args) = args
        .paths
        .split_last()
        .ok_or_else(|| OpError::usage("give a source and a destination"))?;
    let dest = local::depot_path(ctx, ws, dest_arg)?;
    let dest_is_dir = fs::symlink_metadata(ws.local_path(&dest)).is_ok_and(|m| m.is_dir());
    if src_args.len() > 1 && !dest_is_dir {
        return Err(OpError::usage(format!(
            "with several sources, {dest} must be an existing directory"
        )));
    }
    let rows = ws.db().files()?;
    let mut pairs = Vec::new();
    for s in src_args {
        let src = local::depot_path(ctx, ws, s)?;
        if under(&dest, &src) {
            return Err(OpError::usage(format!("cannot {verb} {src} into itself")));
        }
        let exact = rows
            .iter()
            .find(|r| r.path == src && r.state != FileState::Deleted);
        let base = if dest_is_dir && (src_args.len() > 1 || !args.after || exact.is_some()) {
            dest.join(src.name())
                .map_err(|e| OpError::usage(e.to_string()))?
        } else {
            dest.clone()
        };
        let mut found = false;
        for r in &rows {
            if r.state == FileState::Deleted || !under(&r.path, &src) {
                continue;
            }
            found = true;
            let rest = &r.path.as_str()[src.as_str().len()..];
            let to = if rest.is_empty() {
                base.clone()
            } else {
                DepotPath::parse(&format!("{}{rest}", base.as_str()))
                    .map_err(|e| OpError::usage(e.to_string()))?
            };
            pairs.push(Pair {
                from: r.clone(),
                to,
            });
        }
        if !found {
            return Err(local::not_found("not tracked", &[src.as_str().to_owned()]));
        }
    }
    Ok(pairs)
}

/// Checks a plan against the database and the disk before anything changes.
fn check(ws: &Workspace, pairs: &[Pair], after: bool, is_move: bool) -> OpResult<()> {
    let view = ws.view_spec()?.compile()?;
    let mut targets = HashSet::new();
    let sources: HashSet<&DepotPath> = pairs.iter().map(|p| &p.from.path).collect();
    for p in pairs {
        let to = &p.to;
        let fail = |code: &str, msg: String| {
            Err(OpError::failed(code, msg).with_details(json!({ "paths": [to.as_str()] })))
        };
        if to
            .segments()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
        {
            return fail("unsafe_path", format!("{to} is inside .lode"));
        }
        if !targets.insert(to.clone()) || sources.contains(to) {
            return fail("conflict", format!("{to} is named twice"));
        }
        if !view.contains(to) {
            return fail(
                "outside_view",
                format!("{to} is outside this workspace's view"),
            );
        }
        if p.from.state == FileState::Conflicted {
            return fail(
                "conflicted",
                format!("{} is in conflict; resolve it first", p.from.path),
            );
        }
        if ws
            .db()
            .file(to)?
            .is_some_and(|r| r.state != FileState::Deleted)
        {
            return fail("already_tracked", format!("{to} is already tracked"));
        }
        let src_on_disk = fs::symlink_metadata(ws.local_path(&p.from.path)).is_ok();
        let dst_on_disk = fs::symlink_metadata(ws.local_path(to)).is_ok();
        if after {
            if !dst_on_disk {
                return fail("not_found", format!("{to} does not exist"));
            }
            if is_move && src_on_disk {
                return fail(
                    "conflict",
                    format!(
                        "{} still exists; did you mean `lode cp --after`?",
                        p.from.path
                    ),
                );
            }
        } else {
            if dst_on_disk {
                return fail("conflict", format!("{to} already exists"));
            }
            if !src_on_disk {
                return fail(
                    "not_found",
                    format!(
                        "{} is missing; use --after to record a change already made",
                        p.from.path
                    ),
                );
            }
        }
    }
    Ok(())
}

fn copy_on_disk(from: &Path, to: &Path) -> OpResult<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(from)?;
        let text = target.to_str().ok_or_else(|| {
            OpError::failed(
                "invalid_name",
                format!("{} points to a non-UTF-8 name", from.display()),
            )
        })?;
        place_symlink(text, to)?;
    } else {
        // `fs::copy` carries the permission bits, so the executable bit too.
        fs::copy(from, to)?;
    }
    Ok(())
}

async fn move_or_copy(ctx: &Ctx, args: MoveArgs, is_move: bool) -> OpResult<Value> {
    let verb = if is_move { "move" } else { "copy" };
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let pairs = plan(ctx, &ws, &args, verb)?;
    check(&ws, &pairs, args.after, is_move)?;
    let list = changelist_arg(&ws, args.changelist.as_deref())?;
    let rev = local::synced_rev(&ws)?;
    let root = ws.root().to_path_buf();
    let mut done = Vec::new();
    for p in &pairs {
        let from_local = ws.local_path(&p.from.path);
        let to_local = if args.after {
            ws.local_path(&p.to)
        } else {
            let t = prepare_target(&root, &p.to)?;
            if is_move {
                fs::rename(&from_local, &t)?;
                remove_and_prune(&root, &p.from.path)?;
            } else {
                copy_on_disk(&from_local, &t)?;
            }
            t
        };
        let origin = match p.from.state {
            FileState::Added => p.from.copy_from.clone(),
            _ => Some(CopyFrom {
                path: p.from.path.clone(),
                rev,
            }),
        };
        let existing = ws.db().file(&p.to)?;
        let mut new_row = match existing {
            // Back onto a path deleted in this change: that is an edit of it, not an add.
            Some(mut old) => {
                old.state = FileState::Normal;
                old.mtime_ns = 0;
                old
            }
            None => {
                let mut r = added_row(&to_local, p.to.clone())?;
                r.copy_from = origin;
                r
            }
        };
        if new_row.state == FileState::Normal {
            let (size, _) = stat_for_row(&to_local, SystemTime::now())?;
            new_row.size = size;
        }
        new_row.changelist_id = list.unwrap_or(p.from.changelist_id);
        ws.db_mut().write(|tx| {
            tx.put_file(&new_row)?;
            if is_move {
                if p.from.state == FileState::Added {
                    tx.remove_file(&p.from.path)?;
                } else {
                    // Both halves of a move go into the same changelist.
                    let mut old = p.from.clone();
                    old.state = FileState::Deleted;
                    old.changelist_id = new_row.changelist_id;
                    tx.put_file(&old)?;
                }
            }
            Ok(())
        })?;
        let letter = if is_move { 'R' } else { 'C' };
        ctx.out
            .line(format!("{letter}  {} -> {}", p.from.path, p.to));
        done.push(json!({ "from": p.from.path.as_str(), "to": p.to.as_str() }));
    }
    let key = if is_move { "moved" } else { "copied" };
    let mut out = BTreeMap::new();
    out.insert(key, Value::Array(done));
    Ok(json!(out))
}

/// Runs `lode mv`.
pub async fn mv(ctx: &Ctx, args: MoveArgs) -> OpResult<Value> {
    move_or_copy(ctx, args, true).await
}

/// Runs `lode cp`.
pub async fn cp(ctx: &Ctx, args: MoveArgs) -> OpResult<Value> {
    move_or_copy(ctx, args, false).await
}
