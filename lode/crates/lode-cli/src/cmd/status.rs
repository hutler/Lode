//! `lode status [paths]`.

use std::collections::HashMap;
use std::path::PathBuf;

use lode_workspace::{FileState, ScanEntry, Status};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::CliResult;
use crate::local;

/// `lode status` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Show changed paths, one per line.

Each line is a status letter, a lock marker, and the depot path:

  M  modified        A  added           D  deleted (lode rm)
  !  missing         C  conflicted      ?  untracked

The second column is L when this workspace holds the file's lock. A file that needs a lock
(`[locks] required` in .lodeconfig) and was changed without one is marked `(needs lock)`:
the server will refuse to commit it until you `lode lock` it. A copied or moved file names
its origin. Files in a changelist are listed after the others, under a heading per
changelist (see `lode changelist`).

With --json: {\"ok\":true,\"files\":[{\"path\",\"status\",\"copy_from\":{\"path\",\"rev\"}|null,
\"moved\":bool,\"changelist\":name|null,\"locked\":bool,\"needs_lock\":bool}],
\"invalid\":[{\"local\",\"reason\"}]}.
`status` is one of modified, added, deleted, missing, conflicted, untracked.")]
pub struct StatusArgs {
    /// Only these files and directories.
    pub paths: Vec<PathBuf>,
}

/// The `--json` name of a status.
pub fn status_name(s: Status) -> &'static str {
    match s {
        Status::Unchanged => "unchanged",
        Status::Modified => "modified",
        Status::Added => "added",
        Status::Deleted => "deleted",
        Status::Missing => "missing",
        Status::Untracked => "untracked",
        Status::Conflicted => "conflicted",
        Status::Ignored => "ignored",
    }
}

/// Whether an added file's origin was deleted in the same change (a move, not a copy).
fn moved(e: &ScanEntry, rows: &HashMap<String, FileState>) -> bool {
    e.row
        .as_ref()
        .and_then(|r| r.copy_from.as_ref())
        .is_some_and(|c| rows.get(c.path.as_str()) == Some(&FileState::Deleted))
}

/// Runs `lode status`.
pub async fn status(ctx: &Ctx, args: StatusArgs) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    let report = local::scan(ctx, &mut ws, &paths, false)?;
    let states: HashMap<String, FileState> = ws
        .db()
        .files()?
        .into_iter()
        .map(|r| (r.path.as_str().to_owned(), r.state))
        .collect();
    let all_lists = ws.db().changelists()?;
    let required = crate::cmd::locks::required(&ws);
    let lists: HashMap<i64, String> = all_lists.iter().map(|c| (c.id, c.name.clone())).collect();

    let mut files = Vec::new();
    let mut groups: HashMap<Option<i64>, Vec<String>> = HashMap::new();
    for e in report.changed() {
        let row = e.row.as_ref();
        let copy_from = row.and_then(|r| r.copy_from.as_ref());
        let is_move = moved(e, &states);
        let list_id = row
            .and_then(|r| r.changelist_id)
            .filter(|id| lists.contains_key(id));
        let list = list_id.and_then(|id| lists.get(&id));
        let locked = row.is_some_and(|r| r.lock_held);
        // Adds need no lock; changing or deleting a required file does.
        let needs_lock = !locked
            && row.is_some_and(|r| r.state != FileState::Added && r.base_id.is_some())
            && matches!(
                e.status,
                Status::Modified | Status::Deleted | Status::Missing
            )
            && required.matches(&e.path);
        let mut line = format!(
            "{}{} {}",
            e.status.letter(),
            if locked { 'L' } else { ' ' },
            e.path
        );
        if let Some(c) = copy_from {
            let verb = if is_move { "moved" } else { "copied" };
            line.push_str(&format!("  ({verb} from {}@r{})", c.path, c.rev));
        }
        if needs_lock {
            line.push_str("  (needs lock)");
        }
        groups.entry(list_id).or_default().push(line);
        files.push(json!({
            "path": e.path.as_str(),
            "status": status_name(e.status),
            "copy_from": copy_from.map(|c| json!({ "path": c.path.as_str(), "rev": c.rev })),
            "moved": is_move,
            "changelist": list,
            "locked": locked,
            "needs_lock": needs_lock,
        }));
    }
    let default = groups.remove(&None).unwrap_or_default();
    let mut printed = !default.is_empty();
    for line in default {
        ctx.out.line(line);
    }
    for c in &all_lists {
        let Some(lines) = groups.remove(&Some(c.id)) else {
            continue;
        };
        if std::mem::replace(&mut printed, true) {
            ctx.out.line("");
        }
        let desc = c.description.lines().next().unwrap_or_default();
        if desc.is_empty() {
            ctx.out.line(format!("Changelist {}:", c.name));
        } else {
            ctx.out.line(format!("Changelist {}: {desc}", c.name));
        }
        for line in lines {
            ctx.out.line(line);
        }
    }
    let invalid: Vec<Value> = report
        .invalid
        .iter()
        .map(|i| json!({ "local": i.local, "reason": i.reason }))
        .collect();
    Ok(json!({ "files": files, "invalid": invalid }))
}
