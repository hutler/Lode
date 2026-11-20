//! `lode branch` and `lode switch`.

use lode_proto::depot::{BranchState, BranchView, CreateBranchRequest, PatchBranchRequest};
pub use lode_proto::depot::{StreamSetting, StreamType};
use lode_workspace::db::keys;
use serde_json::{json, Value};

use crate::cmd::commit::explain;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
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
        /// Make it a stream: `development` or `release` (its parent must be a stream).
        #[arg(long = "type", value_name = "TYPE", value_parser = parse_child_type)]
        stream: Option<StreamType>,
    },
    /// Archive a branch: hidden from lists and closed to commits.
    Archive {
        /// Branch name.
        name: String,
    },
    /// Set or clear a branch's stream type (depot admins).
    SetType {
        /// Branch name.
        name: String,
        /// `mainline`, `development`, `release` or `none`.
        #[arg(value_parser = parse_setting)]
        stream: StreamSetting,
    },
    /// Show branches as a tree of streams, with what each could merge down and up.
    Tree {
        /// Include archived branches.
        #[arg(long)]
        all: bool,
    },
}

fn parse_child_type(s: &str) -> Result<StreamType, String> {
    match s {
        "development" => Ok(StreamType::Development),
        "release" => Ok(StreamType::Release),
        "mainline" => Err("a new branch has a parent, so it cannot be a mainline; use `lode branch set-type` on a branch without one".into()),
        _ => Err("expected `development` or `release`".into()),
    }
}

fn parse_setting(s: &str) -> Result<StreamSetting, String> {
    match s {
        "mainline" => Ok(StreamSetting::Mainline),
        "development" => Ok(StreamSetting::Development),
        "release" => Ok(StreamSetting::Release),
        "none" => Ok(StreamSetting::None),
        _ => Err("expected `mainline`, `development`, `release` or `none`".into()),
    }
}

/// `lode branch` options.
#[derive(Debug, clap::Args)]
#[command(
    long_about = "List, create or archive branches, and arrange them as streams.
\nA stream is a branch with a type: `mainline` (the trunk, no parent), `development` (takes
merges from its parent, copies up once up to date) or `release` (takes only picked revisions
from its parent, merges up freely). `lode merge` follows these rules between streams; ordinary
branches keep the v1 rules.
\nWith --json: list prints {\"current\",\"branches\":[branch]}; create, archive and set-type
print the branch, create adds \"switch\" (the switch result) with --switch; tree prints
{\"current\",\"tree\":[node]} with node = {\"name\",\"stream\",\"head_rev\",\"down\",\"up\",
\"children\":[node]} (down: parent revisions not merged into it, up: its revisions not merged
into the parent; null at the top). A branch is {\"name\",\"head_rev\",\"created_rev\",
\"parent\":{\"branch\",\"rev\"}|null,\"state\",\"stream\":type|null}."
)]
pub struct BranchArgs {
    #[command(subcommand)]
    pub command: BranchCmd,
}

/// `lode switch` options.
#[derive(Debug, Default, clap::Args)]
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
        "stream": b.stream.map(StreamType::as_str),
    })
}

fn stream_label(b: &BranchView) -> String {
    b.stream
        .map(|t| format!(" [{}]", t.as_str()))
        .unwrap_or_default()
}

/// One branch of `lode branch tree` and the branches created from it.
fn tree_node<'a>(
    b: &'a BranchView,
    all: &'a [BranchView],
    counts: &'a std::collections::HashMap<String, (u64, u64)>,
    depth: usize,
    current: &str,
    ctx: &Ctx,
) -> Value {
    let (down, up) = match counts.get(&b.name) {
        Some((d, u)) => (json!(d), json!(u)),
        None => (Value::Null, Value::Null),
    };
    let mark = if b.name == current { "*" } else { " " };
    let pending = match counts.get(&b.name) {
        Some((d, u)) => format!("  ↓{d} ↑{u}"),
        None => String::new(),
    };
    ctx.out.line(format!(
        "{mark} {}{}{} r{}{pending}",
        "  ".repeat(depth),
        b.name,
        stream_label(b),
        b.head_rev
    ));
    let children: Vec<Value> = all
        .iter()
        .filter(|c| c.parent.as_ref().is_some_and(|p| p.branch == b.name))
        .map(|c| tree_node(c, all, counts, depth + 1, current, ctx))
        .collect();
    json!({
        "name": b.name,
        "stream": b.stream.map(StreamType::as_str),
        "head_rev": b.head_rev,
        "down": down,
        "up": up,
        "children": children,
    })
}

