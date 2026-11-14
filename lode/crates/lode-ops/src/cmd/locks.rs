//! `lode lock`, `lode unlock`, `lode locks` (SPEC §6.1, ARCHITECTURE §9.1) and keeping
//! lock-required files read-only on disk.
//!
//! Files matching `[locks].required` in the workspace's `.lodeconfig` are read-only on disk
//! unless this workspace holds their lock: after checkout, update, commit and unlock they are
//! made read-only, and `lock` makes them writable. The read-only bit is only a reminder; the
//! server enforces the rule at commit time.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lode_client::Client;
use lode_core::{DepotPath, EntryKind, ErrorCode, LodeConfig, PathGlobSet};
use lode_proto::locks::{LockPath, LockRequest, LockView, LocksQuery, UnlockRequest};
use lode_workspace::db::keys;
use lode_workspace::scan::stat_for_row;
use lode_workspace::{FileRow, FileState, Workspace};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, under};

/// `lode lock` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Take exclusive locks on files of this branch.

Only the holder may commit changes to a locked file. All named files are locked or none.
A file must be at the branch head version: if it changed on the server since you synced,
run `lode update` first. Files that need a lock (`[locks] required` in .lodeconfig) become
writable once locked. A commit releases the locks of the files it sends unless
`--keep-locks` is given.

With --json: {\"locked\":[paths]}.")]
pub struct LockArgs {
    /// Files and directories (their tracked files).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// A note for others, such as why you need the file.
    #[arg(short = 'm', long = "message")]
    pub comment: Option<String>,
}

/// `lode unlock` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Release locks.

Releases this workspace's locks on the named files. With --force, a depot administrator
breaks other people's locks. Files that need a lock become read-only again.

With --json: {\"released\":[paths]}.")]
pub struct UnlockArgs {
    /// Files and directories.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Break locks held by others (needs admin on the depot).
    #[arg(long)]
    pub force: bool,
}

/// `lode locks` options.
#[derive(Debug, Default, clap::Args)]
#[command(
    long_about = "List the locks of this branch: path, holder, workspace, time and comment.

Also brings this workspace's record of its own locks up to date with the server.

With --json: {\"locks\":[{\"branch\",\"path\",\"owner\":{\"name\",\"kind\"},\"workspace\",
\"comment\",\"created_at\",\"mine\":bool}]}. `mine` is true for this workspace's locks."
)]
pub struct LocksArgs {
    /// Only locks under this file or directory.
    pub path: Option<PathBuf>,
    /// Only your own locks.
    #[arg(long)]
    pub mine: bool,
}

/// The `[locks].required` patterns of the workspace's `.lodeconfig`.
pub fn required(ws: &Workspace) -> PathGlobSet {
    fs::read(ws.root().join(".lodeconfig"))
        .ok()
        .and_then(|b| LodeConfig::parse(&b).ok())
        .map(|c| c.lock_required)
        .unwrap_or_else(PathGlobSet::empty)
}

/// Makes a file read-only or writable. Symlinks are left alone.
pub fn set_read_only(local: &Path, read_only: bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(local)?;
    if !meta.is_file() {
        return Ok(());
    }
    let mut perm = meta.permissions();
    if perm.readonly() == read_only {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perm.mode();
        perm.set_mode(if read_only {
            mode & !0o222
        } else {
            mode | 0o200
        });
    }
    #[cfg(not(unix))]
    {
        // Clears the Windows read-only attribute; there are no world-write bits to set.
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(read_only);
    }
    fs::set_permissions(local, perm)
}

/// Whether the file on disk still is what the row describes: by size and mtime when the
/// recorded mtime can be trusted, otherwise (a file just written, say) by its content.
fn unmodified(local: &Path, row: &FileRow) -> bool {
    let Ok((size, mtime)) = stat_for_row(local, SystemTime::now()) else {
        return false;
    };
    if size != row.size {
        return false;
    }
    if row.mtime_ns != 0 && mtime == row.mtime_ns {
        return true;
    }
    row.base_id.is_some()
        && fs::File::open(local)
            .and_then(|f| lode_core::blob_id(&f))
            .is_ok_and(|id| Some(id) == row.base_id)
}

