//! `lode commit` (ARCHITECTURE §10.3): scan, hash, upload what the server lacks, commit, and
//! bring the rows up to the new revision.
//!
//! The request, with its id, is stored in the workspace before it is sent. If the answer is
//! lost (a crash, a timeout, a 5xx), the next `lode commit` sends the same request again, and
//! the server answers a request id it has seen with the revision it already made.

use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures::StreamExt;
use lode_client::Client;
use lode_core::{validate_changes, Change, ChangeAction, DepotPath, EntryKind, ObjectId};
use lode_proto::commit::CommitRequest;
use lode_workspace::db::keys;
use lode_workspace::scan::{symlink_id, trusted_mtime};
use lode_workspace::{FileRow, FileState, Status, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{CliError, CliResult, Exit};
use crate::local;
use crate::materialize::PARALLEL_DOWNLOADS;

/// Config key holding a commit that was sent but whose answer was not seen.
pub const PENDING_COMMIT: &str = "pending_commit";

/// `lode commit` options.
#[derive(Debug, clap::Args)]
pub struct CommitArgs {
    /// Only changes to these files and directories.
    pub paths: Vec<PathBuf>,
    /// The commit message.
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,
    /// Read the message from a file (`-` for stdin).
    #[arg(short = 'F', long = "file", conflicts_with = "message")]
    pub file: Option<PathBuf>,
    /// Add untracked files first.
    #[arg(short = 'A', long = "add-all")]
    pub add_all: bool,
}

/// A commit in flight, as stored under [`PENDING_COMMIT`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    request: CommitRequest,
    /// Size and mtime of each sent file when it was hashed.
    stats: HashMap<String, (u64, i64)>,
}

/// One file to send: the change and where its content is.
struct Outgoing {
    change: Change,
    local: PathBuf,
    /// Size and mtime when hashed.
    stat: Option<(u64, i64)>,
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Hashes the file at `local` and returns its id with the stat taken before hashing. Fails
/// if the file changed while it was read.
fn hash_stable(path: &DepotPath, local: &Path, kind: EntryKind) -> CliResult<(ObjectId, u64, i64)> {
    let before = fs::symlink_metadata(local)?;
    let (id, size) = if kind == EntryKind::Symlink {
        symlink_id(local)?
    } else {
        let id = lode_core::blob_id(fs::File::open(local)?)?;
        (id, before.len())
    };
    let after = fs::symlink_metadata(local)?;
    if after.len() != before.len() || mtime_ns(&after) != mtime_ns(&before) {
        return Err(changed_while_committing(path));
    }
    Ok((id, size, mtime_ns(&before)))
}

fn changed_while_committing(path: &DepotPath) -> CliError {
    CliError::failed(
        "changed_during_commit",
        format!("{path} changed while it was being committed; nothing was committed, run the commit again"),
    )
    .with_details(json!({ "paths": [path.as_str()] }))
}

/// The commit message from `-m`, `-F` or the editor, without `#` comment lines.
fn message(ctx: &Ctx, ws: &Workspace, args: &CommitArgs, summary: &[String]) -> CliResult<String> {
    let raw = if let Some(m) = &args.message {
        m.clone()
    } else if let Some(f) = &args.file {
        if f.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin())?
        } else {
            fs::read_to_string(ctx.local(f))?
        }
    } else {
        let editor = std::env::var("VISUAL")
            .ok()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.trim().is_empty());
        let (Some(editor), false, true) = (editor, ctx.g.json, std::io::stdin().is_terminal())
        else {
            return Err(CliError::usage(
                "no commit message: pass -m or -F (or set EDITOR in a terminal)",
            ));
        };
        let file = ws
            .layout()
            .root()
            .join(lode_workspace::layout::DOT_LODE)
            .join("COMMIT_MSG");
        let mut template = String::from(
            "\n# Write the commit message above. Lines starting with # are left out.\n# Changes:\n",
        );
        for s in summary {
            template.push_str(&format!("#   {s}\n"));
        }
        fs::write(&file, template)?;
        let status = editor_command(&editor, &file).status()?;
        if !status.success() {
            return Err(CliError::failed(
                "aborted",
                "the editor failed; nothing was committed",
            ));
        }
        let text = fs::read_to_string(&file)?;
        let _ = fs::remove_file(&file);
        text
    };
    let text: Vec<&str> = raw.lines().filter(|l| !l.starts_with('#')).collect();
    let text = text.join("\n").trim().to_owned();
    if text.is_empty() {
        return Err(CliError::failed(
            "aborted",
            "empty commit message; nothing was committed",
        ));
    }
    Ok(text)
}

