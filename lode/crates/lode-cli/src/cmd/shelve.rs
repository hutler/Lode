//! `lode shelve`, `lode shelves`, `lode shelf show|diff|delete` and `lode unshelve`
//! (SPEC §6.2, ARCHITECTURE §9.2).
//!
//! `shelve` sends pending changes like a commit does, but to a shelf on the server: the content
//! is uploaded, the changes are checked against the branch head, and the workspace is reverted
//! unless `--keep`. `unshelve` applies a shelf's changes on top of the workspace with the merge
//! and conflict handling of `update`, the shelf's base content being the common ancestor.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use lode_client::Client;
use lode_core::{
    validate_changes, Change, ChangeAction, DepotPath, EntryKind, LodeConfig, ObjectId,
};
use lode_proto::shelves::{
    CreateShelfRequest, ShelfState, ShelfView, ShelvesQuery, UpdateShelfRequest,
};
use lode_workspace::classify::{class_by_config, class_by_content};
use lode_workspace::db::keys;
use lode_workspace::{ConflictRow, FileClass, FileRow, FileState, ScanEntry, Workspace};
use serde_json::{json, Value};

use crate::cmd::commit::{explain, gather, upload, Outgoing, PENDING_COMMIT};
use crate::cmd::diff::TEXT_LIMIT;
use crate::cmd::history::{format_time, who};
use crate::cmd::update::{disk_id, local_config, relabel, write_through_tmp, UPDATE_REV};
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local::{self, Bases, FROM_SHELF};
use crate::materialize::remove_and_prune;

/// `lode shelve` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Put pending changes on a shelf on the server.
\nThe content is uploaded and the changes are checked against the branch head, like a commit,
but no revision is made. The shelved files are then reverted in the workspace unless --keep.
--update ID replaces the content of one of your open shelves and raises its version.
\nWith --json: {\"shelf\":id,\"version\":n,\"files\":n,\"uploaded\":n,\"kept\":bool,\"updated\":bool}.")]
pub struct ShelveArgs {
    /// Only changes to these files and directories.
    pub paths: Vec<PathBuf>,
    /// Description (default: the changelist's, or the shelf's when updating).
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,
    /// Only the files of this changelist.
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
    /// Leave the changes in the workspace.
    #[arg(long)]
    pub keep: bool,
    /// Replace the content of this shelf.
    #[arg(long = "update", value_name = "ID")]
    pub update: Option<u64>,
}

/// `lode shelves` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "List the open shelves of this depot.
\nWith --json: {\"shelves\":[{\"id\",\"version\",\"branch\",\"base_rev\",\"owner\":{\"name\",\"kind\"},
\"description\",\"state\",\"created_at\",\"updated_at\",...}]}, newest first.")]
pub struct ShelvesArgs {
    /// Only your shelves.
    #[arg(long, conflicts_with = "user")]
    pub mine: bool,
    /// Only the shelves of this user.
    #[arg(long, value_name = "USER")]
    pub user: Option<String>,
    /// Only shelves of this branch.
    #[arg(long)]
    pub branch: Option<String>,
    /// Submitted or deleted shelves instead of open ones.
    #[arg(long, value_parser = ["open", "submitted", "deleted"])]
    pub state: Option<String>,
}

/// `lode shelf` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum ShelfCommand {
    /// Show a shelf and its changed files.
    Show {
        /// Shelf id.
        id: u64,
    },
    /// Show a shelf's changes as a unified diff.
    Diff {
        /// Shelf id.
        id: u64,
    },
    /// Delete one of your shelves (depot admins may delete any).
    Delete {
        /// Shelf id.
        id: u64,
    },
}

/// `lode shelf` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Look at or delete a shelf.
\nWith --json: show prints the shelf with \"changes\"; diff adds \"diff\":[{\"path\",\"patch\"|\"skipped\"}];
delete prints {\"deleted\":id}.")]
pub struct ShelfArgs {
    #[command(subcommand)]
    pub command: ShelfCommand,
}