/// Runs `lode branch`.
pub async fn branch(ctx: &Ctx, args: BranchArgs) -> OpResult<Value> {
    let ws = local::open(ctx)?;
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?;
    let current = ws
        .db()
        .config(keys::BRANCH)?
        .ok_or_else(|| OpError::failed("workspace", "workspace has no branch"))?;
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
                    "{mark} {:<24} r{}{from}{archived}{}",
                    b.name,
                    b.head_rev,
                    stream_label(b)
                ));
            }
            Ok(json!({
                "current": current,
                "branches": list.branches.iter().map(view).collect::<Vec<_>>(),
            }))
        }
        BranchCmd::Create {
            name,
            from,
            switch,
            stream,
        } => {
            let (from_branch, from_rev) = match from.as_deref() {
                None => (current.clone(), Some(synced)),
                Some(spec) => match spec.rsplit_once('@') {
                    Some((b, r)) => {
                        let digits = r.strip_prefix('r').unwrap_or(r);
                        let rev = digits.parse::<u64>().map_err(|_| {
                            OpError::usage(format!("`{spec}` is not BRANCH or BRANCH@REV"))
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
                stream,
            };
            let b = client.create_branch(&depot, &req).await.map_err(explain)?;
            ctx.out.line(format!(
                "Created {} `{}` at r{}{}",
                b.stream
                    .map(|t| format!("{} stream", t.as_str()))
                    .unwrap_or_else(|| "branch".to_owned()),
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
                stream: None,
            };
            let b = client
                .patch_branch(&depot, &name, &req)
                .await
                .map_err(explain)?;
            ctx.out.line(format!("Archived branch `{}`", b.name));
            Ok(view(&b))
        }
        BranchCmd::SetType { name, stream } => {
            let req = PatchBranchRequest {
                state: None,
                stream: Some(stream),
            };
            let b = client
                .patch_branch(&depot, &name, &req)
                .await
                .map_err(explain)?;
            match b.stream {
                Some(t) => ctx
                    .out
                    .line(format!("`{}` is now a {} stream", b.name, t.as_str())),
                None => ctx
                    .out
                    .line(format!("`{}` is now an ordinary branch", b.name)),
            }
            Ok(view(&b))
        }
        BranchCmd::Tree { all } => {
            let list = client.branches(&depot, all).await?;
            let mut counts = std::collections::HashMap::new();
            for b in &list.branches {
                let Some(p) = &b.parent else { continue };
                if !list.branches.iter().any(|x| x.name == p.branch) {
                    continue;
                }
                let down = client.merge_info(&depot, &b.name, &p.branch).await?;
                let up = client.merge_info(&depot, &p.branch, &b.name).await?;
                counts.insert(b.name.clone(), (down.pending, up.pending));
            }
            let roots: Vec<Value> = list
                .branches
                .iter()
                .filter(|b| {
                    b.parent
                        .as_ref()
                        .is_none_or(|p| !list.branches.iter().any(|x| x.name == p.branch))
                })
                .map(|b| tree_node(b, &list.branches, &counts, 0, &current, ctx))
                .collect();
            Ok(json!({ "current": current, "tree": roots }))
        }
    }
}

/// Runs `lode switch`.
pub async fn switch(ctx: &Ctx, args: SwitchArgs) -> OpResult<Value> {
    crate::cmd::update::sync(ctx, args.rev, Some(args.branch)).await
}
