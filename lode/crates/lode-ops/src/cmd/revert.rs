//! `lode revert` and `lode clean`: back to a known state.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use lode_core::DepotPath;
use lode_workspace::{FileState, Status};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, under, Bases};
use crate::materialize::remove_and_prune;

/// `lode revert` options.
#[derive(Debug, Default, clap::Args)]
pub struct RevertArgs {
    /// Files and directories to revert.
    pub paths: Vec<PathBuf>,
    /// Revert every change in the workspace, clear all conflicts and abandon a pending merge.
    #[arg(long, conflicts_with = "paths")]
    pub all: bool,
    /// Abandon the pending merge: the next commit does not record it. Files stay as they are
    /// unless named or --all is given.
    #[arg(long)]
    pub merge: bool,
}

/// `lode clean` options.
#[derive(Debug, Default, clap::Args)]
pub struct CleanArgs {
    /// Only below these files and directories.
    pub paths: Vec<PathBuf>,
    /// Remove ignored files too.
    #[arg(long)]
    pub ignored: bool,
    /// List what would be removed, and remove nothing.
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

/// Runs `lode revert`.
pub async fn revert(ctx: &Ctx, args: RevertArgs) -> OpResult<Value> {
    if !args.all && !args.merge && args.paths.is_empty() {
        return Err(OpError::usage(
            "name the paths to revert, or pass --all to revert everything",
        ));
    }
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let mut abandoned = Vec::new();
    if args.all || args.merge {
        abandoned = crate::cmd::merge::pending_merges(&ws)?;
        crate::cmd::merge::set_pending_merges(&mut ws, &[])?;
        for m in &abandoned {
            ctx.out.line(format!(
                "Abandoned the merge of {}",
                crate::cmd::merge::describe(m)
            ));
        }
        if args.merge && abandoned.is_empty() {
            ctx.out.line("No merge is pending.");
        }
        if !args.all && args.paths.is_empty() {
            return Ok(json!({ "reverted": [], "merges_abandoned": abandoned }));
        }
    }
    let paths = if args.all {
        Vec::new()
    } else {
        local::depot_paths(ctx, &ws, &args.paths)?
    };
    let report = local::scan(ctx, &mut ws, &paths, false)?;
    let conflicts = ws.db().conflicts()?;
    let mut bases = Bases::new(ctx, &ws)?;
    let mut reverted = Vec::new();
    for e in report.changed() {
        let Some(row) = &e.row else { continue };
        match e.status {
            Status::Added => {
                ws.db_mut().write(|tx| tx.remove_file(&e.path))?;
            }
            Status::Missing if row.state == FileState::Added => {
                ws.db_mut().write(|tx| tx.remove_file(&e.path))?;
            }
            // A conflict with a deletion on the server: taking theirs removes the file.
            Status::Conflicted if row.base_id.is_none() => {
                remove_and_prune(ws.root(), &e.path)?;
                ws.db_mut().write(|tx| tx.remove_file(&e.path))?;
            }
            Status::Modified | Status::Missing | Status::Deleted | Status::Conflicted => {
                bases.restore(&mut ws, row).await?;
            }
            _ => continue,
        }
        reverted.push(e.path.as_str().to_owned());
    }
    // Conflict state goes with the reverted paths, or all of it with --all.
    for c in &conflicts {
        if !args.all && !paths.iter().any(|p| under(&c.path, p)) {
            continue;
        }
        local::clear_conflict(&mut ws, c)?;
    }
    // A lock-required file back at its base is read-only again, unless it is locked here.
    let restored: Vec<DepotPath> = report
        .changed()
        .filter(|e| reverted.iter().any(|p| p == e.path.as_str()))
        .map(|e| e.path.clone())
        .collect();
    crate::cmd::locks::protect(&ws, &restored)?;
    for p in &reverted {
        ctx.out.line(format!("Reverted {p}"));
    }
    if reverted.is_empty() {
        ctx.out.line("Nothing to revert.");
    }
    Ok(json!({ "reverted": reverted, "merges_abandoned": abandoned }))
}

/// What `clean` removes below `dir`: `files` that go, then directories that end up empty.
/// Returns whether `dir` itself ends up empty.
fn plan_dir(dir: &Path, files: &BTreeSet<PathBuf>, dirs: &mut Vec<PathBuf>) -> bool {
    let Ok(read) = fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for entry in read.flatten() {
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map(|t| t.is_dir() && !t.is_symlink())
            .unwrap_or(false);
        if is_dir {
            if plan_dir(&path, files, dirs) {
                dirs.push(path);
            } else {
                empty = false;
            }
        } else if !files.contains(&path) {
            empty = false;
        }
    }
    empty
}

fn display(root: &Path, local: &Path, dir: bool) -> String {
    let rel = local.strip_prefix(root).unwrap_or(local);
    let mut s: String = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if dir {
        s.push('/');
    }
    s
}

/// Runs `lode clean`.
pub async fn clean(ctx: &Ctx, args: CleanArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    let report = local::scan(ctx, &mut ws, &paths, args.ignored)?;
    let root = ws.root().to_path_buf();
    let files: BTreeSet<PathBuf> = report
        .entries
        .iter()
        .filter(|e| match e.status {
            Status::Untracked => true,
            Status::Ignored => args.ignored,
            _ => false,
        })
        .map(|e| ws.local_path(&e.path))
        .collect();

    // Directories to look at for emptiness: the named ones, or everything but `.lode`.
    let mut starts: Vec<PathBuf> = Vec::new();
    if paths.is_empty() {
        if let Ok(read) = fs::read_dir(&root) {
            for entry in read.flatten() {
                if entry.file_name() == lode_workspace::layout::DOT_LODE {
                    continue;
                }
                if entry
                    .file_type()
                    .map(|t| t.is_dir() && !t.is_symlink())
                    .unwrap_or(false)
                {
                    starts.push(entry.path());
                }
            }
        }
    } else {
        for p in &paths {
            let local = ws.local_path(p);
            if fs::symlink_metadata(&local).is_ok_and(|m| m.is_dir()) {
                starts.push(local);
            }
        }
    }
    let mut dirs = Vec::new();
    for s in &starts {
        if plan_dir(s, &files, &mut dirs) {
            dirs.push(s.clone());
        }
    }

    let mut listed: Vec<String> = files.iter().map(|f| display(&root, f, false)).collect();
    listed.extend(dirs.iter().map(|d| display(&root, d, true)));
    if !args.dry_run {
        for f in &files {
            local::remove_if_exists(f)?;
        }
        // Children were pushed before their parents.
        for d in &dirs {
            match fs::remove_dir(d) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    let verb = if args.dry_run {
        "Would remove"
    } else {
        "Removed"
    };
    for l in &listed {
        ctx.out.line(format!("{verb} {l}"));
    }
    Ok(json!({ "removed": listed, "dry_run": args.dry_run }))
}
