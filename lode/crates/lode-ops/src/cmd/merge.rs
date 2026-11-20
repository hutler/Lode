//! `lode merge` (SPEC §5.4, ARCHITECTURE §10.5).
//!
//! The changes of another branch since the merge base are applied to the workspace the way
//! `unshelve` applies a shelf: what is unchanged here takes the source's side, the rest is
//! merged three-way with the base as common ancestor, or left in conflict. The merged range
//! is kept in workspace config until the next commit sends it as `merged_from` (or a shelve
//! carries it into a shelf).

use std::collections::{BTreeMap, HashMap, HashSet};

use futures::TryStreamExt;
use lode_client::Client;
use lode_core::{Change, ChangeAction, CopyFrom, DepotPath};
use lode_proto::commit::MergedFrom;
use lode_proto::depot::Flow;
use lode_proto::read::{ManifestDelta, ManifestEntry, RevisionListQuery, RevisionView};
use lode_workspace::db::keys;
use lode_workspace::Workspace;
use serde_json::{json, Value};

use crate::cmd::apply::{
    apply_changes, count, file_list, print_outcomes, refuse_conflicts, Renames,
};
use crate::cmd::commit::{explain, PENDING_COMMIT};
use crate::cmd::update::{check_on_branch, UPDATE_REV};
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local;

/// Config key: the merges the next commit records, as a JSON list of `MergedFrom`.
pub const PENDING_MERGE: &str = "pending_merge";

/// Config key: set by `lode merge --force-flow`, so the commit that records the merge asks the
/// server to skip the stream rules.
pub const PENDING_FORCE_FLOW: &str = "pending_force_flow";

/// `lode merge` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Merge another branch into the workspace.
\nApplies the source branch's changes since the merge base: the highest source revision up to
which everything is already merged here, or the branch point. Files unchanged here take the
source's version; files changed on both sides are merged as `lode update` merges them, and
what cannot be merged is left in conflict (`lode resolve`). The merge is recorded by the next
`lode commit` (as `merged_from`); `lode revert --merge` abandons it.
\n-r N picks the single revision N of the source branch. Known limit: a later full merge
applies that change again (usually without effect, sometimes as a conflict).
\nBetween streams (`lode branch tree`) the flow rules apply: development streams take merges
from their parent and copy up once up to date, release streams take only picks from their
parent and merge up freely, nothing goes sideways. A refused merge stops before any file is
written (`conflict`, details.reason `stream_flow`); a depot admin can pass --force-flow, which
the next commit sends on.
\nWith --json: {\"source\",\"from_rev\",\"to_rev\",\"base\",\"nothing_to_do\":bool,
\"files\":[{\"path\",\"outcome\",\"from\"?}],\"counts\":{outcome:n},\"flow\"}. Outcomes: applied,
merged, conflicted, unchanged, rename_conflict. A source change outside the view refuses the merge (conflict, reason outside_view).
\nRenames are followed: a file renamed on the source and edited here moves to the new name
with your edits merged in; a file renamed here and changed on the source gets the change
under your name (`from` names the path the change was made to). Renamed differently on both
sides, the source's name is left in conflict (`rename_conflict`): `lode resolve --mine`
drops it, `--theirs` keeps it as well.")]
pub struct MergeArgs {
    /// Branch to merge from.
    pub source: String,
    /// Pick only this revision of the source branch.
    #[arg(short = 'r', long = "rev")]
    pub rev: Option<u64>,
    /// Merge even where the stream rules refuse it (depot admins; the commit sends it on).
    #[arg(long)]
    pub force_flow: bool,
}

/// The merges waiting for the next commit.
pub(crate) fn pending_merges(ws: &Workspace) -> OpResult<Vec<MergedFrom>> {
    match ws.db().config(PENDING_MERGE)? {
        None => Ok(Vec::new()),
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| OpError::failed("workspace", format!("stored merge: {e}"))),
    }
}

