//! `lode view show|include|exclude`: change which paths are checked out.

use std::collections::HashSet;

use lode_core::DepotPath;
use lode_workspace::db::keys;
use lode_workspace::{ScanOptions, Status, ViewSpec, Workspace};
use serde_json::{json, Value};

use crate::cmd::checkout::{full_manifest, CHECKOUT_REV};
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::materialize::{fetch_entries, remove_and_prune};

/// `lode view` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum ViewCmd {
    /// Print the include and exclude patterns.
    Show,
    /// Add include patterns (or remove them with --remove) and sync.
    Include(PatternArgs),
    /// Add exclude patterns (or remove them with --remove) and sync.
    Exclude(PatternArgs),
}

/// Patterns to add or remove.
#[derive(Debug, Default, clap::Args)]
pub struct PatternArgs {
    /// Glob patterns, such as `art/**`.
    #[arg(required = true)]
    pub patterns: Vec<String>,
    /// Remove these patterns instead of adding them.
    #[arg(long)]
    pub remove: bool,
    /// Drop files with local changes too.
    #[arg(long)]
    pub force: bool,
}

fn edit(list: &mut Vec<String>, args: &PatternArgs) {
    for p in &args.patterns {
        if args.remove {
            list.retain(|x| x != p);
        } else if !list.contains(p) {
            list.push(p.clone());
        }
    }
}

fn view_json(v: &ViewSpec) -> Value {
    json!({ "include": v.include, "exclude": v.exclude })
}

/// Runs a `lode view` subcommand.
pub async fn view(ctx: &Ctx, cmd: ViewCmd) -> OpResult<Value> {
    let mut ws = ctx.workspace()?;
    let current = ws.view_spec()?;
    let (args, include) = match cmd {
        ViewCmd::Show => {
            if current.include.is_empty() && current.exclude.is_empty() {
                ctx.out.line("Everything is in the view.");
            }
            for p in &current.include {
                ctx.out.line(format!("include {p}"));
            }
            for p in &current.exclude {
                ctx.out.line(format!("exclude {p}"));
            }
            return Ok(json!({ "view": view_json(&current) }));
        }
        ViewCmd::Include(a) => (a, true),
        ViewCmd::Exclude(a) => (a, false),
    };
    let mut next = current.clone();
    edit(
        if include {
            &mut next.include
        } else {
            &mut next.exclude
        },
        &args,
    );
    next.compile()?;
    sync_view(ctx, &mut ws, &next, args.force).await
}

/// Brings the files on disk to `next` and stores it.
async fn sync_view(ctx: &Ctx, ws: &mut Workspace, next: &ViewSpec, force: bool) -> OpResult<Value> {
    let _lock = ws.lock()?;
    if ws.db().config(CHECKOUT_REV)?.is_some() {
        return Err(OpError::failed(
            "checkout_incomplete",
            "the checkout of this workspace did not finish; rerun `lode checkout` first",
        ));
    }
    let rev = ws.db().config_u64(keys::SYNCED_REV)?.ok_or_else(|| {
        OpError::failed(
            "checkout_incomplete",
            "this workspace has no synced revision",
        )
    })?;
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?;
    let server = ctx.server_url(Some(ws))?;
    let client = ctx.client(&server)?;
    let (entries, _) = full_manifest(&client, &depot, rev, next).await?;
    let wanted: HashSet<DepotPath> = entries.iter().map(|e| e.path.clone()).collect();

    // What leaves the view, and whether any of it has local changes.
    // Scanned without the view, so untracked files at paths entering it are seen too.
    let root = ws.root().to_path_buf();
    let report = lode_workspace::scan(
        &root,
        ws.db_mut(),
        &lode_workspace::View::default(),
        &ScanOptions {
            report_ignored: true,
            ..Default::default()
        },
    )?;
    let mut drop_paths = Vec::new();
    let mut blocked = Vec::new();
    for e in &report.entries {
        if e.row.is_none() || wanted.contains(&e.path) {
            continue;
        }
        if e.status != Status::Unchanged {
            blocked.push(e.path.as_str().to_owned());
        }
        drop_paths.push((e.path.clone(), e.status));
    }
    // Untracked files where incoming ones would go.
    let mut in_the_way = Vec::new();
    for e in &entries {
        if let Some(s) = report.get(&e.path) {
            if s.row.is_none() && matches!(s.status, Status::Untracked | Status::Ignored) {
                in_the_way.push(e.path.as_str().to_owned());
            }
        }
    }
    if !force && (!blocked.is_empty() || !in_the_way.is_empty()) {
        let mut msg = String::from("the view change would ");
        if !blocked.is_empty() {
            msg.push_str(&format!(
                "drop files with local changes ({})",
                blocked.join(", ")
            ));
        }
        if !in_the_way.is_empty() {
            if !blocked.is_empty() {
                msg.push_str(" and ");
            }
            msg.push_str(&format!(
                "overwrite untracked files ({})",
                in_the_way.join(", ")
            ));
        }
        msg.push_str("; commit or revert them, or pass --force");
        return Err(OpError::failed("local_changes", msg)
            .with_details(json!({ "paths": blocked, "untracked": in_the_way })));
    }

    let mut removed = 0u64;
    for (path, status) in &drop_paths {
        // An add was never on the server: forget it but leave the file.
        if *status != Status::Added {
            remove_and_prune(&root, path)?;
        }
        ws.db_mut().write(|tx| {
            tx.remove_conflict(path)?;
            tx.remove_file(path)
        })?;
        removed += 1;
    }
    let fetched = fetch_entries(ws, &client, &depot, entries, ctx).await?;
    ws.set_view_spec(next)?;
    ctx.out.line(format!(
        "View updated: {} files added, {removed} removed.",
        fetched.files
    ));
    Ok(json!({
        "view": view_json(next),
        "added": fetched.files,
        "removed": removed,
    }))
}
