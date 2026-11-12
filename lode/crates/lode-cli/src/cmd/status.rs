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

The second column is L when this workspace holds the file's lock. A copied or moved file
names its origin, and a file in a changelist shows the changelist in brackets.

With --json: {\"ok\":true,\"files\":[{\"path\",\"status\",\"copy_from\":{\"path\",\"rev\"}|null,
\"moved\":bool,\"changelist\":name|null,\"locked\":bool}],\"invalid\":[{\"local\",\"reason\"}]}.
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
    let lists: HashMap<i64, String> = ws
        .db()
        .changelists()?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect();

    let mut files = Vec::new();
    for e in report.changed() {
        let row = e.row.as_ref();
        let copy_from = row.and_then(|r| r.copy_from.as_ref());
        let is_move = moved(e, &states);
        let list = row
            .and_then(|r| r.changelist_id)
            .and_then(|id| lists.get(&id));
        let locked = row.is_some_and(|r| r.lock_held);
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
        if let Some(l) = list {
            line.push_str(&format!("  [{l}]"));
        }
        ctx.out.line(line);
        files.push(json!({
            "path": e.path.as_str(),
            "status": status_name(e.status),
            "copy_from": copy_from.map(|c| json!({ "path": c.path.as_str(), "rev": c.rev })),
            "moved": is_move,
            "changelist": list,
            "locked": locked,
        }));
    }
    let invalid: Vec<Value> = report
        .invalid
        .iter()
        .map(|i| json!({ "local": i.local, "reason": i.reason }))
        .collect();
    Ok(json!({ "files": files, "invalid": invalid }))
}