/// `lode unshelve` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Apply a shelf's changes to the workspace.
\nWorks for anyone's shelf. A file changed here too is merged as `update` would merge it, with
the shelf's base as the common ancestor; what cannot be merged is left in conflict (resolve it
with `lode resolve`). The workspace's own base revision does not change.
\nWith --json: {\"shelf\":id,\"version\":n,\"files\":[{\"path\",\"outcome\"}],\"counts\":{outcome:n}}.
Outcomes: applied, merged, conflicted, unchanged (already as on the shelf), skipped (outside the view).")]
pub struct UnshelveArgs {
    /// Shelf id.
    pub id: u64,
}

/// The workspace with its server, depot and branch.
struct Target {
    ws: Workspace,
    client: Client,
    depot: String,
    branch: String,
}

fn open(ctx: &Ctx) -> CliResult<Target> {
    let ws = local::open(ctx)?;
    let config = |k: &str| -> CliResult<String> {
        ws.db()
            .config(k)?
            .ok_or_else(|| CliError::failed("workspace", format!("workspace has no {k}")))
    };
    let depot = config(keys::DEPOT)?;
    let branch = config(keys::BRANCH)?;
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;
    Ok(Target {
        ws,
        client,
        depot,
        branch,
    })
}

/// A shelf call's error, saying so when the server does not run the module.
async fn refused(client: &Client, e: lode_client::Error) -> CliError {
    if e.is_code(lode_core::ErrorCode::NotFound) && e.api().is_some_and(|a| a.paths().is_empty()) {
        if let Ok(info) = client.info().await {
            if !info.modules.iter().any(|m| m == "shelves") {
                return CliError::failed(
                    "module_disabled",
                    "this server does not have the shelves module enabled",
                );
            }
        }
    }
    explain(e)
}

fn refuse_unfinished(ws: &Workspace) -> CliResult<()> {
    if ws.db().config(PENDING_COMMIT)?.is_some() {
        return Err(CliError::failed(
            "pending_commit",
            "an earlier commit did not get an answer; run `lode commit` first",
        ));
    }
    if ws.db().config(UPDATE_REV)?.is_some() {
        return Err(CliError::failed(
            "update_incomplete",
            "an update did not finish; run `lode update` first",
        ));
    }
    Ok(())
}

fn letter(a: ChangeAction) -> char {
    match a {
        ChangeAction::Add => 'A',
        ChangeAction::Modify => 'M',
        ChangeAction::Delete => 'D',
    }
}

fn state_name(s: ShelfState) -> &'static str {
    match s {
        ShelfState::Open => "open",
        ShelfState::Submitted => "submitted",
        ShelfState::Deleted => "deleted",
    }
}

fn to_json(v: &impl serde::Serialize) -> CliResult<Value> {
    serde_json::to_value(v).map_err(|e| CliError::failed("internal", e.to_string()))
}