/// Stores the merges waiting for the next commit (none removes the key).
pub(crate) fn set_pending_merges(ws: &mut Workspace, merges: &[MergedFrom]) -> OpResult<()> {
    if merges.is_empty() {
        ws.db_mut().write(|tx| {
            tx.remove_config(PENDING_MERGE)?;
            tx.remove_config(PENDING_FORCE_FLOW)
        })?;
        return Ok(());
    }
    let text =
        serde_json::to_string(merges).map_err(|e| OpError::failed("internal", e.to_string()))?;
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

/// The error for a merge the stream rules refuse, as the server would answer it.
pub(crate) fn flow_refused(flow: &Flow, source: &str, target: &str) -> OpError {
    let mut details = json!({
        "reason": "stream_flow",
        "direction": flow.direction,
        "source": source,
        "target": target,
    });
    if let Some(b) = flow.behind {
        details["behind"] = json!(b);
    }
    let reason = flow
        .reason
        .clone()
        .unwrap_or_else(|| "the stream rules refuse this merge".to_owned());
    OpError::failed(
        "conflict",
        format!("{reason} (a depot admin can pass --force-flow)"),
    )
    .with_details(details)
}

/// The renames (old path → final name) made by the revisions of `branch` numbered above
/// `after` and up to `upto`: an add with `copy_from` whose source the same revision deletes.
/// A chain `a→b→c` is `a→c`; a renamed file deleted later is left out.
pub(crate) async fn renames_in(
    client: &Client,
    depot: &str,
    branch: &str,
    after: u64,
    upto: u64,
) -> OpResult<Renames> {
    let mut revs: Vec<RevisionView> = Vec::new();
    let mut cursor = after;
    while cursor < upto {
        let q = RevisionListQuery {
            branch: Some(branch.to_owned()),
            after: Some(cursor),
            changes: true,
            limit: Some(200),
            ..Default::default()
        };
        let page = client.revisions(depot, &q).await?;
        let Some(last) = page.revisions.last().map(|r| r.rev) else {
            break;
        };
        revs.extend(page.revisions.into_iter().filter(|r| r.rev <= upto));
        if !page.more || last <= cursor {
            break;
        }
        cursor = last;
    }
    revs.sort_by_key(|r| r.rev);
    // New name → the name it had at `after`.
    let mut origin: BTreeMap<DepotPath, DepotPath> = BTreeMap::new();
    for r in revs {
        let changes = r.changes.unwrap_or_default();
        let deleted: HashSet<&DepotPath> = changes
            .iter()
            .filter(|c| c.action == ChangeAction::Delete)
            .map(|c| &c.path)
            .collect();
        let mut sources: HashSet<&DepotPath> = HashSet::new();
        let mut moved: Vec<(DepotPath, DepotPath)> = Vec::new();
        for c in &changes {
            let Some(cf) = &c.copy_from else { continue };
            if c.action == ChangeAction::Add && deleted.contains(&cf.path) && cf.path != c.path {
                sources.insert(&cf.path);
                moved.push((cf.path.clone(), c.path.clone()));
            }
        }
        for d in &deleted {
            if !sources.contains(d) {
                origin.remove(*d);
            }
        }
        for (from, to) in moved {
            let first = origin.remove(&from).unwrap_or(from);
            origin.insert(to, first);
        }
    }
    Ok(origin
        .into_iter()
        .filter(|(new, old)| new != old)
        .map(|(new, old)| (old, new))
        .collect())
}

/// The changes from `base` (nothing when `None`) to `head`, with each path's base side.
async fn changes_between(
    client: &Client,
    depot: &str,
    base: Option<u64>,
    head: u64,
) -> OpResult<Vec<Change>> {
    let Some(base) = base else {
        let entries: Vec<ManifestEntry> = client
            .manifest(depot, head, &[], &[])
            .await?
            .try_collect()
            .await?;
        return Ok(entries.into_iter().map(|e| put(e, None)).collect());
    };
    let forward: Vec<ManifestDelta> = client
        .manifest_delta(depot, base, head, &[], &[])
        .await?
        .try_collect()
        .await?;
    // The same delta the other way round has the base side of every changed path.
    let backward: Vec<ManifestDelta> = client
        .manifest_delta(depot, head, base, &[], &[])
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
pub async fn merge(ctx: &Ctx, args: MergeArgs) -> OpResult<Value> {
    let mut ws = local::open(ctx)?;
    let _lock = ws.lock()?;
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
    refuse_conflicts(&ws)?;
    if let Some(m) = pending_merges(&ws)?.first() {
        return Err(OpError::failed(
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
        .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?;
    let branch = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no branch"))?;
    if args.source == branch {
        return Err(OpError::usage(format!(
            "the workspace is on `{branch}` already"
        )));
    }
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;

    let mut flow: Option<Flow> = None;
    let mut branch_point: Option<u64> = None;
    let (base, head, range) = match args.rev {
        Some(n) => {
            check_on_branch(&client, &depot, &args.source, n).await?;
            // Branches without shared history have no flow to check.
            if let Ok(info) = client.merge_info(&depot, &branch, &args.source).await {
                branch_point = Some(info.branch_point);
                if let Some(f) = info.flow {
                    if !f.pick_allowed && !args.force_flow {
                        return Err(flow_refused(&f, &args.source, &branch));
                    }
                    flow = Some(f);
                }
            }
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
            if let Some(f) = &info.flow {
                if !f.allowed && !args.force_flow && info.pending > 0 {
                    // One pending revision is a pick, which the rules may still allow.
                    if !(info.pending == 1 && f.pick_allowed) {
                        return Err(flow_refused(f, &args.source, &branch));
                    }
                }
            }
            flow = info.flow.clone();
            branch_point = Some(info.branch_point);
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
                    "flow": flow,
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

    let view = ws.view_spec()?.compile()?;
    let mut changes = changes_between(&client, &depot, base, head).await?;
    // Merge history records a whole revision range, so every changed path must fit the view.
    if let Some(change) = changes.iter().find(|change| !view.contains(&change.path)) {
        return Err(OpError::failed(
            "conflict",
            format!(
                "{} is outside the workspace view; expand the view before merging",
                change.path
            ),
        )
        .with_details(json!({ "reason": "outside_view", "path": change.path })));
    }
    // Renames on the source side become adds copied from the old name, as a commit of them
    // here should record; renames committed on this branch since the branch point are
    // followed too.
    let synced = local::synced_rev(&ws)?;
    let there = renames_in(&client, &depot, &args.source, base.unwrap_or(0), head).await?;
    let deleted: HashSet<DepotPath> = changes
        .iter()
        .filter(|c| c.action == ChangeAction::Delete)
        .map(|c| c.path.clone())
        .collect();
    for c in &mut changes {
        if c.action != ChangeAction::Add || c.copy_from.is_some() {
            continue;
        }
        if let Some((old, _)) = there.iter().find(|(_, new)| **new == c.path) {
            if deleted.contains(old) {
                c.copy_from = Some(CopyFrom {
                    path: old.clone(),
                    rev: synced,
                });
            }
        }
    }
    // Up to the branch head: a rename this workspace has not synced yet is ignored by
    // `apply_changes`, which follows only renames whose new name is here.
    let here = match branch_point {
        Some(p) => renames_in(&client, &depot, &branch, p, u64::MAX).await?,
        None => Renames::new(),
    };
    for c in &changes {
        if c.path
            .segments()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case(lode_workspace::layout::DOT_LODE))
        {
            return Err(OpError::failed(
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
        &here,
    )
    .await?;
    set_pending_merges(&mut ws, std::slice::from_ref(&range))?;
    if args.force_flow {
        ws.db_mut()
            .write(|tx| tx.set_config(PENDING_FORCE_FLOW, "true"))?;
    }

    let counts = count(&outcomes.outcomes);
    print_outcomes(ctx, &outcomes);
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
    if counts.contains_key("conflicted") || counts.contains_key("rename_conflict") {
        ctx.out
            .line("Resolve conflicts with `lode resolve <path> --mine|--theirs|--merged`.");
    }
    let config = crate::cmd::update::local_config(&ws);
    let files: Vec<Value> = file_list(&outcomes, config.as_ref());
    if files.iter().any(|f| f.get("tool").is_some()) {
        ctx.out
            .line("Some conflicts have a merge tool: `lode resolve --tool` starts it.");
    }
    Ok(json!({
        "source": args.source,
        "from_rev": range.from_rev,
        "to_rev": range.to_rev,
        "base": base,
        "nothing_to_do": false,
        "files": files,
        "counts": counts,
        "flow": flow,
    }))
}
