//! `lode commit` (ARCHITECTURE §10.3): scan, hash, upload what the server lacks, commit, and
//! bring the rows up to the new revision.
//!
//! The request, with its id, is stored in the workspace before it is sent. If the answer is
//! lost (a crash, a timeout, a 5xx), the next `lode commit` sends the same request again, and
//! the server answers a request id it has seen with the revision it already made.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures::{StreamExt, TryStreamExt};
use lode_client::Client;
use lode_core::{validate_changes, Change, ChangeAction, DepotPath, EntryKind, ObjectId};
use lode_proto::commit::{CommitRequest, EXT_FORCE_FLOW, EXT_OVERRIDE_SHELF_ONLY};
use lode_proto::read::ManifestDelta;
use lode_workspace::db::keys;
use lode_workspace::scan::{symlink_id, trusted_mtime};
use lode_workspace::{FileRow, FileState, Status, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cmd::merge::{describe, pending_merges, PENDING_FORCE_FLOW, PENDING_MERGE};
use crate::context::Ctx;
use crate::error::{Exit, OpError, OpResult};
use crate::local;
use crate::materialize::PARALLEL_DOWNLOADS;

/// Commit extension asking the locks module to keep the author's locks.
const KEEP_LOCKS: &str = "locks.keep";

/// Config key holding a commit that was sent but whose answer was not seen.
pub const PENDING_COMMIT: &str = "pending_commit";

/// `lode commit` options.
#[derive(Debug, Default, clap::Args)]
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
    /// Keep the locks of the committed files (by default the commit releases them).
    #[arg(long)]
    pub keep_locks: bool,
    /// Only the files of this changelist; its description is the default message.
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
    /// Ask the server to skip the stream flow rules for the merge this commit records (depot
    /// admins only; `lode merge --force-flow` sets it for you).
    #[arg(long)]
    pub force_flow: bool,
    /// Commit directly to a branch that takes shelf submits only (`[branches].shelf_only`),
    /// giving the reason the revision records. Depot admins only.
    #[arg(long = "override", value_name = "REASON")]
    pub override_reason: Option<String>,
    /// Do not run the depot's validation scripts first.
    #[arg(long)]
    pub no_verify: bool,
}

/// The refusal for a direct commit to a `[branches].shelf_only` branch, as the server words
/// it, with what to do instead.
pub(crate) fn shelf_required(branch: &str) -> OpError {
    let mut e = OpError::failed(
        "shelf_required",
        format!(
            "branch `{branch}` takes changes through shelves only; {}",
            advice("shelf_required").unwrap_or_default()
        ),
    );
    e.details = json!({ "branch": branch });
    e
}

/// A commit in flight, as stored under [`PENDING_COMMIT`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    request: CommitRequest,
    /// Size and mtime of each sent file when it was hashed.
    stats: HashMap<String, (u64, i64)>,
    /// Changelists the sent files were in; those left empty are deleted afterwards.
    #[serde(default)]
    changelists: Vec<i64>,
}