/// Runs `lode shelve`.
pub async fn shelve(ctx: &Ctx, args: ShelveArgs) -> CliResult<Value> {
    let mut t = open(ctx)?;
    let _lock = t.ws.lock()?;
    refuse_unfinished(&t.ws)?;
    let paths = local::depot_paths(ctx, &t.ws, &args.paths)?;
    let list = args
        .changelist
        .as_deref()
        .map(|n| local::changelist(&t.ws, n))
        .transpose()?;
    let list_id = list.as_ref().map(|l| l.as_ref().map(|c| c.id));
    let existing = match args.update {
        Some(id) => Some(t.client.shelf(&t.depot, id, false).await.map_err(explain)?),
        None => None,
    };
    let description = args
        .message
        .clone()
        .or_else(|| list.flatten().map(|c| c.description))
        .or_else(|| existing.as_ref().map(|s| s.description.clone()))
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
        .ok_or_else(|| CliError::usage("a shelf needs a description: pass -m"))?;

    let (out, _) = gather(ctx, &mut t.ws, &paths, list_id)?;
    if out.is_empty() {
        let msg = match &args.changelist {
            Some(n) => format!("nothing to shelve in changelist {n}"),
            None => "nothing to shelve".to_owned(),
        };
        return Err(CliError::failed("nothing_to_shelve", msg));
    }
    let changes: Vec<Change> = out.iter().map(|o| o.change.clone()).collect();
    validate_changes(&changes).map_err(|e| CliError::failed("invalid_change", e.to_string()))?;
    let uploaded = upload(ctx, &t.client, &t.depot, &out).await?;
    // Checked against the head: the touched files must be up to date, as for a commit.
    let head = t.client.branch(&t.depot, &t.branch).await?.head_rev;
    let view = match args.update {
        Some(id) => t
            .client
            .update_shelf(
                &t.depot,
                id,
                &UpdateShelfRequest {
                    base_rev: head,
                    description,
                    changes,
                    merged_from: vec![],
                    provenance: None,
                },
            )
            .await
            .map_err(explain)?,
        None => {
            let r = t
                .client
                .create_shelf(
                    &t.depot,
                    &CreateShelfRequest {
                        branch: t.branch.clone(),
                        base_rev: head,
                        description,
                        changes,
                        merged_from: vec![],
                        provenance: None,
                    },
                )
                .await;
            match r {
                Ok(v) => v,
                Err(e) => return Err(refused(&t.client, e).await),
            }
        }
    };
    if !args.keep {
        discard(ctx, &mut t.ws, &out).await?;
    }
    let n = out.len();
    ctx.out.line(format!(
        "{} shelf {} (version {}): {n} file{}{}.",
        if args.update.is_some() {
            "Updated"
        } else {
            "Created"
        },
        view.id,
        view.version,
        if n == 1 { "" } else { "s" },
        if args.keep {
            ", kept in the workspace"
        } else {
            ", reverted in the workspace"
        }
    ));
    Ok(json!({
        "shelf": view.id,
        "version": view.version,
        "files": n,
        "uploaded": uploaded,
        "kept": args.keep,
        "updated": args.update.is_some(),
    }))
}

/// Takes the shelved changes out of the workspace: adds are removed from disk, the rest
/// restored to their base.
async fn discard(ctx: &Ctx, ws: &mut Workspace, out: &[Outgoing]) -> CliResult<()> {
    let mut bases = Bases::new(ctx, ws)?;
    let root = ws.root().to_path_buf();
    let mut paths = Vec::with_capacity(out.len());
    for o in out {
        let path = &o.change.path;
        match o.change.action {
            ChangeAction::Add => {
                remove_and_prune(&root, path)?;
                ws.db_mut().write(|tx| tx.remove_file(path))?;
            }
            ChangeAction::Modify | ChangeAction::Delete => {
                if let Some(row) = ws.db().file(path)? {
                    bases.restore(ws, &row).await?;
                }
            }
        }
        paths.push(path.clone());
    }
    crate::cmd::locks::protect(ws, &paths)
}

/// Runs `lode shelves`.
pub async fn shelves(ctx: &Ctx, args: ShelvesArgs) -> CliResult<Value> {
    let t = open(ctx)?;
    let owner = if args.mine {
        Some(t.client.whoami().await?.principal.name)
    } else {
        args.user.clone()
    };
    let state = match args.state.as_deref() {
        Some("submitted") => Some(ShelfState::Submitted),
        Some("deleted") => Some(ShelfState::Deleted),
        Some(_) => Some(ShelfState::Open),
        None => None,
    };
    let q = ShelvesQuery {
        owner,
        branch: args.branch.clone(),
        state,
    };
    let list = match t.client.shelves(&t.depot, &q).await {
        Ok(l) => l,
        Err(e) => return Err(refused(&t.client, e).await),
    };
    for s in &list.shelves {
        ctx.out.line(format!(
            "{}  v{}  {}  {}  {}",
            s.id,
            s.version,
            who(&s.owner.name, s.owner.kind),
            s.branch,
            s.description.lines().next().unwrap_or_default()
        ));
    }
    if list.shelves.is_empty() {
        ctx.out.line("No shelves.");
    }
    to_json(&list)
}

