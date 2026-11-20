//! `lode shelve`, `lode shelves`, `lode shelf show|diff|delete` and `lode unshelve`
//! (SPEC §6.2, ARCHITECTURE §9.2).
//!
//! `shelve` sends pending changes like a commit does, but to a shelf on the server: the content
//! is uploaded, the changes are checked against the branch head, and the workspace is reverted
//! unless `--keep`. `unshelve` applies a shelf's changes on top of the workspace with the merge
//! and conflict handling of `update`, the shelf's base content being the common ancestor.

use std::path::PathBuf;

use lode_client::Client;
use lode_core::{validate_changes, Change, ChangeAction};
use lode_proto::shelves::{
    CreateShelfRequest, ShelfState, ShelfView, ShelvesQuery, SubmitShelfRequest, UpdateShelfRequest,
};
use lode_workspace::db::keys;
use lode_workspace::Workspace;
use serde_json::{json, Value};

use crate::cmd::apply::{apply_changes, count, file_list, print_outcomes, refuse_conflicts};
use crate::cmd::commit::{explain, gather, upload, Outgoing, PENDING_COMMIT};
use crate::cmd::history::{format_time, who};
use crate::cmd::merge::{describe, pending_merges, set_pending_merges};
use crate::cmd::update::UPDATE_REV;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, Bases, FROM_SHELF};
use crate::materialize::remove_and_prune;

/// `lode shelve` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Put pending changes on a shelf on the server.
\nThe content is uploaded and the changes are checked against the branch head, like a commit,
but no revision is made. The shelved files are then reverted in the workspace unless --keep.
--update ID replaces the content of one of your open shelves and raises its version.
\nWith --json: {\"shelf\":id,\"version\":n,\"files\":n,\"uploaded\":n,\"bytes_uploaded\":n,\"bytes_reused\":n,\"kept\":bool,\"updated\":bool}.")]
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
    /// Do not run the depot's validation scripts first (the server checks shelves again).
    #[arg(long)]
    pub no_verify: bool,
}

/// `lode shelves` options.
#[derive(Debug, Default, clap::Args)]
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
    /// Commit a shelf on the server: you are the submitter, its owner the author.
    Submit {
        /// `ID@VERSION`, the version you reviewed. Without a version, the shelf is shown and
        /// confirmed first (in a terminal only).
        spec: String,
        /// Commit message (default: the shelf's description).
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
        /// Skip the stream flow rules for the merge the shelf records (depot admins).
        #[arg(long)]
        force_flow: bool,
    },
}

/// `lode shelf` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Look at or delete a shelf.
\nWith --json: show prints the shelf with \"changes\"; diff adds \"diff\":[{\"path\",\"patch\"|\"skipped\"}];
delete prints {\"deleted\":id}; submit prints {\"rev\",\"branch\",\"shelf\",\"version\",\"replayed\":bool}.")]
pub struct ShelfArgs {
    #[command(subcommand)]
    pub command: ShelfCommand,
}

/// `lode unshelve` options.
#[derive(Debug, Default, clap::Args)]
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