/// Sets the read-only bit of lock-required files among `rows`: writable when this workspace
/// holds the lock, read-only when it does not and the file has no local changes.
fn protect_rows(ws: &Workspace, required: &PathGlobSet, rows: &[FileRow]) {
    if required.is_empty() {
        return;
    }
    for row in rows {
        if row.kind != EntryKind::File || !required.matches(&row.path) {
            continue;
        }
        let local = ws.local_path(&row.path);
        // The bit is a reminder only, so a file that refuses it is left as it is.
        let _ = if row.lock_held {
            set_read_only(&local, false)
        } else if row.state == FileState::Normal && unmodified(&local, row) {
            set_read_only(&local, true)
        } else {
            Ok(())
        };
    }
}

/// [`protect_rows`] for the given paths.
pub fn protect(ws: &Workspace, paths: &[DepotPath]) -> OpResult<()> {
    let required = required(ws);
    if required.is_empty() {
        return Ok(());
    }
    let mut rows = Vec::with_capacity(paths.len());
    for p in paths {
        if let Some(r) = ws.db().file(p)? {
            rows.push(r);
        }
    }
    protect_rows(ws, &required, &rows);
    Ok(())
}

/// [`protect_rows`] for every tracked file.
pub fn protect_all(ws: &Workspace) -> OpResult<()> {
    let required = required(ws);
    if required.is_empty() {
        return Ok(());
    }
    protect_rows(ws, &required, &ws.db().files()?);
    Ok(())
}

fn module_disabled() -> OpError {
    OpError::failed(
        "module_disabled",
        "this server does not have the locks module enabled",
    )
}

/// Maps a refusal of the locks endpoints to what the user can do about it.
async fn explain(client: &Client, e: lode_client::Error) -> OpError {
    if e.is_code(ErrorCode::NotFound) && e.api().is_some_and(|a| a.paths().is_empty()) {
        // The route itself is missing when the module is off.
        if let Ok(info) = client.info().await {
            if !info.modules.iter().any(|m| m == "locks") {
                return module_disabled();
            }
        }
    }
    let paths: Vec<String> = e
        .api()
        .map(|a| a.paths().iter().map(|p| p.to_string()).collect())
        .unwrap_or_default();
    let mut err = OpError::from(e);
    let hint = match err.code.as_str() {
        "out_of_date" => Some("changed on the server since you synced: run `lode update` first"),
        "locked" => Some("held by someone else (see `lode locks`)"),
        _ => None,
    };
    if let Some(h) = hint {
        err.message = format!("{}: {}; {h}", err.message, paths.join(", "));
    }
    err
}

struct Target {
    ws: Workspace,
    client: Client,
    depot: String,
    branch: String,
    workspace_id: String,
}

fn open(ctx: &Ctx) -> OpResult<Target> {
    let ws = local::open(ctx)?;
    let config = |k: &str| -> OpResult<String> {
        ws.db()
            .config(k)?
            .ok_or_else(|| OpError::failed("workspace", format!("workspace has no {k}")))
    };
    let depot = config(keys::DEPOT)?;
    let branch = config(keys::BRANCH)?;
    let workspace_id = config(keys::WORKSPACE_ID)?;
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;
    Ok(Target {
        ws,
        client,
        depot,
        branch,
        workspace_id,
    })
}

/// Tracked rows under the named paths; `not_found` for a name with none.
fn rows_under(ws: &Workspace, named: &[DepotPath]) -> OpResult<Vec<FileRow>> {
    let rows = ws.db().files()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut unknown = Vec::new();
    for p in named {
        let before = out.len();
        for r in rows.iter().filter(|r| under(&r.path, p)) {
            if seen.insert(r.path.clone()) {
                out.push(r.clone());
            }
        }
        if out.len() == before && !rows.iter().any(|r| under(&r.path, p)) {
            unknown.push(p.as_str().to_owned());
        }
    }
    if !unknown.is_empty() {
        return Err(local::not_found("not tracked", &unknown));
    }
    Ok(out)
}