/// Runs `lode shelf`.
pub async fn shelf(ctx: &Ctx, args: ShelfArgs) -> CliResult<Value> {
    let t = open(ctx)?;
    match args.command {
        ShelfCommand::Show { id } => {
            let v = get(&t, id, false).await?;
            print_shelf(ctx, &v);
            to_json(&v)
        }
        ShelfCommand::Diff { id } => {
            let v = get(&t, id, true).await?;
            ctx.out
                .line(format!("# shelf {} version {}", v.id, v.version));
            for d in v.diff.iter().flatten() {
                match (&d.patch, &d.skipped) {
                    (Some(p), _) => {
                        if !ctx.g.json {
                            print!("{p}");
                        }
                    }
                    (None, Some(why)) => ctx.out.line(format!("# {}: {why}, not shown", d.path)),
                    (None, None) => {}
                }
            }
            to_json(&v)
        }
        ShelfCommand::Delete { id } => {
            if let Err(e) = t.client.delete_shelf(&t.depot, id).await {
                return Err(refused(&t.client, e).await);
            }
            ctx.out.line(format!("Deleted shelf {id}."));
            Ok(json!({ "deleted": id }))
        }
    }
}

async fn get(t: &Target, id: u64, diff: bool) -> CliResult<ShelfView> {
    match t.client.shelf(&t.depot, id, diff).await {
        Ok(v) => Ok(v),
        Err(e) => Err(refused(&t.client, e).await),
    }
}

/// The human form of `shelf show`.
pub(crate) fn print_shelf(ctx: &Ctx, v: &ShelfView) {
    ctx.out.line(format!(
        "Shelf {}  version {}  {}",
        v.id,
        v.version,
        state_name(v.state)
    ));
    let mut owner = who(&v.owner.name, v.owner.kind);
    if let Some(a) = &v.agent_owner {
        owner.push_str(&format!(" (for {a})"));
    }
    ctx.out.line(format!("Owner:   {owner}"));
    ctx.out
        .line(format!("Branch:  {} at r{}", v.branch, v.base_rev));
    ctx.out.line(format!(
        "Updated: {}{}",
        format_time(v.updated_at),
        v.submitted_rev
            .map(|r| format!("  (submitted as r{r})"))
            .unwrap_or_default()
    ));
    ctx.out.line("");
    for l in v.description.lines() {
        ctx.out.line(format!("    {l}"));
    }
    ctx.out.line("");
    for c in v.changes.iter().flatten() {
        let from = c
            .copy_from
            .as_ref()
            .map(|cf| format!("  (from {}@{})", cf.path, cf.rev))
            .unwrap_or_default();
        ctx.out
            .line(format!("{}  {}{from}", letter(c.action), c.path));
    }
}

/// What `unshelve` did to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Applied,
    Merged,
    Conflicted,
    Unchanged,
    Skipped,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Merged => "merged",
            Outcome::Conflicted => "conflicted",
            Outcome::Unchanged => "unchanged",
            Outcome::Skipped => "skipped",
        }
    }
}

struct Apply<'a> {
    bases: Bases<'a>,
    config: Option<LodeConfig>,
    shelf: u64,
}

