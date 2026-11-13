//! `lode merge` (SPEC §5.4, ARCHITECTURE §10.5).
//!
//! The changes of another branch since the merge base are applied to the workspace the way
//! `unshelve` applies a shelf: what is unchanged here takes the source's side, the rest is
//! merged three-way with the base as common ancestor, or left in conflict. The merged range
//! is kept in workspace config until the next commit sends it as `merged_from` (or a shelve
//! carries it into a shelf).

use std::collections::HashMap;

use futures::TryStreamExt;
use lode_client::Client;
use lode_core::{Change, ChangeAction, DepotPath};
use lode_proto::commit::MergedFrom;
use lode_proto::read::{ManifestDelta, ManifestEntry};
use lode_workspace::db::keys;
use lode_workspace::Workspace;
use serde_json::{json, Value};

use crate::cmd::apply::{apply_changes, count, refuse_conflicts, Outcome};
use crate::cmd::commit::{explain, PENDING_COMMIT};
use crate::cmd::update::{check_on_branch, UPDATE_REV};
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local;

/// Config key: the merges the next commit records, as a JSON list of `MergedFrom`.
pub const PENDING_MERGE: &str = "pending_merge";

/// `lode merge` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Merge another branch into the workspace.
\nApplies the source branch's changes since the merge base: the highest source revision up to
which everything is already merged here, or the branch point. Files unchanged here take the
source's version; files changed on both sides are merged as `lode update` merges them, and
what cannot be merged is left in conflict (`lode resolve`). The merge is recorded by the next
`lode commit` (as `merged_from`); `lode revert --merge` abandons it.
\n-r N picks the single revision N of the source branch. Known limit: a later full merge
applies that change again (usually without effect, sometimes as a conflict).
\nWith --json: {\"source\",\"from_rev\",\"to_rev\",\"base\",\"nothing_to_do\":bool,
\"files\":[{\"path\",\"outcome\"}],\"counts\":{outcome:n}}. Outcomes: applied, merged,
conflicted, unchanged, skipped (outside the view).")]
pub struct MergeArgs {
    /// Branch to merge from.
    pub source: String,
    /// Pick only this revision of the source branch.
    #[arg(short = 'r', long = "rev")]
    pub rev: Option<u64>,
}

/// The merges waiting for the next commit.
pub(crate) fn pending_merges(ws: &Workspace) -> CliResult<Vec<MergedFrom>> {
    match ws.db().config(PENDING_MERGE)? {
        None => Ok(Vec::new()),
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| CliError::failed("workspace", format!("stored merge: {e}"))),
    }
}

/// Stores the merges waiting for the next commit (none removes the key).
pub(crate) fn set_pending_merges(ws: &mut Workspace, merges: &[MergedFrom]) -> CliResult<()> {
    if merges.is_empty() {
        ws.db_mut().write(|tx| tx.remove_config(PENDING_MERGE))?;
        return Ok(());
    }
    let text =
        serde_json::to_string(merges).map_err(|e| CliError::failed("internal", e.to_string()))?;
    ws.db_mut()
        .write(|tx| tx.set_config(PENDING_MERGE, &text))?;
    Ok(())
}

/// `branch r5..r9`, or `branch r7` for one revision.
pub(crate) fn describe(m: &MergedFrom) -> String {
    if m.from_rev == m.to_rev {
        format!("`{}` r{}", m.branch, m.to_rev)
    } else {
        format!("`{}` r{}..r{}", m.branch, m.from_rev, m.to_rev)
    }
}

