//! `lode branch` and `lode switch`.

use lode_proto::depot::{BranchState, BranchView, CreateBranchRequest, PatchBranchRequest};
use lode_workspace::db::keys;
use serde_json::{json, Value};

use crate::cmd::commit::explain;
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local;

/// `lode branch` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum BranchCmd {
    /// List the branches of this depot.
    List {
        /// Include archived branches.
        #[arg(long)]
        all: bool,
    },
    /// Create a branch.
    Create {
        /// New branch name.
        name: String,
        /// Where it starts: `BRANCH` or `BRANCH@REV` (default: this workspace's branch at the
        /// revision the workspace is at).
        #[arg(long, value_name = "BRANCH[@REV]")]
        from: Option<String>,
        /// Switch the workspace to the new branch.
        #[arg(long)]
        switch: bool,
    },
    /// Archive a branch: hidden from lists and closed to commits.
    Archive {
        /// Branch name.
        name: String,
    },
}

/// `lode branch` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "List, create or archive branches.
\nWith --json: list prints {\"current\",\"branches\":[branch]}; create prints the branch, plus
\"switch\" (the switch result) with --switch; archive prints the branch. A branch is
{\"name\",\"head_rev\",\"created_rev\",\"parent\":{\"branch\",\"rev\"}|null,\"state\"}.")]
pub struct BranchArgs {
    #[command(subcommand)]
    pub command: BranchCmd,
}

/// `lode switch` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Move the workspace to another branch.
\nLike `lode update`, but to the head (or -r REV) of another branch: local changes are carried
over and merged, and conflicts are left as an update leaves them. Locks held here stay on the
old branch and are listed in \"locks_left\". With --json it prints the update result plus
\"branch\" and \"switched_from\".")]
pub struct SwitchArgs {
    /// Branch to switch to.
    pub branch: String,
    /// Revision of that branch (default: its head).
    #[arg(short = 'r', long = "rev")]
    pub rev: Option<u64>,
}

fn view(b: &BranchView) -> Value {
    json!({
        "name": b.name,
        "head_rev": b.head_rev,
        "created_rev": b.created_rev,
        "parent": b.parent.as_ref().map(|p| json!({ "branch": p.branch, "rev": p.rev })),
        "state": match b.state {
            BranchState::Active => "active",
            BranchState::Archived => "archived",
        },
    })
}

/// Runs `lode branch`.
pub async fn branch(ctx: &Ctx, args: BranchArgs) -> CliResult<Value> {
    let ws = local::open(ctx)?;
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| CliError::failed("workspace", "workspace has no depot"))?;
    let current = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| CliError::failed("workspace", "workspace has no branch"))?;
    let synced = local::synced_rev(&ws)?;
    let server = ctx.server_url(Some(&ws))?;
    let client = ctx.client(&server)?;
    drop(ws);
    match args.command {
        BranchCmd::List { all } => {
            let list = client.branches(&depot, all).await?;
            for b in &list.branches {
                let mark = if b.name == current { "*" } else { " " };
                let from = b
                    .parent
                    .as_ref()
                    .map(|p| format!(" (from {}@{})", p.branch, p.rev))
                    .unwrap_or_default();
                let archived = if b.state == BranchState::Archived {
                    " [archived]"
                } else {
                    ""
                };
                ctx.out.line(format!(
                    "{mark} {:<24} r{}{from}{archived}",
                    b.name, b.head_rev
                ));
            }
            Ok(json!({
                "current": current,
                "branches": list.branches.iter().map(view).collect::<Vec<_>>(),
            }))
        }
        BranchCmd::Create { name, from, switch } => {
            let (from_branch, from_rev) = match from.as_deref() {
                None => (current.clone(), Some(synced)),
                Some(spec) => match spec.rsplit_once('@') {
                    Some((b, r)) => {
                        let digits = r.strip_prefix('r').unwrap_or(r);
                        let rev = digits.parse::<u64>().map_err(|_| {
                            CliError::usage(format!("`{spec}` is not BRANCH or BRANCH@REV"))
                        })?;
                        (b.to_owned(), Some(rev))
                    }
                    None => (spec.to_owned(), None),
                },
            };
            let req = CreateBranchRequest {
                name: name.clone(),
                from_branch,
                from_rev,
            };
            let b = client.create_branch(&depot, &req).await.map_err(explain)?;
            ctx.out.line(format!(
                "Created branch `{}` at r{}{}",
                b.name,
                b.created_rev,
                b.parent
                    .as_ref()
                    .map(|p| format!(" from {}@{}", p.branch, p.rev))
                    .unwrap_or_default()
            ));
            let mut out = view(&b);
            if switch {
                let s = crate::cmd::update::sync(ctx, None, Some(name)).await?;
                out["switch"] = s;
            }
            Ok(out)
        }
        BranchCmd::Archive { name } => {
            if name == current {
                ctx.out.warn(format!(
                    "this workspace is on `{name}`; it can no longer commit there"
                ));
            }
            let req = PatchBranchRequest {
                state: Some(BranchState::Archived),
            };
            let b = client
                .patch_branch(&depot, &name, &req)
                .await
                .map_err(explain)?;
            ctx.out.line(format!("Archived branch `{}`", b.name));
            Ok(view(&b))
        }
    }
}

/// Runs `lode switch`.
pub async fn switch(ctx: &Ctx, args: SwitchArgs) -> CliResult<Value> {
    crate::cmd::update::sync(ctx, args.rev, Some(args.branch)).await
}