fn open(ctx: &Ctx) -> OpResult<Target> {
    let ws = local::open(ctx)?;
    let config = |k: &str| -> OpResult<String> {
        ws.db()
            .config(k)?
            .ok_or_else(|| OpError::failed("workspace", format!("workspace has no {k}")))
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
async fn refused(client: &Client, e: lode_client::Error) -> OpError {
    if e.is_code(lode_core::ErrorCode::NotFound) && e.api().is_some_and(|a| a.paths().is_empty()) {
        if let Ok(info) = client.info().await {
            if !info.modules.iter().any(|m| m == "shelves") {
                return OpError::failed(
                    "module_disabled",
                    "this server does not have the shelves module enabled",
                );
            }
        }
    }
    explain(e)
}

fn refuse_unfinished(ws: &Workspace) -> OpResult<()> {
    if ws.db().config(PENDING_COMMIT)?.is_some() {
        return Err(OpError::failed(
            "pending_commit",
            "an earlier commit did not get an answer; run `lode commit` first",
        ));
    }
    if ws.db().config(UPDATE_REV)?.is_some() {
        return Err(OpError::failed(
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

fn to_json(v: &impl serde::Serialize) -> OpResult<Value> {
    serde_json::to_value(v).map_err(|e| OpError::failed("internal", e.to_string()))
}

/// Runs `lode shelve`.
pub async fn shelve(ctx: &Ctx, args: ShelveArgs) -> OpResult<Value> {
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
        .ok_or_else(|| OpError::usage("a shelf needs a description: pass -m"))?;

    let merges = pending_merges(&t.ws)?;
    if !merges.is_empty() && (!paths.is_empty() || list_id.is_some()) {
        return Err(OpError::usage(format!(
            "the merge of {} must be shelved whole: shelve without paths or -c",
            describe(&merges[0])
        )));
    }
    let (out, _) = gather(ctx, &mut t.ws, &paths, list_id)?;
    if out.is_empty() {
        let msg = match &args.changelist {
            Some(n) => format!("nothing to shelve in changelist {n}"),
            None => "nothing to shelve".to_owned(),
        };
        return Err(OpError::failed("nothing_to_shelve", msg));
    }
    let changes: Vec<Change> = out.iter().map(|o| o.change.clone()).collect();
    validate_changes(&changes).map_err(|e| OpError::failed("invalid_change", e.to_string()))?;
    crate::cmd::validate::before_send(
        ctx,
        &t.ws,
        crate::cmd::validate::Phase::Shelve,
        &changes,
        args.no_verify,
    )
    .await?;
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
                    merged_from: merges.clone(),
                    provenance: ctx.g.provenance(),
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
                        merged_from: merges.clone(),
                        provenance: ctx.g.provenance(),
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
        set_pending_merges(&mut t.ws, &[])?;
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
    // A link the person (or an agent) can paste; absent for a server under a path.
    let link = ctx
        .server_url(Some(&t.ws))
        .ok()
        .and_then(|server| crate::cmd::link::shelf_link(&server, &t.depot, view.id));
    Ok(json!({
        "shelf": view.id,
        "link": link,
        "version": view.version,
        "files": n,
        "uploaded": uploaded.blobs,
        "bytes_uploaded": uploaded.transfer.network,
        "bytes_reused": uploaded.transfer.reused,
        "kept": args.keep,
        "updated": args.update.is_some(),
        "merged_from": merges,
    }))
}

/// Takes the shelved changes out of the workspace: adds are removed from disk, the rest
/// restored to their base.
async fn discard(ctx: &Ctx, ws: &mut Workspace, out: &[Outgoing]) -> OpResult<()> {
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
pub async fn shelves(ctx: &Ctx, args: ShelvesArgs) -> OpResult<Value> {
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
pub async fn shelf(ctx: &Ctx, args: ShelfArgs) -> OpResult<Value> {
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
                    (Some(p), _) => ctx.out.raw(p.as_bytes())?,
                    (None, Some(why)) => ctx.out.line(format!("# {}: {why}, not shown", d.path)),
                    (None, None) => {}
                }
            }
            to_json(&v)
        }
        ShelfCommand::Submit {
            spec,
            message,
            force_flow,
        } => submit(ctx, &t, &spec, message, force_flow).await,
        ShelfCommand::Delete { id } => {
            if let Err(e) = t.client.delete_shelf(&t.depot, id).await {
                return Err(refused(&t.client, e).await);
            }
            ctx.out.line(format!("Deleted shelf {id}."));
            Ok(json!({ "deleted": id }))
        }
    }
}

/// `ID` or `ID@VERSION`.
fn parse_spec(spec: &str) -> OpResult<(u64, Option<u64>)> {
    let bad = || OpError::usage(format!("`{spec}` is not a shelf: use ID or ID@VERSION"));
    let (id, version) = match spec.split_once('@') {
        Some((i, v)) => (i, Some(v.parse::<u64>().map_err(|_| bad())?)),
        None => (spec, None),
    };
    Ok((id.parse::<u64>().map_err(|_| bad())?, version))
}

/// The unmet checks of a `checks_required` refusal, one per line.
fn unmet_checks(details: &Value) -> Option<String> {
    let missing = details.get("missing")?.as_array()?;
    let mut text = format!(
        "shelf {} version {} cannot be submitted until these checks pass:",
        details
            .get("shelf_id")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        details
            .get("shelf_version")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    );
    for m in missing {
        let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
        let reporter = m.get("reporter").and_then(Value::as_str).unwrap_or("?");
        let state = m.get("state").and_then(Value::as_str).unwrap_or("?");
        let why = match m.get("reason").and_then(Value::as_str) {
            Some("absent") => format!("not reported yet (expected from {reporter})"),
            Some("wrong_reporter") => format!(
                "{state}, but reported by {}, not {reporter}",
                m.get("reported_by").and_then(Value::as_str).unwrap_or("?")
            ),
            _ => state.to_owned(),
        };
        text.push_str(&format!("\n  {name}: {why}"));
    }
    Some(text)
}

/// What to do about a refused submit.
fn submit_refused(e: lode_client::Error) -> OpError {
    let version = e
        .api()
        .and_then(|a| a.details.get("version"))
        .and_then(Value::as_u64);
    let mut err = explain(e);
    match (err.code.as_str(), version) {
        ("conflict", Some(v)) => {
            err.message = format!(
                "the shelf was changed since that version; it is at version {v} now: review it again"
            );
        }
        ("checks_required", _) => {
            if let Some(text) = unmet_checks(&err.details) {
                err.message = text;
            }
        }
        ("out_of_date", _) => {
            err.message = format!(
                "{}; the branch moved under the shelf: unshelve it, update, and shelve it again with `lode shelve --update`",
                err.message.split("; ").next().unwrap_or_default()
            );
        }
        _ => {}
    }
    err
}

async fn submit(
    ctx: &Ctx,
    t: &Target,
    spec: &str,
    message: Option<String>,
    force_flow: bool,
) -> OpResult<Value> {
    let (id, version) = parse_spec(spec)?;
    let version = match version {
        Some(v) => v,
        None => {
            let interactive = !ctx.g.json && ctx.out.ui().interactive();
            if !interactive {
                return Err(OpError::usage(
                    "name the version you reviewed: `lode shelf submit ID@VERSION`",
                ));
            }
            let v = get(t, id, false).await?;
            print_shelf(ctx, &v);
            let question = format!("Submit version {} of shelf {id}?", v.version);
            if !ctx.out.ui().confirm(&question)? {
                return Err(OpError::failed("aborted", "nothing was submitted"));
            }
            v.version
        }
    };
    let req = SubmitShelfRequest {
        expected_version: version,
        message: message.filter(|m| !m.trim().is_empty()),
        force_flow,
    };
    let resp = match t.client.submit_shelf(&t.depot, id, &req).await {
        Ok(r) => r,
        Err(e) if e.is_code(lode_core::ErrorCode::NotFound) => {
            return Err(refused(&t.client, e).await)
        }
        Err(e) => return Err(submit_refused(e)),
    };
    ctx.out.line(format!(
        "{} shelf {id} version {version} as r{} on {}.",
        if resp.replayed {
            "Already submitted"
        } else {
            "Submitted"
        },
        resp.rev,
        resp.branch
    ));
    Ok(json!({
        "rev": resp.rev,
        "branch": resp.branch,
        "shelf": id,
        "version": version,
        "replayed": resp.replayed,
    }))
}

async fn get(t: &Target, id: u64, diff: bool) -> OpResult<ShelfView> {
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

/// Runs `lode unshelve`.
pub async fn unshelve(ctx: &Ctx, args: UnshelveArgs) -> OpResult<Value> {
    let mut t = open(ctx)?;
    let _lock = t.ws.lock()?;
    refuse_unfinished(&t.ws)?;
    refuse_conflicts(&t.ws)?;
    let shelf = get(&t, args.id, false).await?;
    if shelf.state == ShelfState::Deleted {
        return Err(OpError::failed(
            "not_found",
            format!("shelf {} was deleted", shelf.id),
        ));
    }
    if !shelf.merged_from.is_empty() {
        if let Some(m) = pending_merges(&t.ws)?.first() {
            return Err(OpError::failed(
                "merge_pending",
                format!(
                    "the shelf carries a merge, and the merge of {} is pending here; commit or abandon it first",
                    describe(m)
                ),
            ));
        }
    }
    let changes = shelf.changes.clone().unwrap_or_default();
    let outcomes = apply_changes(
        ctx,
        &mut t.ws,
        &changes,
        format!("shelf {}", shelf.id),
        FROM_SHELF,
        &Default::default(),
    )
    .await?;
    if !shelf.merged_from.is_empty() {
        set_pending_merges(&mut t.ws, &shelf.merged_from)?;
    }
    let counts = count(&outcomes.outcomes);
    print_outcomes(ctx, &outcomes);
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
    for m in &shelf.merged_from {
        ctx.out.line(format!(
            "The shelf carries the merge of {}; the next commit records it.",
            describe(m)
        ));
    }
    if counts.contains_key("conflicted") || counts.contains_key("rename_conflict") {
        ctx.out
            .line("Resolve conflicts with `lode resolve <path> --mine|--theirs|--merged`.");
    }
    let config = crate::cmd::update::local_config(&t.ws);
    let files: Vec<Value> = file_list(&outcomes, config.as_ref());
    if files.iter().any(|f| f.get("tool").is_some()) {
        ctx.out
            .line("Some conflicts have a merge tool: `lode resolve --tool` starts it.");
    }
    Ok(json!({
        "shelf": shelf.id,
        "version": shelf.version,
        "files": files,
        "counts": counts,
        "merged_from": shelf.merged_from,
    }))
}
