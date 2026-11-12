//! `lode update` (ARCHITECTURE §10.4) and `lode resolve`.
//!
//! An update stores its target as `update_rev` before touching anything and records each file
//! as it is done, so a rerun after an interruption takes the same delta and skips what is
//! already there (a row whose base already is the incoming id needs nothing). Merges and
//! conflicts are written first, then deletions, then downloads.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use futures::TryStreamExt;
use lode_core::{DepotPath, EntryKind, LodeConfig, ObjectId};
use lode_proto::read::{ManifestDelta, ManifestEntry};
use lode_workspace::classify::{class_by_config, class_by_content};
use lode_workspace::db::keys;
use lode_workspace::scan::{stat_for_row, symlink_id};
use lode_workspace::{ConflictRow, FileClass, FileRow, FileState, ScanEntry, Status, Workspace};
use serde_json::{json, Value};

use crate::cmd::commit::PENDING_COMMIT;
use crate::cmd::diff::TEXT_LIMIT;
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local::{self, Bases};
use crate::materialize::{fetch_entries, place_file, prepare_target, remove_and_prune};

/// Config key: the revision an unfinished update is going to.
pub const UPDATE_REV: &str = "update_rev";

/// `lode update` options.
#[derive(Debug, clap::Args)]
pub struct UpdateArgs {
    /// Revision to update to (default: the branch head).
    #[arg(short = 'r', long = "rev")]
    pub rev: Option<u64>,
}

/// `lode resolve` options.
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("how").required(true).args(["mine", "theirs", "merged"])))]
pub struct ResolveArgs {
    /// Conflicted files (or directories holding them).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Keep the local version as it was before the update.
    #[arg(long)]
    pub mine: bool,
    /// Take the incoming version.
    #[arg(long)]
    pub theirs: bool,
    /// Keep the file as it is now (conflict markers edited by hand).
    #[arg(long)]
    pub merged: bool,
}

/// What happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Added,
    Updated,
    Merged,
    Conflicted,
    Deleted,
    /// No longer visible after a full sync, but modified locally: kept, untracked.
    Kept,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Added => "added",
            Outcome::Updated => "updated",
            Outcome::Merged => "merged",
            Outcome::Conflicted => "conflicted",
            Outcome::Deleted => "deleted",
            Outcome::Kept => "kept",
        }
    }
}

/// One incoming change.
enum Incoming {
    Put(ManifestEntry),
    /// Deleted on the server.
    Del(DepotPath),
    /// Absent from a full manifest (deleted, or no longer readable).
    Gone(DepotPath),
}

impl Incoming {
    fn path(&self) -> &DepotPath {
        match self {
            Incoming::Put(e) => &e.path,
            Incoming::Del(p) | Incoming::Gone(p) => p,
        }
    }
}

/// The content id of what is on disk at `e` now.
fn disk_id(ws: &Workspace, e: &ScanEntry) -> CliResult<Option<ObjectId>> {
    let Some(d) = &e.disk else { return Ok(None) };
    if let Some(id) = d.id {
        return Ok(Some(id));
    }
    let local = ws.local_path(&e.path);
    Ok(Some(if d.kind == EntryKind::Symlink {
        symlink_id(&local)?.0
    } else {
        lode_core::blob_id(fs::File::open(&local)?)?
    }))
}

fn local_config(ws: &Workspace) -> Option<LodeConfig> {
    LodeConfig::parse(&fs::read(ws.root().join(".lodeconfig")).ok()?).ok()
}

