//! `lode events`: what happened in a depot since a cursor (SPEC §6.7), for scripts and
//! agents that watch a depot. Works in a workspace or, with `--depot`, without one.

use lode_proto::feed::{EventsQuery, MAX_WAIT_SECONDS};
use serde_json::{json, Value};

use crate::cmd::checks::module_missing;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local;
use lode_workspace::db::keys;

/// `lode events` options.
#[derive(Debug, Default, clap::Args)]
#[command(
    long_about = "List the events of a depot after a cursor: new revisions, branches \
created or archived, locks taken or released, shelves created, updated, submitted or deleted, \
and check results. Events carry ids only; read details with `lode log`, `lode shelf show`, \
`lode locks` or `lode checks`.
\nWithout --after it prints no events and the current cursor, so a script starts from now. \
Pass the printed `next` as --after the next time. --wait S holds the request up to S seconds \
(at most 55) until an event arrives. `truncated` means events after the cursor were dropped \
(they are kept for 7 days): read the history again, then continue from `next`.
\nWith --json: {\"events\":[{\"id\",\"event\",\"depot\",\"created_at\",\"data\"}],\"next\",\
\"truncated\":bool}."
)]
pub struct EventsArgs {
    /// Depot, instead of the workspace's.
    #[arg(long)]
    pub depot: Option<String>,
    /// Only events after this cursor.
    #[arg(long, value_name = "ID")]
    pub after: Option<i64>,
    /// Only these events, comma-separated, such as `shelf.created,lock.released`.
    #[arg(long, value_name = "EVENTS")]
    pub kinds: Option<String>,
    /// Wait up to this many seconds for an event.
    #[arg(long, value_name = "SECONDS")]
    pub wait: Option<u64>,
}

/// Runs `lode events`.
pub async fn events(ctx: &Ctx, args: EventsArgs) -> OpResult<Value> {
    if args.wait.is_some_and(|w| w > MAX_WAIT_SECONDS) {
        return Err(OpError::usage(format!(
            "--wait is at most {MAX_WAIT_SECONDS} seconds"
        )));
    }
    let ws = match &args.depot {
        Some(_) => None,
        None => Some(local::open(ctx).map_err(|e| {
            if e.code == "not_a_workspace" {
                OpError::usage("not in a workspace: run this inside one, or give --depot")
            } else {
                e
            }
        })?),
    };
    let depot = match (&args.depot, &ws) {
        (Some(d), _) => d.clone(),
        (None, ws) => ws
            .as_ref()
            .map(|w| w.db().config(keys::DEPOT))
            .transpose()?
            .flatten()
            .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?,
    };
    let server = ctx.server_url(ws.as_ref())?;
    let client = ctx.client(&server)?;
    let q = EventsQuery {
        after: args.after,
        kinds: args.kinds,
        wait: args.wait,
    };
    let list = match client.events(&depot, &q).await {
        Ok(l) => l,
        Err(e) => return Err(module_missing(&client, e, "feed").await),
    };
    for e in &list.events {
        ctx.out.line(format!(
            "{:>8}  {:<17} {}  {}",
            e["id"],
            e["event"].as_str().unwrap_or_default(),
            e["created_at"].as_str().unwrap_or_default(),
            e["data"]
        ));
    }
    if list.truncated {
        ctx.out
            .line("Some events after the cursor were dropped; read the history again.");
    }
    ctx.out.line(format!("next: {}", list.next));
    Ok(json!({
        "events": list.events,
        "next": list.next,
        "truncated": list.truncated,
    }))
}