/// Runs `lode lock`.
pub async fn lock(ctx: &Ctx, args: LockArgs) -> OpResult<Value> {
    let mut t = open(ctx)?;
    let _lock = t.ws.lock()?;
    let named = local::depot_paths(ctx, &t.ws, &args.paths)?;
    let rows = rows_under(&t.ws, &named)?;
    let new: Vec<String> = rows
        .iter()
        .filter(|r| r.base_id.is_none() || r.state == FileState::Added)
        .map(|r| r.path.as_str().to_owned())
        .collect();
    if !new.is_empty() {
        return Err(OpError::failed(
            "not_found",
            format!(
                "not on the server yet, so there is nothing to lock: {}",
                new.join(", ")
            ),
        )
        .with_details(json!({ "paths": new })));
    }
    let req = LockRequest {
        branch: t.branch.clone(),
        workspace: t.workspace_id.clone(),
        comment: args.comment.clone(),
        paths: rows
            .iter()
            .filter_map(|r| {
                r.base_id.map(|id| LockPath {
                    path: r.path.clone(),
                    base_id: id,
                })
            })
            .collect(),
    };
    let list = match t.client.lock(&t.depot, &req).await {
        Ok(l) => l,
        Err(e) => return Err(explain(&t.client, e).await),
    };
    let locked: Vec<DepotPath> = list.locks.iter().map(|l| l.path.clone()).collect();
    set_held(&mut t.ws, &locked, true)?;
    protect(&t.ws, &locked)?;
    for p in &locked {
        ctx.out.line(format!("Locked {p}"));
    }
    let locked: Vec<&str> = locked.iter().map(DepotPath::as_str).collect();
    Ok(json!({ "locked": locked }))
}