impl Apply<'_> {
    fn is_text(&self, path: &DepotPath, sides: &[&[u8]]) -> bool {
        match class_by_config(path, self.config.as_ref()) {
            Some(c) => c == FileClass::Text,
            None => sides
                .iter()
                .all(|s| s.len() as u64 <= TEXT_LIMIT && class_by_content(s) == FileClass::Text),
        }
    }

    /// A row for `c.path` now that the shelf's content is on disk: the row it had (deleted
    /// no more), or an add.
    fn put_row(ws: &mut Workspace, c: &Change, row: Option<FileRow>) -> CliResult<()> {
        let r = match row {
            Some(mut r) => {
                if r.state == FileState::Deleted {
                    r.state = FileState::Normal;
                }
                r.mtime_ns = 0;
                r
            }
            None => {
                let mut r = crate::cmd::files::added_row(&ws.local_path(&c.path), c.path.clone())?;
                r.copy_from = c.copy_from.clone();
                r.mtime_ns = 0;
                r
            }
        };
        ws.db_mut().write(|tx| tx.put_file(&r))?;
        Ok(())
    }

    /// Records a conflict: the row keeps the workspace's base, the conflict holds the shelf's
    /// side.
    fn conflict(
        &self,
        ws: &mut Workspace,
        c: &Change,
        row: Option<FileRow>,
        mine_id: Option<ObjectId>,
    ) -> CliResult<Outcome> {
        let mut r = row.unwrap_or(FileRow {
            path: c.path.clone(),
            base_id: None,
            kind: c.kind.unwrap_or(EntryKind::File),
            executable: c.executable,
            size: 0,
            mtime_ns: 0,
            state: FileState::Added,
            copy_from: c.copy_from.clone(),
            changelist_id: None,
            lock_held: false,
        });
        r.state = FileState::Conflicted;
        r.mtime_ns = 0;
        let conflict = ConflictRow {
            path: c.path.clone(),
            base_id: c.base_id,
            theirs_id: c.new_id,
            theirs_rev: FROM_SHELF,
            mine_id,
        };
        ws.db_mut().write(|tx| {
            tx.put_file(&r)?;
            tx.put_conflict(&conflict)
        })?;
        Ok(Outcome::Conflicted)
    }

    /// An add or modify: the shelf has `new_id` at the path.
    async fn put(
        &mut self,
        ws: &mut Workspace,
        c: &Change,
        e: Option<&ScanEntry>,
    ) -> CliResult<Outcome> {
        let Some(theirs) = c.new_id else {
            return Ok(Outcome::Unchanged);
        };
        let kind = c.kind.unwrap_or(EntryKind::File);
        let row = e.and_then(|e| e.row.clone());
        let mine = match e {
            Some(e) => disk_id(ws, e)?,
            None => None,
        };
        let root = ws.root().to_path_buf();
        if mine == Some(theirs) {
            if row.as_ref().map_or(true, |r| r.state == FileState::Deleted) {
                Self::put_row(ws, c, row)?;
            }
            return Ok(Outcome::Unchanged);
        }
        // Nothing changed here since the shelf's base (or nothing is here at all).
        if mine == c.base_id {
            self.bases
                .place(&root, &c.path, &theirs, kind, c.executable)
                .await?;
            Self::put_row(ws, c, row)?;
            return Ok(Outcome::Applied);
        }
        let Some(disk) = e.and_then(|e| e.disk.as_ref()) else {
            // Deleted here, changed on the shelf.
            return self.conflict(ws, c, row, None);
        };
        let local = ws.local_path(&c.path);
        let small = disk.size <= TEXT_LIMIT && c.size.unwrap_or(0) <= TEXT_LIMIT;
        if kind == EntryKind::File && disk.kind == EntryKind::File && small {
            let base = match c.base_id {
                Some(b) => self.bases.read(&b).await?,
                None => Vec::new(),
            };
            let their_bytes = self.bases.read(&theirs).await?;
            let mine_bytes = fs::read(&local)?;
            if self.is_text(&c.path, &[&base, &mine_bytes, &their_bytes]) {
                let mut opts = diffy::MergeOptions::new();
                opts.set_conflict_style(diffy::ConflictStyle::Merge)
                    .set_incomplete_hunk_style(diffy::IncompleteHunkStyle::Git);
                return match opts.merge_bytes(&base, &mine_bytes, &their_bytes) {
                    Ok(merged) => {
                        write_through_tmp(ws, &c.path, &merged, c.executable)?;
                        Self::put_row(ws, c, row)?;
                        Ok(Outcome::Merged)
                    }
                    Err(conflicted) => {
                        let mine_id = lode_core::blob_id(&mine_bytes[..])?;
                        let keep = local::mine_path(ws, &mine_id);
                        fs::create_dir_all(ws.layout().conflicts())?;
                        if !keep.exists() {
                            fs::write(&keep, &mine_bytes)?;
                        }
                        let label = format!("shelf {}", self.shelf);
                        write_through_tmp(
                            ws,
                            &c.path,
                            &relabel(conflicted, &label),
                            disk.executable,
                        )?;
                        self.conflict(ws, c, row, Some(mine_id))
                    }
                };
            }
        }
        // Binary or a symlink: keep the local file, put the shelf's beside it.
        if kind == EntryKind::File {
            let content = self.bases.read(&theirs).await?;
            fs::write(local::theirs_path(&local, FROM_SHELF), content)?;
        }
        self.conflict(ws, c, row, None)
    }

    /// A delete: the shelf removes the path.
    fn delete(
        &mut self,
        ws: &mut Workspace,
        c: &Change,
        e: Option<&ScanEntry>,
    ) -> CliResult<Outcome> {
        let Some(row) = e.and_then(|e| e.row.clone()) else {
            return Ok(Outcome::Unchanged);
        };
        let mine = match e {
            Some(e) => disk_id(ws, e)?,
            None => None,
        };
        let gone = |ws: &mut Workspace, row: FileRow| -> CliResult<()> {
            if row.base_id.is_none() {
                ws.db_mut().write(|tx| tx.remove_file(&row.path))?;
            } else {
                let mut r = row;
                r.state = FileState::Deleted;
                r.mtime_ns = 0;
                ws.db_mut().write(|tx| tx.put_file(&r))?;
            }
            Ok(())
        };
        if mine.is_none() {
            if row.state == FileState::Deleted {
                return Ok(Outcome::Unchanged);
            }
            gone(ws, row)?;
            return Ok(Outcome::Applied);
        }
        if mine == c.base_id {
            remove_and_prune(ws.root(), &c.path)?;
            gone(ws, row)?;
            return Ok(Outcome::Applied);
        }
        // Changed here, deleted on the shelf: the local file stays.
        self.conflict(ws, c, Some(row), None)
    }
}