/// Writes `bytes` to `path` through `.lode/tmp`.
fn write_through_tmp(ws: &Workspace, path: &DepotPath, bytes: &[u8], exec: bool) -> CliResult<()> {
    let tmp_dir = ws.layout().tmp();
    fs::create_dir_all(&tmp_dir)?;
    let tmp = tmp_dir.join(format!("merge-{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    let target = prepare_target(ws.root(), path)?;
    place_file(&tmp, &target, exec)?;
    Ok(())
}

/// Conflict markers with the names SPEC users see.
fn relabel(merged: Vec<u8>, rev: u64) -> Vec<u8> {
    let text = String::from_utf8_lossy(&merged).into_owned();
    let mut out = String::with_capacity(text.len() + 32);
    for line in text.split_inclusive('\n') {
        match line.trim_end_matches(['\r', '\n']) {
            "<<<<<<< ours" => out.push_str(&line.replacen("ours", "mine", 1)),
            ">>>>>>> theirs" => {
                out.push_str(&line.replacen("theirs", &format!("theirs r{rev}"), 1))
            }
            _ => out.push_str(line),
        }
    }
    // Content that was not UTF-8 is left as merged.
    if String::from_utf8(merged.clone()).is_ok() {
        out.into_bytes()
    } else {
        merged
    }
}

struct Run<'a> {
    ctx: &'a Ctx,
    bases: Bases<'a>,
    rev: u64,
    config: Option<LodeConfig>,
    outcomes: BTreeMap<String, Outcome>,
}

impl Run<'_> {
    fn note(&mut self, path: &DepotPath, o: Outcome) {
        self.outcomes.insert(path.as_str().to_owned(), o);
    }

    fn is_text(&self, path: &DepotPath, sides: &[&[u8]]) -> bool {
        match class_by_config(path, self.config.as_ref()) {
            Some(c) => c == FileClass::Text,
            None => sides
                .iter()
                .all(|s| s.len() as u64 <= TEXT_LIMIT && class_by_content(s) == FileClass::Text),
        }
    }

    /// Records a conflict on `path`: the row (base now the incoming id) and the conflict.
    fn conflict(
        &mut self,
        ws: &mut Workspace,
        row: FileRow,
        old_base: Option<ObjectId>,
        theirs: Option<&ManifestEntry>,
        mine_id: Option<ObjectId>,
    ) -> CliResult<()> {
        // Already recorded by an interrupted run of this update: keep what it set aside.
        if ws
            .db()
            .conflicts()?
            .iter()
            .any(|c| c.path == row.path && c.theirs_rev == self.rev)
        {
            self.note(&row.path, Outcome::Conflicted);
            return Ok(());
        }
        let mut row = row;
        row.state = FileState::Conflicted;
        row.base_id = theirs.map(|t| t.id);
        if let Some(t) = theirs {
            row.kind = t.kind;
            row.executable = t.executable;
        }
        row.mtime_ns = 0;
        let c = ConflictRow {
            path: row.path.clone(),
            base_id: old_base,
            theirs_id: theirs.map(|t| t.id),
            theirs_rev: self.rev,
            mine_id,
        };
        ws.db_mut().write(|tx| {
            tx.put_file(&row)?;
            tx.put_conflict(&c)
        })?;
        self.note(&row.path, Outcome::Conflicted);
        Ok(())
    }

    /// A locally modified file that also changed on the server.
    async fn merge(
        &mut self,
        ws: &mut Workspace,
        e: &ScanEntry,
        row: &FileRow,
        theirs: &ManifestEntry,
    ) -> CliResult<()> {
        let local = ws.local_path(&e.path);
        let is_file = row.kind == EntryKind::File
            && theirs.kind == EntryKind::File
            && e.disk.as_ref().is_some_and(|d| d.kind == EntryKind::File);
        let small =
            e.disk.as_ref().is_some_and(|d| d.size <= TEXT_LIMIT) && theirs.size <= TEXT_LIMIT;
        if is_file && small {
            if let Some(base_id) = row.base_id {
                let base = self.bases.read(&base_id).await?;
                let their_bytes = self.bases.read(&theirs.id).await?;
                let mine = fs::read(&local)?;
                if self.is_text(&e.path, &[&base, &mine, &their_bytes]) {
                    let mut opts = diffy::MergeOptions::new();
                    opts.set_conflict_style(diffy::ConflictStyle::Merge)
                        .set_incomplete_hunk_style(diffy::IncompleteHunkStyle::Git);
                    let exec = e.disk.as_ref().is_some_and(|d| d.executable);
                    match opts.merge_bytes(&base, &mine, &their_bytes) {
                        Ok(merged) => {
                            write_through_tmp(ws, &e.path, &merged, exec)?;
                            let mut r = row.clone();
                            r.base_id = Some(theirs.id);
                            r.executable = theirs.executable;
                            r.state = FileState::Normal;
                            r.mtime_ns = 0;
                            ws.db_mut().write(|tx| tx.put_file(&r))?;
                            self.note(&e.path, Outcome::Merged);
                        }
                        Err(conflicted) => {
                            // Keep the local version for `resolve --mine`.
                            let mine_id = lode_core::blob_id(&mine[..])?;
                            let keep = local::mine_path(ws, &mine_id);
                            fs::create_dir_all(ws.layout().conflicts())?;
                            if !keep.exists() {
                                fs::write(&keep, &mine)?;
                            }
                            write_through_tmp(ws, &e.path, &relabel(conflicted, self.rev), exec)?;
                            self.conflict(
                                ws,
                                row.clone(),
                                row.base_id,
                                Some(theirs),
                                Some(mine_id),
                            )?;
                        }
                    }
                    return Ok(());
                }
            }
        }
        // Binary (or a symlink): keep the local file, put theirs beside it.
        if theirs.kind == EntryKind::File {
            let bytes_path = local::theirs_path(&local, self.rev);
            let content = self.bases.read(&theirs.id).await?;
            fs::write(&bytes_path, content)?;
        }
        self.conflict(ws, row.clone(), row.base_id, Some(theirs), None)
    }
}