/// The changes from `base` (nothing when `None`) to `head`, with each path's base side.
async fn changes_between(
    client: &Client,
    depot: &str,
    base: Option<u64>,
    head: u64,
    include: &[String],
    exclude: &[String],
) -> CliResult<Vec<Change>> {
    let Some(base) = base else {
        let entries: Vec<ManifestEntry> = client
            .manifest(depot, head, include, exclude)
            .await?
            .try_collect()
            .await?;
        return Ok(entries.into_iter().map(|e| put(e, None)).collect());
    };
    let forward: Vec<ManifestDelta> = client
        .manifest_delta(depot, base, head, include, exclude)
        .await?
        .try_collect()
        .await?;
    // The same delta the other way round has the base side of every changed path.
    let backward: Vec<ManifestDelta> = client
        .manifest_delta(depot, head, base, include, exclude)
        .await?
        .try_collect()
        .await?;
    let mut before: HashMap<DepotPath, ManifestEntry> = HashMap::new();
    for d in backward {
        if let ManifestDelta::Put(e) = d {
            before.insert(e.path.clone(), e);
        }
    }
    let mut out = Vec::with_capacity(forward.len());
    for d in forward {
        match d {
            ManifestDelta::Put(e) => {
                let old = before.get(&e.path).map(|b| b.id);
                out.push(put(e, old));
            }
            ManifestDelta::Del { path } => {
                let Some(old) = before.get(&path) else {
                    continue;
                };
                out.push(Change {
                    base_id: Some(old.id),
                    new_id: None,
                    kind: None,
                    executable: false,
                    size: None,
                    copy_from: None,
                    action: ChangeAction::Delete,
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn put(e: ManifestEntry, base_id: Option<lode_core::ObjectId>) -> Change {
    Change {
        action: if base_id.is_some() {
            ChangeAction::Modify
        } else {
            ChangeAction::Add
        },
        base_id,
        new_id: Some(e.id),
        kind: Some(e.kind),
        executable: e.executable,
        size: Some(e.size),
        copy_from: None,
        path: e.path,
    }
}

/// Runs `lode merge`.
pub async fn merge(ctx: &Ctx, args: MergeArgs) -> CliResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
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
    refuse_conflicts(&ws)?;
    if let Some(m) = pending_merges(&ws)?.first() {
        return Err(CliError::failed(
            "merge_pending",
            format!(
                "the merge of {} is not committed yet; commit it, or abandon it with `lode revert --merge`",
                describe(m)
            ),
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
    if args.source == branch {
        return Err(CliError::usage(format!(
            "the workspace is on `{branch}` already"
        )));
    }
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;

    let (base, head, range) = match args.rev {
        Some(n) => {
            check_on_branch(&client, &depot, &args.source, n).await?;
            let r = client.revision(&depot, n).await.map_err(explain)?;
            let range = MergedFrom {
                branch: args.source.clone(),
                from_rev: n,
                to_rev: n,
            };
            (r.parent, n, range)
        }
        None => {
            let info = client
                .merge_info(&depot, &branch, &args.source)
                .await
                .map_err(explain)?;
            if info.pending == 0 {
                ctx.out.line(format!(
                    "Nothing to merge: `{}` is merged up to r{}.",
                    args.source, info.base
                ));
                return Ok(json!({
                    "source": args.source,
                    "from_rev": null,
                    "to_rev": null,
                    "base": info.base,
                    "nothing_to_do": true,
                    "files": [],
                    "counts": {},
                }));
            }
            let range = MergedFrom {
                branch: args.source.clone(),
                from_rev: info.base + 1,
                to_rev: info.source_head,
            };
            (Some(info.base), info.source_head, range)
        }
    };

    let view = ws.view_spec()?;
    let changes =
        changes_between(&client, &depot, base, head, &view.include, &view.exclude).await?;
    for c in &changes {
        if c.path
            .segments()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
        {
            return Err(CliError::failed(
                "unsafe_path",
                format!("refusing to write {}: it is inside .lode", c.path),
            ));
        }
    }
    let outcomes = apply_changes(
        ctx,
        &mut ws,
        &changes,
        format!("{} r{head}", args.source),
        head,
    )
    .await?;
    set_pending_merges(&mut ws, std::slice::from_ref(&range))?;

    let counts = count(&outcomes);
    for (p, o) in &outcomes {
        if *o != Outcome::Applied {
            ctx.out.line(format!("{:<10} {p}", o.name()));
        }
    }
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    ctx.out.line(format!(
        "Merged {}: {}. Commit to record the merge.",
        describe(&range),
        if summary.is_empty() {
            "no file changes".to_owned()
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
        "source": args.source,
        "from_rev": range.from_rev,
        "to_rev": range.to_rev,
        "base": base,
        "nothing_to_do": false,
        "files": files,
        "counts": counts,
    }))
}