/// Runs `$EDITOR file` through the shell, so an editor with arguments works.
fn editor_command(editor: &str, file: &Path) -> std::process::Command {
    if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(format!("{editor} \"{}\"", file.display()));
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(file);
        c
    }
}

/// What the server says next, for the errors a user can act on.
fn advice(code: &str) -> Option<&'static str> {
    Some(match code {
        "out_of_date" => "these files changed on the server since you synced them: run `lode update`, then commit again",
        "locked" => "someone else holds the lock on these files",
        "lock_required" => "these files need a lock: run `lode lock <path>` first",
        "permission_denied" => "you may not write these paths; ask a depot admin",
        _ => return None,
    })
}

/// A server refusal with the affected paths and what to do next.
fn explain(e: lode_client::Error) -> CliError {
    let paths: Vec<String> = e
        .api()
        .map(|a| a.paths().iter().map(|p| p.to_string()).collect())
        .unwrap_or_default();
    let mut err = CliError::from(e);
    if let Some(hint) = advice(&err.code) {
        if !paths.is_empty() {
            err.message = format!("{}: {}", err.message, paths.join(", "));
        }
        err.message = format!("{}; {hint}", err.message);
    }
    err
}

/// Whether a failed send may still have been committed by the server.
fn maybe_committed(e: &lode_client::Error) -> bool {
    match e.api() {
        Some(api) => api.status >= 500,
        None => matches!(e, lode_client::Error::Transport(_)),
    }
}

/// Brings the rows of a committed request up to date.
fn apply(ws: &mut Workspace, p: &Pending, trust_stats: bool) -> CliResult<()> {
    let now = SystemTime::now();
    ws.db_mut().write(|tx| {
        for c in &p.request.changes {
            match c.action {
                ChangeAction::Delete => {
                    tx.remove_file(&c.path)?;
                }
                ChangeAction::Add | ChangeAction::Modify => {
                    let Some(id) = c.new_id else { continue };
                    let mut row = FileRow::synced(
                        c.path.clone(),
                        id,
                        c.kind.unwrap_or(EntryKind::File),
                        c.executable,
                    );
                    // The stat at hashing time vouches for the content only if nothing could
                    // have touched the file since (not after a crash and retry).
                    if let (true, Some((size, mtime))) = (trust_stats, p.stats.get(c.path.as_str()))
                    {
                        row.size = *size;
                        row.mtime_ns = trusted_mtime(*mtime, now);
                    }
                    tx.put_file(&row)?;
                }
            }
        }
        tx.remove_config(PENDING_COMMIT)
    })?;
    Ok(())
}

