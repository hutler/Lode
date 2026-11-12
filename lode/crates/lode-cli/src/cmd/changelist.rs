//! `lode changelist new|list|move|delete`: named local groups of pending changes (SPEC §6.3).
//!
//! A changelist is a label on tracked files in this workspace only; the server never sees
//! it. Files in no changelist are in `default`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use lode_core::DepotPath;
use lode_workspace::{FileState, Status};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local::{self, under, DEFAULT_CHANGELIST};

/// Longest changelist name.
pub const MAX_NAME: usize = 100;

/// `lode changelist` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum ChangelistCommand {
    /// Create a changelist.
    New {
        /// Name: letters, digits, `.`, `_` and `-`.
        name: String,
        /// Description, used as the default commit message of `commit -c`.
        #[arg(short = 'm', long = "message", default_value = "")]
        description: String,
    },
    /// List changelists with their changed files.
    List,
    /// Put files into a changelist (`default` takes them out of any).
    Move {
        /// The changelist.
        name: String,
        /// Tracked files and directories.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Delete a changelist; its files return to `default`.
    Delete {
        /// The changelist.
        name: String,
    },
}

/// `lode changelist` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Named groups of pending changes in this workspace.

`lode commit -c NAME` commits only the files of one changelist, with its description as the
default message; `add`, `rm`, `mv` and `cp` take `-c NAME` to file what they touch. A file
leaves its changelist when it is committed or reverted, and a changelist left empty by a
commit is deleted. Files in no changelist are in `default`.

With --json: new {\"changelist\"}; list {\"changelists\":[{\"name\",\"description\",\"files\":[paths]}]}
(changed files only, `default` first); move {\"changelist\",\"moved\":[paths]};
delete {\"deleted\",\"files\":n}.")]
pub struct ChangelistArgs {
    #[command(subcommand)]
    pub command: ChangelistCommand,
}

fn check_name(name: &str) -> CliResult<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return Err(CliError::usage(format!(
            "invalid changelist name `{name}`: use 1 to {MAX_NAME} letters, digits, `.`, `_` or `-`"
        )));
    }
    if name == DEFAULT_CHANGELIST {
        return Err(CliError::usage("`default` always exists"));
    }
    Ok(())
}

/// Runs `lode changelist`.
pub async fn changelist(ctx: &Ctx, args: ChangelistArgs) -> CliResult<Value> {
    match args.command {
        ChangelistCommand::New { name, description } => new(ctx, &name, &description),
        ChangelistCommand::List => list(ctx),
        ChangelistCommand::Move { name, paths } => move_to(ctx, &name, &paths),
        ChangelistCommand::Delete { name } => delete(ctx, &name),
    }
}

fn new(ctx: &Ctx, name: &str, description: &str) -> CliResult<Value> {
    check_name(name)?;
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    if ws.db().changelists()?.iter().any(|c| c.name == name) {
        return Err(CliError::failed(
            "conflict",
            format!("changelist `{name}` already exists"),
        ));
    }
    ws.db_mut()
        .write(|tx| tx.create_changelist(name, description.trim()))?;
    ctx.out.line(format!("Created changelist {name}."));
    Ok(json!({ "changelist": name }))
}

fn list(ctx: &Ctx) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let report = local::scan(ctx, &mut ws, &[], false)?;
    let lists = ws.db().changelists()?;
    let mut files: BTreeMap<Option<i64>, Vec<String>> = BTreeMap::new();
    for e in report.changed() {
        let id = e.row.as_ref().and_then(|r| r.changelist_id);
        // Untracked files are no pending change of any list.
        if e.status == Status::Untracked {
            continue;
        }
        files
            .entry(id)
            .or_default()
            .push(e.path.as_str().to_owned());
    }
    let mut out = vec![json!({
        "name": DEFAULT_CHANGELIST,
        "description": "",
        "files": files.remove(&None).unwrap_or_default(),
    })];
    for c in &lists {
        out.push(json!({
            "name": c.name,
            "description": c.description,
            "files": files.remove(&Some(c.id)).unwrap_or_default(),
        }));
    }
    for c in &out {
        let n = c["files"].as_array().map_or(0, Vec::len);
        let desc = c["description"].as_str().unwrap_or_default();
        let mut line = format!(
            "{}  {n} file{}",
            c["name"].as_str().unwrap_or_default(),
            if n == 1 { "" } else { "s" }
        );
        if !desc.is_empty() {
            line.push_str(&format!("  {}", desc.lines().next().unwrap_or_default()));
        }
        ctx.out.line(line);
    }
    Ok(json!({ "changelists": out }))
}

fn move_to(ctx: &Ctx, name: &str, paths: &[PathBuf]) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let target = local::changelist(&ws, name)?.map(|c| c.id);
    let named = local::depot_paths(ctx, &ws, paths)?;
    let rows = ws.db().files()?;
    let mut chosen: Vec<DepotPath> = Vec::new();
    let mut unknown = Vec::new();
    for p in &named {
        let before = chosen.len();
        chosen.extend(
            rows.iter()
                .filter(|r| under(&r.path, p))
                .map(|r| r.path.clone()),
        );
        if chosen.len() == before {
            unknown.push(p.as_str().to_owned());
        }
    }
    if !unknown.is_empty() {
        return Err(local::not_found(
            "not tracked (add new files with `lode add -c`)",
            &unknown,
        ));
    }
    // The two halves of a move stay together, so `commit -c` sends both.
    let mut set: HashSet<DepotPath> = chosen.iter().cloned().collect();
    for r in &rows {
        if let Some(cf) = &r.copy_from {
            let origin_deleted = rows
                .iter()
                .any(|o| o.path == cf.path && o.state == FileState::Deleted);
            if !origin_deleted {
                continue;
            }
            if set.contains(&r.path) {
                set.insert(cf.path.clone());
            } else if set.contains(&cf.path) {
                set.insert(r.path.clone());
            }
        }
    }
    let mut moved: Vec<DepotPath> = set.into_iter().collect();
    moved.sort();
    ws.db_mut().write(|tx| {
        for p in &moved {
            tx.set_changelist(p, target)?;
        }
        Ok(())
    })?;
    for p in &moved {
        ctx.out.line(format!("{p} -> {name}"));
    }
    let moved: Vec<&str> = moved.iter().map(DepotPath::as_str).collect();
    Ok(json!({ "changelist": name, "moved": moved }))
}

fn delete(ctx: &Ctx, name: &str) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let Some(cl) = local::changelist(&ws, name)? else {
        return Err(CliError::usage("`default` cannot be deleted"));
    };
    let files = ws.db_mut().write(|tx| {
        let n = tx.changelist_size(cl.id)?;
        tx.delete_changelist(cl.id)?;
        Ok(n)
    })?;
    ctx.out.line(format!(
        "Deleted changelist {name}; {files} file{} moved to default.",
        if files == 1 { "" } else { "s" }
    ));
    Ok(json!({ "deleted": name, "files": files }))
}