/// Runs `lode unshelve`.
pub async fn unshelve(ctx: &Ctx, args: UnshelveArgs) -> CliResult<Value> {
    let mut t = open(ctx)?;
    let _lock = t.ws.lock()?;
    refuse_unfinished(&t.ws)?;
    let conflicts = t.ws.db().conflicts()?;
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
    let shelf = get(&t, args.id, false).await?;
    if shelf.state == ShelfState::Deleted {
        return Err(CliError::failed(
            "not_found",
            format!("shelf {} was deleted", shelf.id),
        ));
    }
    let changes = shelf.changes.clone().unwrap_or_default();
    let view = t.ws.view_spec()?.compile()?;
    let paths: Vec<DepotPath> = changes.iter().map(|c| c.path.clone()).collect();
    let report = local::scan(ctx, &mut t.ws, &paths, false)?;
    let mut run = Apply {
        bases: Bases::new(ctx, &t.ws)?,
        config: local_config(&t.ws),
        shelf: shelf.id,
    };
    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    for c in &changes {
        let outcome = if !view.contains(&c.path) {
            Outcome::Skipped
        } else {
            let e = report.get(&c.path);
            match c.action {
                ChangeAction::Delete => run.delete(&mut t.ws, c, e)?,
                ChangeAction::Add | ChangeAction::Modify => run.put(&mut t.ws, c, e).await?,
            }
        };
        outcomes.insert(c.path.as_str().to_owned(), outcome);
    }
    crate::cmd::locks::protect(&t.ws, &paths)?;

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for o in outcomes.values() {
        *counts.entry(o.name()).or_default() += 1;
    }
    for (p, o) in &outcomes {
        if *o != Outcome::Applied {
            ctx.out.line(format!("{:<10} {p}", o.name()));
        }
    }
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    ctx.out.line(format!(
        "Unshelved shelf {} (version {}): {}.",
        shelf.id,
        shelf.version,
        if summary.is_empty() {
            "nothing to apply".to_owned()
        } else {
            summary.join(", ")
        }
    ));
    if counts.contains_key("conflicted") {
        ctx.out
            .line("Resolve conflicts with `lode resolve <path> --mine|--theirs|--merged`.");
    }
    let files: Vec<Value> = outcomes
        .iter()
        .map(|(p, o)| json!({ "path": p, "outcome": o.name() }))
        .collect();
    Ok(json!({
        "shelf": shelf.id,
        "version": shelf.version,
        "files": files,
        "counts": counts,
    }))
}