/// Sends a stored request again and applies it.
async fn finish_pending(
    ctx: &Ctx,
    ws: &mut Workspace,
    client: &Client,
    depot: &str,
    branch: &str,
    pending: Pending,
) -> CliResult<Value> {
    ctx.out
        .warn("an earlier commit did not get an answer; sending it again");
    match client.commit(depot, branch, &pending.request).await {
        Ok(resp) => {
            apply(ws, &pending, false)?;
            ctx.out.line(format!(
                "Committed r{} ({} file{}).",
                resp.rev,
                pending.request.changes.len(),
                if pending.request.changes.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            Ok(json!({
                "rev": resp.rev,
                "branch": resp.branch,
                "files": pending.request.changes.len(),
                "uploaded": 0,
                "replayed": resp.replayed,
                "resumed": true,
            }))
        }
        Err(e) if maybe_committed(&e) => Err(explain(e)),
        Err(e) => {
            ws.db_mut().write(|tx| tx.remove_config(PENDING_COMMIT))?;
            Err(explain(e))
        }
    }
}

/// Runs `lode commit`.
pub async fn commit(ctx: &Ctx, args: CommitArgs) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| CliError::failed("workspace", "workspace has no depot"))?;
    let branch = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| CliError::failed("workspace", "workspace has no branch"))?;
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;

    if let Some(text) = ws.db().config(PENDING_COMMIT)? {
        let pending: Pending = serde_json::from_str(&text)
            .map_err(|e| CliError::failed("workspace", format!("stored commit: {e}")))?;
        return finish_pending(ctx, &mut ws, &client, &depot, &branch, pending).await;
    }

    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    if args.add_all {
        let report = local::scan(ctx, &mut ws, &paths, false)?;
        let mut rows = Vec::new();
        for e in report
            .entries
            .iter()
            .filter(|e| e.status == Status::Untracked)
        {
            rows.push(crate::cmd::files::added_row(
                &ws.local_path(&e.path),
                e.path.clone(),
            )?);
        }
        ws.db_mut().write(|tx| {
            for r in &rows {
                tx.put_file(r)?;
            }
            Ok(())
        })?;
    }
    let report = local::scan(ctx, &mut ws, &paths, false)?;

    let mut conflicted = Vec::new();
    let mut missing = Vec::new();
    let mut out: Vec<Outgoing> = Vec::new();
    for e in report.changed() {
        let Some(row) = &e.row else { continue };
        let local_path = ws.local_path(&e.path);
        match e.status {
            Status::Conflicted => conflicted.push(e.path.as_str().to_owned()),
            Status::Missing => missing.push(e.path.as_str().to_owned()),
            Status::Deleted => out.push(Outgoing {
                change: Change {
                    path: e.path.clone(),
                    action: ChangeAction::Delete,
                    base_id: row.base_id,
                    new_id: None,
                    kind: None,
                    executable: false,
                    size: None,
                    copy_from: None,
                },
                local: local_path,
                stat: None,
            }),
            Status::Modified | Status::Added => {
                let Some(disk) = &e.disk else { continue };
                let (id, size, mtime) = hash_stable(&e.path, &local_path, disk.kind)?;
                if e.status == Status::Modified && disk.id.is_some_and(|d| d != id) {
                    return Err(changed_while_committing(&e.path));
                }
                let added = row.state == FileState::Added;
                out.push(Outgoing {
                    change: Change {
                        path: e.path.clone(),
                        action: if added {
                            ChangeAction::Add
                        } else {
                            ChangeAction::Modify
                        },
                        base_id: if added { None } else { row.base_id },
                        new_id: Some(id),
                        kind: Some(disk.kind),
                        executable: disk.executable,
                        size: Some(size),
                        copy_from: if added { row.copy_from.clone() } else { None },
                    },
                    local: local_path,
                    stat: Some((size, mtime)),
                });
            }
            _ => {}
        }
    }
    if !conflicted.is_empty() {
        return Err(CliError::failed(
            "conflicted",
            format!(
                "resolve these conflicts first (`lode resolve`): {}",
                conflicted.join(", ")
            ),
        )
        .with_details(json!({ "paths": conflicted })));
    }
    if !missing.is_empty() {
        return Err(CliError::failed(
            "missing",
            format!(
                "these files are gone without `lode rm`: {}; remove them with `lode rm` or restore them with `lode revert`",
                missing.join(", ")
            ),
        )
        .with_details(json!({ "paths": missing })));
    }
    if out.is_empty() {
        return Err(CliError::failed("nothing_to_commit", "nothing to commit"));
    }
    let changes: Vec<Change> = out.iter().map(|o| o.change.clone()).collect();
    validate_changes(&changes).map_err(|e| CliError::failed("invalid_change", e.to_string()))?;
    let summary: Vec<String> = changes
        .iter()
        .map(|c| {
            let l = match c.action {
                ChangeAction::Add => 'A',
                ChangeAction::Modify => 'M',
                ChangeAction::Delete => 'D',
            };
            format!("{l} {}", c.path)
        })
        .collect();
    let msg = message(ctx, &ws, &args, &summary)?;

    // Upload what the server lacks.
    let mut by_id: HashMap<ObjectId, &Outgoing> = HashMap::new();
    for o in &out {
        if let Some(id) = o.change.new_id {
            by_id.entry(id).or_insert(o);
        }
    }
    let ids: Vec<ObjectId> = by_id.keys().copied().collect();
    let lacking = client.missing_objects(&depot, &ids).await?;
    let mut progress = ctx.out.progress("Uploading", lacking.len() as u64);
    let uploads = futures::stream::iter(lacking.iter().map(|id| {
        let o = by_id[id];
        let client = client.clone();
        let depot = depot.clone();
        let id = *id;
        let path = o.change.path.clone();
        let local_path = o.local.clone();
        let kind = o.change.kind;
        let size = o.change.size.unwrap_or(0);
        async move {
            let r = if kind == Some(EntryKind::Symlink) {
                let target = fs::read_link(&local_path)?;
                let text = target.to_string_lossy().replace('\\', "/").into_bytes();
                client
                    .put_blob(&depot, &id, std::io::Cursor::new(text))
                    .await
            } else {
                client.put_blob_file(&depot, &id, &local_path).await
            };
            match r {
                Ok(_) => Ok(size),
                Err(lode_client::Error::Api(a))
                    if a.known_code() == Some(lode_core::ErrorCode::HashMismatch) =>
                {
                    Err(changed_while_committing(&path))
                }
                Err(e) => Err(CliError::from(e)),
            }
        }
    }))
    .buffer_unordered(PARALLEL_DOWNLOADS);
    futures::pin_mut!(uploads);
    while let Some(r) = uploads.next().await {
        progress.step(r?);
    }
    progress.finish();

    // Files edited during the upload would make the revision lie about them.
    for o in &out {
        if let Some((size, mtime)) = o.stat {
            let meta = fs::symlink_metadata(&o.local)
                .map_err(|_| changed_while_committing(&o.change.path))?;
            let size_changed = o.change.kind != Some(EntryKind::Symlink) && meta.len() != size;
            if size_changed || mtime_ns(&meta) != mtime {
                return Err(changed_while_committing(&o.change.path));
            }
        }
    }

    let pending = Pending {
        request: CommitRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            message: msg,
            changes,
            merged_from: vec![],
            provenance: None,
            extensions: Default::default(),
        },
        stats: out
            .iter()
            .filter_map(|o| o.stat.map(|s| (o.change.path.as_str().to_owned(), s)))
            .collect(),
    };
    let stored =
        serde_json::to_string(&pending).map_err(|e| CliError::failed("internal", e.to_string()))?;
    ws.db_mut()
        .write(|tx| tx.set_config(PENDING_COMMIT, &stored))?;

    let resp = match client.commit(&depot, &branch, &pending.request).await {
        Ok(r) => r,
        Err(e) if maybe_committed(&e) => {
            let mut err = explain(e);
            err.message = format!(
                "{}; the server may have committed it: run `lode commit` again to find out and finish",
                err.message
            );
            err.exit = Exit::Network;
            return Err(err);
        }
        Err(e) => {
            ws.db_mut().write(|tx| tx.remove_config(PENDING_COMMIT))?;
            return Err(explain(e));
        }
    };
    apply(&mut ws, &pending, true)?;
    let n = pending.request.changes.len();
    ctx.out.line(format!(
        "Committed r{} ({n} file{}).",
        resp.rev,
        if n == 1 { "" } else { "s" }
    ));
    Ok(json!({
        "rev": resp.rev,
        "branch": resp.branch,
        "files": n,
        "uploaded": lacking.len(),
        "replayed": resp.replayed,
        "resumed": false,
    }))
}