/// One file to send: the change and where its content is.
pub(crate) struct Outgoing {
    pub change: Change,
    pub local: PathBuf,
    /// Size and mtime when hashed.
    pub stat: Option<(u64, i64)>,
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
fn hash_stable(path: &DepotPath, local: &Path, kind: EntryKind) -> OpResult<(ObjectId, u64, i64)> {
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

fn changed_while_committing(path: &DepotPath) -> OpError {
    OpError::failed(
        "changed_during_commit",
        format!("{path} changed while it was being committed; nothing was committed, run the commit again"),
    )
    .with_details(json!({ "paths": [path.as_str()] }))
}

/// The commit message from `-m`, `-F` or the editor, without `#` comment lines.
fn message(
    ctx: &Ctx,
    ws: &Workspace,
    args: &CommitArgs,
    summary: &[String],
    default: Option<&str>,
) -> OpResult<String> {
    let raw = if let Some(m) = &args.message {
        m.clone()
    } else if let Some(f) = &args.file {
        if f.as_os_str() == "-" {
            ctx.out.ui().read_all()?
        } else {
            fs::read_to_string(ctx.local(f))?
        }
    } else if let Some(d) = default.filter(|d| !d.trim().is_empty()) {
        d.to_owned()
    } else {
        let editor = std::env::var("VISUAL")
            .ok()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.trim().is_empty());
        let (Some(editor), false, true) = (editor, ctx.g.json, ctx.out.ui().interactive()) else {
            return Err(OpError::usage(
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
        if !ctx.out.ui().edit(&editor, &file)? {
            return Err(OpError::failed(
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
        return Err(OpError::failed(
            "aborted",
            "empty commit message; nothing was committed",
        ));
    }
    Ok(text)
}

/// What the server says next, for the errors a user can act on.
fn advice(code: &str) -> Option<&'static str> {
    Some(match code {
        "out_of_date" => "these files changed on the server since you synced them: run `lode update`, then commit again",
        "locked" => "someone else holds the lock on these files",
        "lock_required" => "these files need a lock: run `lode lock <path>` first",
        "permission_denied" => "you may not write these paths; ask a depot admin",
        "shelf_required" => "shelve your changes with `lode shelve`, then submit the shelf with `lode shelf submit ID@VERSION`",
        _ => return None,
    })
}

/// A server refusal with the affected paths and what to do next.
pub(crate) fn explain(e: lode_client::Error) -> OpError {
    let paths: Vec<String> = e
        .api()
        .map(|a| a.paths().iter().map(|p| p.to_string()).collect())
        .unwrap_or_default();
    let mut err = OpError::from(e);
    if err.code == "shelf_required" {
        let branch = err
            .details
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or("?");
        return shelf_required(branch);
    }
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
fn apply(ws: &mut Workspace, p: &Pending, trust_stats: bool) -> OpResult<()> {
    let now = SystemTime::now();
    let keep_locks = p
        .request
        .extensions
        .get(KEEP_LOCKS)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let held: std::collections::HashSet<String> = if keep_locks {
        ws.db()
            .files()?
            .into_iter()
            .filter(|r| r.lock_held)
            .map(|r| r.path.as_str().to_owned())
            .collect()
    } else {
        Default::default()
    };
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
                    row.lock_held = held.contains(c.path.as_str());
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
        // Committed files left their changelist; one left empty has served its purpose.
        for id in &p.changelists {
            if tx.changelist_size(*id)? == 0 {
                tx.delete_changelist(*id)?;
            }
        }
        if !p.request.merged_from.is_empty() {
            tx.remove_config(PENDING_MERGE)?;
            tx.remove_config(PENDING_FORCE_FLOW)?;
        }
        tx.remove_config(PENDING_COMMIT)
    })?;
    let paths: Vec<DepotPath> = p.request.changes.iter().map(|c| c.path.clone()).collect();
    crate::cmd::locks::protect(ws, &paths)?;
    Ok(())
}

/// Advances the synced revision to a committed one when the commit is the only difference
/// between the two revisions' trees: then the pristine rows `apply` just wrote are exactly
/// the new tree. When anything else came in between (or the check itself fails), the synced
/// revision stays and the next update reconciles as before.
async fn advance_synced(
    ws: &mut Workspace,
    client: &Client,
    depot: &str,
    p: &Pending,
    new_rev: u64,
) -> OpResult<()> {
    let Some(from) = ws.db().config_u64(keys::SYNCED_REV)? else {
        return Ok(());
    };
    if from >= new_rev {
        return Ok(());
    }
    let Ok(view) = ws.view_spec() else {
        return Ok(());
    };
    let checked: OpResult<bool> = async {
        let stream = client
            .manifest_delta(depot, from, new_rev, &view.include, &view.exclude)
            .await?;
        let entries: Vec<ManifestDelta> = stream.try_collect().await?;
        Ok(entries.iter().all(|d| {
            let path = match d {
                ManifestDelta::Put(e) => &e.path,
                ManifestDelta::Del { path } => path,
            };
            p.request.changes.iter().any(|c| &c.path == path)
        }))
    }
    .await;
    if checked.unwrap_or(false) {
        ws.db_mut()
            .write(|tx| tx.set_config(keys::SYNCED_REV, &new_rev.to_string()))?;
    }
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
) -> OpResult<Value> {
    ctx.out
        .warn("an earlier commit did not get an answer; sending it again");
    match client.commit(depot, branch, &pending.request).await {
        Ok(resp) => {
            apply(ws, &pending, false)?;
            advance_synced(ws, client, depot, &pending, resp.rev).await?;
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
                "bytes_uploaded": 0,
                "bytes_reused": 0,
                "replayed": resp.replayed,
                "resumed": true,
                "forced_flow": pending.request.extensions.contains_key(EXT_FORCE_FLOW),
                "shelf_only_override": pending.request.extensions.contains_key(EXT_OVERRIDE_SHELF_ONLY),
            }))
        }
        Err(e) if maybe_committed(&e) => Err(explain(e)),
        Err(e) => {
            ws.db_mut().write(|tx| tx.remove_config(PENDING_COMMIT))?;
            Err(explain(e))
        }
    }
}

/// The changed files under `paths` (of one changelist when `list` is set) as changes to send,
/// with the changelists they were in. Refuses conflicted and missing files.
pub(crate) fn gather(
    ctx: &Ctx,
    ws: &mut Workspace,
    paths: &[DepotPath],
    list_id: Option<Option<i64>>,
) -> OpResult<(Vec<Outgoing>, Vec<i64>)> {
    let report = local::scan(ctx, ws, paths, false)?;

    let mut conflicted = Vec::new();
    let mut missing = Vec::new();
    let mut out: Vec<Outgoing> = Vec::new();
    let mut lists: Vec<i64> = Vec::new();
    for e in report.changed() {
        let Some(row) = &e.row else { continue };
        if list_id.is_some_and(|l| l != row.changelist_id) {
            continue;
        }
        if let Some(id) = row.changelist_id {
            if !lists.contains(&id) {
                lists.push(id);
            }
        }
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
        return Err(OpError::failed(
            "conflicted",
            format!(
                "resolve these conflicts first (`lode resolve`): {}",
                conflicted.join(", ")
            ),
        )
        .with_details(json!({ "paths": conflicted })));
    }
    if !missing.is_empty() {
        return Err(OpError::failed(
            "missing",
            format!(
                "these files are gone without `lode rm`: {}; remove them with `lode rm` or restore them with `lode revert`",
                missing.join(", ")
            ),
        )
        .with_details(json!({ "paths": missing })));
    }
    Ok((out, lists))
}

/// What [`upload`] sent.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Uploaded {
    /// Blobs the server lacked.
    pub blobs: usize,
    /// Content bytes sent and bytes the server already had (chunks of large files).
    pub transfer: lode_client::Transfer,
}

/// Uploads the content the server lacks and checks that no file changed meanwhile. Large
/// files go in chunks, so only the chunks the server lacks are sent (ARCHITECTURE §3.5).
pub(crate) async fn upload(
    ctx: &Ctx,
    client: &Client,
    depot: &str,
    out: &[Outgoing],
) -> OpResult<Uploaded> {
    let mut by_id: HashMap<ObjectId, &Outgoing> = HashMap::new();
    for o in out {
        if let Some(id) = o.change.new_id {
            by_id.entry(id).or_insert(o);
        }
    }
    let ids: Vec<ObjectId> = by_id.keys().copied().collect();
    let lacking = client.missing_objects(depot, &ids).await?;
    let mut progress = ctx.out.progress("Uploading", lacking.len() as u64);
    let uploads = futures::stream::iter(lacking.iter().map(|id| {
        let o = by_id[id];
        let client = client.clone();
        let depot = depot.to_owned();
        let id = *id;
        let path = o.change.path.clone();
        let local_path = o.local.clone();
        let kind = o.change.kind;
        let size = o.change.size.unwrap_or(0);
        async move {
            let r = if kind == Some(EntryKind::Symlink) {
                let target = fs::read_link(&local_path)?;
                let text = target.to_string_lossy().replace('\\', "/").into_bytes();
                let n = text.len() as u64;
                client
                    .put_blob(&depot, &id, std::io::Cursor::new(text))
                    .await
                    .map(|_| lode_client::Transfer {
                        network: n,
                        reused: 0,
                    })
            } else {
                client.upload_file(&depot, &id, &local_path, size).await
            };
            match r {
                Ok(t) => Ok((size, t)),
                Err(lode_client::Error::HashMismatch { .. }) => {
                    Err(changed_while_committing(&path))
                }
                Err(lode_client::Error::Api(a))
                    if a.known_code() == Some(lode_core::ErrorCode::HashMismatch) =>
                {
                    Err(changed_while_committing(&path))
                }
                Err(lode_client::Error::Api(a))
                    if a.known_code() == Some(lode_core::ErrorCode::Obliterated) =>
                {
                    Err(OpError::failed(
                        "obliterated",
                        format!(
                            "{path} holds content that was obliterated from this depot and \
                             cannot be committed again"
                        ),
                    )
                    .with_details(json!({ "paths": [path.as_str()] })))
                }
                Err(e) => Err(OpError::from(e)),
            }
        }
    }))
    .buffer_unordered(PARALLEL_DOWNLOADS);
    futures::pin_mut!(uploads);
    let mut done = Uploaded {
        blobs: lacking.len(),
        ..Uploaded::default()
    };
    while let Some(r) = uploads.next().await {
        let (size, t) = r?;
        done.transfer += t;
        progress.step(size);
    }
    progress.finish();

    // Files edited during the upload would make the revision lie about them.
    for o in out {
        if let Some((size, mtime)) = o.stat {
            let meta = fs::symlink_metadata(&o.local)
                .map_err(|_| changed_while_committing(&o.change.path))?;
            let size_changed = o.change.kind != Some(EntryKind::Symlink) && meta.len() != size;
            if size_changed || mtime_ns(&meta) != mtime {
                return Err(changed_while_committing(&o.change.path));
            }
        }
    }
    Ok(done)
}

/// Runs `lode commit`.
pub async fn commit(ctx: &Ctx, args: CommitArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?;
    let branch = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no branch"))?;
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;

    if let Some(text) = ws.db().config(PENDING_COMMIT)? {
        let pending: Pending = serde_json::from_str(&text)
            .map_err(|e| OpError::failed("workspace", format!("stored commit: {e}")))?;
        return finish_pending(ctx, &mut ws, &client, &depot, &branch, pending).await;
    }

    // Refuse before uploading anything; the server checks again against the branch head.
    if args.override_reason.is_none()
        && crate::cmd::update::local_config(&ws).is_some_and(|c| c.shelf_only.matches_str(&branch))
    {
        return Err(shelf_required(&branch));
    }

    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    // `Some(None)` is `-c default`: files in no changelist.
    let list = args
        .changelist
        .as_deref()
        .map(|n| local::changelist(&ws, n))
        .transpose()?;
    let list_id = list.as_ref().map(|l| l.as_ref().map(|c| c.id));
    if args.add_all {
        let report = local::scan(ctx, &mut ws, &paths, false)?;
        let mut rows = Vec::new();
        for e in report
            .entries
            .iter()
            .filter(|e| e.status == Status::Untracked)
        {
            let mut row = crate::cmd::files::added_row(&ws.local_path(&e.path), e.path.clone())?;
            row.changelist_id = list_id.flatten();
            rows.push(row);
        }
        ws.db_mut().write(|tx| {
            for r in &rows {
                tx.put_file(r)?;
            }
            Ok(())
        })?;
    }
    let merges = pending_merges(&ws)?;
    if !merges.is_empty() && (!paths.is_empty() || list_id.is_some()) {
        return Err(OpError::usage(format!(
            "the merge of {} must be committed whole: commit without paths or -c",
            describe(&merges[0])
        )));
    }
    let (out, lists) = gather(ctx, &mut ws, &paths, list_id)?;
    if out.is_empty() && merges.is_empty() {
        let msg = match &args.changelist {
            Some(n) => format!("nothing to commit in changelist {n}"),
            None => "nothing to commit".to_owned(),
        };
        return Err(OpError::failed("nothing_to_commit", msg));
    }
    let changes: Vec<Change> = out.iter().map(|o| o.change.clone()).collect();
    validate_changes(&changes).map_err(|e| OpError::failed("invalid_change", e.to_string()))?;
    crate::cmd::validate::before_send(
        ctx,
        &ws,
        crate::cmd::validate::Phase::Commit,
        &changes,
        args.no_verify,
    )
    .await?;
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
    let default_msg = list
        .as_ref()
        .and_then(|l| l.as_ref())
        .map(|c| c.description.clone())
        .or_else(|| {
            (!merges.is_empty()).then(|| {
                let names: Vec<String> = merges.iter().map(describe).collect();
                format!("Merge {}", names.join(", ").replace('`', ""))
            })
        });
    let msg = message(ctx, &ws, &args, &summary, default_msg.as_deref())?;

    let force_flow =
        !merges.is_empty() && (args.force_flow || ws.db().config(PENDING_FORCE_FLOW)?.is_some());
    let uploaded = upload(ctx, &client, &depot, &out).await?;
    crate::crash::point("commit.uploaded");

    let pending = Pending {
        request: CommitRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            message: msg,
            changes,
            merged_from: merges,
            provenance: ctx.g.provenance(),
            extensions: {
                let mut ext = std::collections::BTreeMap::new();
                if args.keep_locks {
                    ext.insert(KEEP_LOCKS.to_owned(), Value::Bool(true));
                }
                if force_flow {
                    ext.insert(EXT_FORCE_FLOW.to_owned(), Value::Bool(true));
                }
                if let Some(reason) = &args.override_reason {
                    ext.insert(
                        EXT_OVERRIDE_SHELF_ONLY.to_owned(),
                        Value::String(reason.clone()),
                    );
                }
                ext
            },
        },
        stats: out
            .iter()
            .filter_map(|o| o.stat.map(|s| (o.change.path.as_str().to_owned(), s)))
            .collect(),
        changelists: lists,
    };
    let stored =
        serde_json::to_string(&pending).map_err(|e| OpError::failed("internal", e.to_string()))?;
    ws.db_mut()
        .write(|tx| tx.set_config(PENDING_COMMIT, &stored))?;
    crate::crash::point("commit.stored");

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
    crate::crash::point("commit.sent");
    apply(&mut ws, &pending, true)?;
    advance_synced(&mut ws, &client, &depot, &pending, resp.rev).await?;
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
        "uploaded": uploaded.blobs,
        "bytes_uploaded": uploaded.transfer.network,
        "bytes_reused": uploaded.transfer.reused,
        "replayed": resp.replayed,
        "resumed": false,
        "forced_flow": force_flow,
        "shelf_only_override": args.override_reason.is_some(),
    }))
}
