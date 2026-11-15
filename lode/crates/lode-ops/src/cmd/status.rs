//! `lode status [paths]`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use futures::TryStreamExt;
use lode_core::{DepotPath, ErrorCode, ObjectId};
use lode_proto::locks::LocksQuery;
use lode_proto::read::ManifestDelta;
use lode_workspace::db::keys;
use lode_workspace::{FileState, ScanEntry, Status, Workspace};
use serde_json::{json, Map, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local;

/// `lode status` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Show changed paths, one per line.

Each line is a status letter, a lock marker, and the depot path:

  M  modified        A  added           D  deleted (lode rm)
  !  missing         C  conflicted      ?  untracked

The second column is L when this workspace holds the file's lock. A file that needs a lock
(`[locks] required` in .lodeconfig) and was changed without one is marked `(needs lock)`:
the server will refuse to commit it until you `lode lock` it. A copied or moved file names
its origin. Files in a changelist are listed after the others, under a heading per
changelist (see `lode changelist`).

--unchanged lists unchanged tracked files too. --remote also asks the server, with one request
for the branch head, at most one for the changes since this workspace's revision and one for
locks, which files in scope have a newer version and who holds their locks.

With --json: {\"ok\":true,\"branch\",\"rev\",\"merges\":[{\"branch\",\"from_rev\",\"to_rev\"}],\"files\":[{\"path\",\"status\",\"copy_from\":{\"path\",\"rev\"}|null,
\"moved\":bool,\"changelist\":name|null,\"locked\":bool,\"needs_lock\":bool,\"lock_required\":bool}],
\"invalid\":[{\"local\",\"reason\"}]}, plus with --remote \"remote\":{\"head_rev\",\"proxy\",\"files\":[{\"path\",
\"newer\":bool,\"deleted_at_head\":bool,\"locked_by\":{\"name\",\"kind\"}|null,\"lock_workspace\"|null,\"mine\":bool}]}
(only files with a newer version or a lock).
`status` is one of modified, added, deleted, missing, conflicted, untracked, and unchanged with
--unchanged.")]
pub struct StatusArgs {
    /// Only these files and directories.
    pub paths: Vec<PathBuf>,
    /// Also list tracked files that are unchanged.
    #[arg(long)]
    pub unchanged: bool,
    /// Also ask the server which files have a newer version and who holds their locks.
    #[arg(long)]
    pub remote: bool,
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
pub async fn status(ctx: &Ctx, args: StatusArgs) -> OpResult<Value> {
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
    let branch = ws.db().config(lode_workspace::db::keys::BRANCH)?;
    let rev = ws.db().config_u64(lode_workspace::db::keys::SYNCED_REV)?;
    if let (Some(b), Some(r)) = (&branch, rev) {
        ctx.out.line(format!("On branch {b} at r{r}"));
    }
    let merges = crate::cmd::merge::pending_merges(&ws)?;
    for m in &merges {
        ctx.out.line(format!(
            "Merging {} (commit to record it, `lode revert --merge` to abandon it)",
            crate::cmd::merge::describe(m)
        ));
    }

    let mut files = Vec::new();
    let mut groups: HashMap<Option<i64>, Vec<String>> = HashMap::new();
    let listed = report
        .entries
        .iter()
        .filter(|e| args.unchanged || e.status != Status::Unchanged);
    for e in listed {
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
            "lock_required": required.matches(&e.path),
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
    let mut out = json!({
        "branch": branch,
        "rev": rev,
        "merges": merges,
        "files": files,
        "invalid": invalid,
    });
    if args.remote {
        out["remote"] = remote(ctx, &ws, &paths).await?;
    }
    Ok(out)
}

/// What the server knows about the files in `scope` (all when empty): a newer version on the
/// branch head, and locks. One request each for the head, the changes since the workspace's
/// revision (skipped when it is at the head) and the locks.
async fn remote(ctx: &Ctx, ws: &Workspace, scope: &[DepotPath]) -> OpResult<Value> {
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?;
    let branch = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no branch"))?;
    let workspace_id = ws.db().config(keys::WORKSPACE_ID)?;
    let synced = local::synced_rev(ws)?;
    let server = ctx.server_url(Some(ws))?;
    let client = ctx.client(&server)?;
    let in_scope = |p: &DepotPath| scope.is_empty() || scope.iter().any(|s| local::under(p, s));
    let mut files: BTreeMap<DepotPath, Map<String, Value>> = BTreeMap::new();
    let blank = || {
        let mut m = Map::new();
        m.insert("newer".into(), json!(false));
        m.insert("deleted_at_head".into(), json!(false));
        m.insert("locked_by".into(), Value::Null);
        m.insert("lock_workspace".into(), Value::Null);
        m.insert("mine".into(), json!(false));
        m
    };

    let head = client.branch(&depot, &branch).await?.head_rev;
    if head > synced {
        let bases: HashMap<DepotPath, Option<ObjectId>> = ws
            .db()
            .files()?
            .into_iter()
            .map(|r| (r.path, r.base_id))
            .collect();
        let view = ws.view_spec()?;
        let delta = client
            .manifest_delta(&depot, synced, head, &view.include, &view.exclude)
            .await?;
        let items: Vec<ManifestDelta> = delta.try_collect().await?;
        for d in items {
            match d {
                ManifestDelta::Put(e) => {
                    // A file this workspace committed after its revision is not newer.
                    if in_scope(&e.path) && bases.get(&e.path).copied().flatten() != Some(e.id) {
                        files
                            .entry(e.path)
                            .or_insert_with(blank)
                            .insert("newer".into(), json!(true));
                    }
                }
                ManifestDelta::Del { path } => {
                    if in_scope(&path) && bases.contains_key(&path) {
                        let m = files.entry(path).or_insert_with(blank);
                        m.insert("newer".into(), json!(true));
                        m.insert("deleted_at_head".into(), json!(true));
                    }
                }
            }
        }
    }

    let q = LocksQuery {
        branch: Some(branch.clone()),
        prefix: None,
        owner: None,
    };
    match client.locks(&depot, &q).await {
        Ok(list) => {
            for l in list.locks {
                if !in_scope(&l.path) {
                    continue;
                }
                let mine = workspace_id.as_deref() == Some(l.workspace.as_str());
                let m = files.entry(l.path).or_insert_with(blank);
                m.insert(
                    "locked_by".into(),
                    json!({ "name": l.owner.name, "kind": l.owner.kind }),
                );
                m.insert("lock_workspace".into(), json!(l.workspace));
                m.insert("mine".into(), json!(mine));
            }
        }
        // Without the locks module there are no locks to report.
        Err(e) if e.is_code(ErrorCode::NotFound) => {}
        Err(e) => return Err(e.into()),
    }

    if !files.is_empty() {
        ctx.out.line("");
        ctx.out.line(format!("On the server (r{head}):"));
    }
    let mut list = Vec::with_capacity(files.len());
    for (path, mut m) in files {
        let mut notes = Vec::new();
        if m.get("deleted_at_head") == Some(&json!(true)) {
            notes.push("deleted at head".to_owned());
        } else if m.get("newer") == Some(&json!(true)) {
            notes.push("newer at head".to_owned());
        }
        if let Some(Value::Object(by)) = m.get("locked_by") {
            let who = by.get("name").and_then(Value::as_str).unwrap_or_default();
            if m.get("mine") == Some(&json!(true)) {
                notes.push("locked here".to_owned());
            } else {
                notes.push(format!("locked by {who}"));
            }
        }
        ctx.out.line(format!("  {path}  ({})", notes.join(", ")));
        m.insert("path".into(), json!(path.as_str()));
        list.push(Value::Object(m));
    }
    let proxy = client.proxy_url().map(|u| u.to_string());
    if let Some(p) = &proxy {
        ctx.out.line("");
        ctx.out.line(format!(
            "File content is downloaded through the caching proxy {p}."
        ));
    }
    Ok(json!({ "head_rev": head, "files": list, "proxy": proxy }))
}