/// Runs `lode update`.
pub async fn update(ctx: &Ctx, args: UpdateArgs) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    if ws.db().config(PENDING_COMMIT)?.is_some() {
        return Err(CliError::failed(
            "pending_commit",
            "an earlier commit did not get an answer; run `lode commit` first",
        ));
    }
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
    let synced = local::synced_rev(&ws)?;

    let resuming = ws.db().config_u64(UPDATE_REV)?;
    let target = match (resuming, args.rev) {
        (Some(r), Some(asked)) if r != asked => {
            return Err(CliError::usage(format!(
                "an update to r{r} did not finish; run `lode update` (or `-r {r}`) to complete it first"
            )))
        }
        (Some(r), _) => r,
        (None, Some(r)) => r,
        (None, None) => client.branch(&depot, &branch).await?.head_rev,
    };
    if resuming.is_none() {
        let conflicts = ws.db().conflicts()?;
        if !conflicts.is_empty() {
            let paths: Vec<String> = conflicts
                .iter()
                .map(|c| c.path.as_str().to_owned())
                .collect();
            return Err(CliError::failed(
                "conflicted",
                format!(
                    "resolve these conflicts first (`lode resolve`): {}",
                    paths.join(", ")
                ),
            )
            .with_details(json!({ "paths": paths })));
        }
    }

    let view = ws.view_spec()?;
    let stored_epoch = ws.db().config_u64(keys::ACCESS_EPOCH)?;
    let delta = client
        .manifest_delta(&depot, synced, target, &view.include, &view.exclude)
        .await?;
    let mut epoch = delta.access_epoch();
    let full = epoch.is_some() && epoch != stored_epoch;
    let mut incoming: Vec<Incoming> = Vec::new();
    if full {
        drop(delta);
        let stream = client
            .manifest(&depot, target, &view.include, &view.exclude)
            .await?;
        epoch = stream.access_epoch().or(epoch);
        let entries: Vec<ManifestEntry> = stream.try_collect().await?;
        let present: HashSet<DepotPath> = entries.iter().map(|e| e.path.clone()).collect();
        for r in ws.db().files()? {
            if r.state != FileState::Added && !present.contains(&r.path) {
                incoming.push(Incoming::Gone(r.path));
            }
        }
        incoming.extend(entries.into_iter().map(Incoming::Put));
    } else {
        let items: Vec<ManifestDelta> = delta.try_collect().await?;
        for d in items {
            incoming.push(match d {
                ManifestDelta::Put(e) => Incoming::Put(e),
                ManifestDelta::Del { path } => Incoming::Del(path),
            });
        }
    }
    for i in &incoming {
        if i.path()
            .segments()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
        {
            return Err(CliError::failed(
                "unsafe_path",
                format!("refusing to write {}: it is inside .lode", i.path()),
            ));
        }
    }

    let report = local::scan(ctx, &mut ws, &[], true)?;
    if target < synced && resuming.is_none() {
        let touched: Vec<String> = incoming
            .iter()
            .filter_map(|i| report.get(i.path()))
            .filter(|e| !matches!(e.status, Status::Unchanged | Status::Missing))
            .map(|e| e.path.as_str().to_owned())
            .collect();
        if !touched.is_empty() {
            return Err(CliError::failed(
                "local_changes",
                format!(
                    "going back to r{target} would touch files with local changes: {}; commit or revert them first",
                    touched.join(", ")
                ),
            )
            .with_details(json!({ "paths": touched })));
        }
    }
    ws.db_mut()
        .write(|tx| tx.set_config(UPDATE_REV, &target.to_string()))?;

    let mut run = Run {
        ctx,
        bases: Bases::new(ctx, &ws)?,
        rev: target,
        config: local_config(&ws),
        outcomes: BTreeMap::new(),
    };
    let mut deletes: Vec<DepotPath> = Vec::new();
    let mut downloads: Vec<ManifestEntry> = Vec::new();
    let now = SystemTime::now();

    for i in &incoming {
        let entry = report.get(i.path());
        let row = entry.and_then(|e| e.row.clone());
        let status = entry.map(|e| e.status);
        match (i, row, entry) {
            (Incoming::Put(t), None, None) => {
                downloads.push(t.clone());
                run.note(&t.path, Outcome::Added);
            }
            // Something untracked where an incoming file goes.
            (Incoming::Put(t), None, Some(e)) => {
                if disk_id(&ws, e)? == Some(t.id)
                    && e.disk.as_ref().is_some_and(|d| d.kind == t.kind)
                {
                    let mut r = FileRow::synced(t.path.clone(), t.id, t.kind, t.executable);
                    let (size, mtime) = stat_for_row(&ws.local_path(&t.path), now)?;
                    (r.size, r.mtime_ns) = (size, mtime);
                    ws.db_mut().write(|tx| tx.put_file(&r))?;
                    run.note(&t.path, Outcome::Added);
                } else {
                    let r = FileRow::synced(t.path.clone(), t.id, t.kind, t.executable);
                    run.conflict(&mut ws, r, None, Some(t), None)?;
                }
            }
            (Incoming::Put(t), Some(r), Some(e)) => {
                if r.base_id == Some(t.id) && r.kind == t.kind {
                    if r.executable != t.executable {
                        let mut r2 = r.clone();
                        r2.executable = t.executable;
                        if status == Some(Status::Unchanged) {
                            crate::materialize::set_mode(&ws.local_path(&t.path), t.executable)?;
                            r2.mtime_ns = 0;
                        }
                        ws.db_mut().write(|tx| tx.put_file(&r2))?;
                        run.note(&t.path, Outcome::Updated);
                    }
                    continue;
                }
                match e.status {
                    Status::Unchanged | Status::Missing => {
                        downloads.push(t.clone());
                        run.note(&t.path, Outcome::Updated);
                    }
                    Status::Modified => run.merge(&mut ws, e, &r, t).await?,
                    Status::Added => {
                        if disk_id(&ws, e)? == Some(t.id) {
                            let mut r2 =
                                FileRow::synced(t.path.clone(), t.id, t.kind, t.executable);
                            r2.mtime_ns = 0;
                            ws.db_mut().write(|tx| tx.put_file(&r2))?;
                            run.note(&t.path, Outcome::Updated);
                        } else {
                            run.conflict(&mut ws, r, None, Some(t), None)?;
                        }
                    }
                    // Deleted here, changed there; or still in conflict from a newer change.
                    _ => {
                        let old = r.base_id;
                        run.conflict(&mut ws, r, old, Some(t), None)?;
                    }
                }
            }
            (Incoming::Put(_), Some(_), None) => {}
            (Incoming::Del(_) | Incoming::Gone(_), None, _) => {}
            (Incoming::Del(p) | Incoming::Gone(p), Some(r), _) => match status {
                Some(Status::Unchanged | Status::Missing | Status::Deleted) | None => {
                    deletes.push(p.clone());
                    run.note(p, Outcome::Deleted);
                }
                Some(Status::Added) => {}
                _ => {
                    if matches!(i, Incoming::Gone(_)) {
                        if let Some(c) = ws.db().conflicts()?.into_iter().find(|c| &c.path == p) {
                            local::clear_conflict(&mut ws, &c)?;
                        }
                        ws.db_mut().write(|tx| tx.remove_file(p))?;
                        run.note(p, Outcome::Kept);
                    } else {
                        let old = r.base_id;
                        run.conflict(&mut ws, r, old, None, None)?;
                    }
                }
            },
        }
    }

    let root = ws.root().to_path_buf();
    for p in &deletes {
        remove_and_prune(&root, p)?;
        ws.db_mut().write(|tx| tx.remove_file(p))?;
    }
    let fetched = fetch_entries(&mut ws, &client, &depot, downloads, &ctx.out).await?;
    ws.db_mut().write(|tx| {
        tx.set_config(keys::SYNCED_REV, &target.to_string())?;
        if let Some(e) = epoch {
            tx.set_config(keys::ACCESS_EPOCH, &e.to_string())?;
        }
        tx.remove_config(UPDATE_REV)
    })?;
    crate::cmd::locks::reconcile_after_update(ctx, &mut ws, &client).await?;
    crate::cmd::locks::protect_all(&ws)?;

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for o in run.outcomes.values() {
        *counts.entry(o.name()).or_default() += 1;
    }
    for (p, o) in &run.outcomes {
        if matches!(o, Outcome::Conflicted | Outcome::Kept | Outcome::Merged) {
            run.ctx.out.line(format!("{:<10} {p}", o.name()));
        }
    }
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    ctx.out.line(format!(
        "Updated to r{target}{}{}",
        if summary.is_empty() {
            ": nothing changed".to_owned()
        } else {
            format!(": {}", summary.join(", "))
        },
        if full { " (full sync)" } else { "" }
    ));
    if counts.contains_key("conflicted") {
        ctx.out
            .line("Resolve conflicts with `lode resolve <path> --mine|--theirs|--merged`.");
    }
    let files: Vec<Value> = run
        .outcomes
        .iter()
        .map(|(p, o)| json!({ "path": p, "outcome": o.name() }))
        .collect();
    Ok(json!({
        "rev": target,
        "from": synced,
        "full": full,
        "files": files,
        "counts": counts,
        "downloaded": fetched.downloaded,
    }))
}