fn set_held(ws: &mut Workspace, paths: &[DepotPath], held: bool) -> OpResult<()> {
    ws.db_mut().write(|tx| {
        for p in paths {
            tx.set_lock_held(p, held)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Runs `lode unlock`.
pub async fn unlock(ctx: &Ctx, args: UnlockArgs) -> OpResult<Value> {
    let mut t = open(ctx)?;
    let _lock = t.ws.lock()?;
    let named = local::depot_paths(ctx, &t.ws, &args.paths)?;
    // A forced unlock may name files this workspace does not track.
    let paths: Vec<DepotPath> = if args.force {
        let rows = t.ws.db().files()?;
        let mut out: Vec<DepotPath> = Vec::new();
        for p in &named {
            let tracked: Vec<DepotPath> = rows
                .iter()
                .filter(|r| under(&r.path, p))
                .map(|r| r.path.clone())
                .collect();
            if tracked.is_empty() {
                out.push(p.clone());
            } else {
                out.extend(tracked);
            }
        }
        out
    } else {
        rows_under(&t.ws, &named)?
            .into_iter()
            .map(|r| r.path)
            .collect()
    };
    let req = UnlockRequest {
        branch: t.branch.clone(),
        paths,
        force: args.force,
    };
    let resp = match t.client.unlock(&t.depot, &req).await {
        Ok(r) => r,
        Err(e) => return Err(explain(&t.client, e).await),
    };
    set_held(&mut t.ws, &resp.released, false)?;
    protect(&t.ws, &resp.released)?;
    for p in &resp.released {
        ctx.out.line(format!("Unlocked {p}"));
    }
    if resp.released.is_empty() {
        ctx.out.line("Nothing was locked.");
    }
    let released: Vec<&str> = resp.released.iter().map(DepotPath::as_str).collect();
    Ok(json!({ "released": released }))
}

/// This workspace's locks on the server, by path.
async fn own_locks(
    client: &Client,
    depot: &str,
    branch: &str,
    workspace_id: &str,
    me: &str,
) -> Result<BTreeMap<String, LockView>, lode_client::Error> {
    let q = LocksQuery {
        branch: Some(branch.to_owned()),
        prefix: None,
        owner: Some(me.to_owned()),
    };
    let list = client.locks(depot, &q).await?;
    Ok(list
        .locks
        .into_iter()
        .filter(|l| l.workspace == workspace_id)
        .map(|l| (l.path.as_str().to_owned(), l))
        .collect())
}

/// Brings `lock_held` in line with `held` (the server's view), returning the paths whose
/// lock was lost and those gained.
fn reconcile_rows(
    ws: &mut Workspace,
    held: &BTreeMap<String, LockView>,
) -> OpResult<(Vec<DepotPath>, Vec<DepotPath>)> {
    let rows = ws.db().files()?;
    let lost: Vec<DepotPath> = rows
        .iter()
        .filter(|r| r.lock_held && !held.contains_key(r.path.as_str()))
        .map(|r| r.path.clone())
        .collect();
    let gained: Vec<DepotPath> = rows
        .iter()
        .filter(|r| !r.lock_held && held.contains_key(r.path.as_str()))
        .map(|r| r.path.clone())
        .collect();
    set_held(ws, &lost, false)?;
    set_held(ws, &gained, true)?;
    Ok((lost, gained))
}

/// After an update: if this workspace believes it holds locks, asks the server which it still
/// does, and tells the user about any that were broken. Quietly does nothing when the module
/// is off or the server cannot be asked.
pub async fn reconcile_after_update(
    ctx: &Ctx,
    ws: &mut Workspace,
    client: &Client,
) -> OpResult<()> {
    if !ws.db().files()?.iter().any(|r| r.lock_held) {
        return Ok(());
    }
    let (Some(depot), Some(branch), Some(workspace_id)) = (
        ws.db().config(keys::DEPOT)?,
        ws.db().config(keys::BRANCH)?,
        ws.db().config(keys::WORKSPACE_ID)?,
    ) else {
        return Ok(());
    };
    let Ok(me) = client.whoami().await else {
        return Ok(());
    };
    let held = match own_locks(client, &depot, &branch, &workspace_id, &me.principal.name).await {
        Ok(h) => h,
        // The update itself worked; the next `lode locks` or update tries again.
        Err(_) => return Ok(()),
    };
    let (lost, _) = reconcile_rows(ws, &held)?;
    for p in &lost {
        ctx.out.warn(format!(
            "your lock on {p} was released on the server (broken by an administrator?)"
        ));
    }
    protect(ws, &lost)?;
    Ok(())
}

/// Runs `lode locks`.
pub async fn locks(ctx: &Ctx, args: LocksArgs) -> OpResult<Value> {
    let mut t = open(ctx)?;
    let me = t.client.whoami().await?.principal.name;
    let held = match own_locks(&t.client, &t.depot, &t.branch, &t.workspace_id, &me).await {
        Ok(h) => h,
        Err(e) => return Err(explain(&t.client, e).await),
    };
    {
        let _lock = t.ws.lock()?;
        let (lost, gained) = reconcile_rows(&mut t.ws, &held)?;
        protect(&t.ws, &lost)?;
        protect(&t.ws, &gained)?;
    }
    let prefix = match &args.path {
        Some(p) => {
            let dp = local::depot_path(ctx, &t.ws, p)?;
            // A directory covers what is below it, not names that merely start alike.
            let is_dir = fs::metadata(ctx.local(p)).is_ok_and(|m| m.is_dir())
                || t.ws
                    .db()
                    .files()?
                    .iter()
                    .any(|r| r.path != dp && under(&r.path, &dp));
            Some(if is_dir {
                format!("{dp}/")
            } else {
                dp.as_str().to_owned()
            })
        }
        None => None,
    };
    let q = LocksQuery {
        branch: Some(t.branch.clone()),
        prefix,
        owner: args.mine.then(|| me.clone()),
    };
    let list = match t.client.locks(&t.depot, &q).await {
        Ok(l) => l,
        Err(e) => return Err(explain(&t.client, e).await),
    };
    let mut out = Vec::new();
    for l in &list.locks {
        let mine = l.owner.name == me && l.workspace == t.workspace_id;
        let when = l
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let mut line = format!(
            "{}  {}{}  {when}",
            l.path,
            l.owner.name,
            if mine { " (this workspace)" } else { "" }
        );
        if let Some(c) = &l.comment {
            line.push_str(&format!("  {c}"));
        }
        ctx.out.line(line);
        let mut v =
            serde_json::to_value(l).map_err(|e| OpError::failed("internal", e.to_string()))?;
        v["mine"] = json!(mine);
        out.push(v);
    }
    if list.locks.is_empty() {
        ctx.out.line("No locks.");
    }
    Ok(json!({ "locks": out }))
}