/// Runs `lode resolve`.
pub async fn resolve(ctx: &Ctx, args: ResolveArgs) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
    let named = local::depot_paths(ctx, &ws, &args.paths)?;
    let conflicts: Vec<ConflictRow> = ws
        .db()
        .conflicts()?
        .into_iter()
        .filter(|c| named.is_empty() || named.iter().any(|n| local::under(&c.path, n)))
        .collect();
    let not_conflicted: Vec<String> = named
        .iter()
        .filter(|n| !conflicts.iter().any(|c| local::under(&c.path, n)))
        .map(|n| n.as_str().to_owned())
        .collect();
    if !not_conflicted.is_empty() {
        return Err(local::not_found("not in conflict", &not_conflicted));
    }
    let mut bases = Bases::new(ctx, &ws)?;
    let mut done = Vec::new();
    for c in &conflicts {
        let Some(row) = ws.db().file(&c.path)? else {
            local::clear_conflict(&mut ws, c)?;
            continue;
        };
        let local_path = ws.local_path(&c.path);
        if args.theirs {
            match row.base_id {
                Some(_) => bases.restore(&mut ws, &row).await?,
                None => {
                    remove_and_prune(ws.root(), &c.path)?;
                    ws.db_mut().write(|tx| tx.remove_file(&c.path))?;
                }
            }
        } else {
            if args.mine {
                if let Some(id) = c.mine_id {
                    let kept = local::mine_path(&ws, &id);
                    let bytes = fs::read(&kept).map_err(|e| {
                        CliError::failed(
                            "workspace",
                            format!(
                                "the local version of {} is gone ({e}); use --merged or --theirs",
                                c.path
                            ),
                        )
                    })?;
                    let exec = fs::symlink_metadata(&local_path)
                        .map(|m| is_exec(&m))
                        .unwrap_or(row.executable);
                    write_through_tmp(&ws, &c.path, &bytes, exec)?;
                }
            }
            let on_disk = fs::symlink_metadata(&local_path).is_ok();
            let mut r = row.clone();
            r.mtime_ns = 0;
            r.state = match (on_disk, row.base_id) {
                (true, Some(_)) => FileState::Normal,
                // Deleted on the server, kept here: it is an add again.
                (true, None) => FileState::Added,
                (false, Some(_)) => FileState::Deleted,
                (false, None) => {
                    ws.db_mut().write(|tx| tx.remove_file(&c.path))?;
                    local::clear_conflict(&mut ws, c)?;
                    done.push(c.path.as_str().to_owned());
                    continue;
                }
            };
            ws.db_mut().write(|tx| tx.put_file(&r))?;
        }
        local::clear_conflict(&mut ws, c)?;
        done.push(c.path.as_str().to_owned());
    }
    let how = if args.mine {
        "mine"
    } else if args.theirs {
        "theirs"
    } else {
        "merged"
    };
    for p in &done {
        ctx.out.line(format!("Resolved {p} ({how})"));
    }
    Ok(json!({ "resolved": done, "using": how }))
}

#[cfg(unix)]
fn is_exec(m: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_exec(_m: &fs::Metadata) -> bool {
    false
}
