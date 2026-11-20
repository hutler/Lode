//! `lode checks`: the results outside systems reported on a revision or a shelf version
//! (SPEC §6.6). Works in a workspace or, with `--server` and `--depot`, without one.

use lode_client::Client;
use lode_core::ErrorCode;
use lode_proto::checks::{CheckList, CheckState, CheckTargetKind, CheckView, ChecksQuery};
use serde_json::{json, Value};

use crate::cmd::history::format_time;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local;
use lode_workspace::db::keys;

/// `lode checks` options.
#[derive(Debug, Default, clap::Args)]
#[command(
    long_about = "List the checks reported on a revision (`r214`) or on one version of \
a shelf (`37@3`; `37` alone means the shelf's current version): name, state, reporter, time, \
summary and link.
\nChecks belong to an exact shelf version, so a shelf that changed must pass again.
\nWith --json: {\"target\":{\"kind\",\"id\",\"version\"?},\"checks\":[{\"target\",\"name\",\
\"state\",\"summary\"?,\"url\"?,\"reporter\":{\"name\",\"kind\",\"owner\"?},\"updated_at\"}]}."
)]
pub struct ChecksArgs {
    /// `rN` for a revision, `ID` or `ID@VERSION` for a shelf.
    pub target: String,
    /// Depot, instead of the workspace's.
    #[arg(long)]
    pub depot: Option<String>,
}

/// What `lode checks` was asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spec {
    Revision(u64),
    Shelf(u64, Option<u64>),
}

fn parse_spec(s: &str) -> OpResult<Spec> {
    let bad = || {
        OpError::usage(format!(
            "`{s}` is not a target: use rN for a revision, ID or ID@VERSION for a shelf"
        ))
    };
    let num = |t: &str| t.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(bad);
    if let Some(rest) = s.strip_prefix('r').or_else(|| s.strip_prefix('R')) {
        return Ok(Spec::Revision(num(rest)?));
    }
    match s.split_once('@') {
        Some((id, v)) => Ok(Spec::Shelf(num(id)?, Some(num(v)?))),
        None => Ok(Spec::Shelf(num(s)?, None)),
    }
}

/// When a 404 means the server runs without a module, says so.
pub(crate) async fn module_missing(
    client: &Client,
    e: lode_client::Error,
    module: &str,
) -> OpError {
    if e.is_code(ErrorCode::NotFound) {
        if let Ok(info) = client.info().await {
            if !info.modules.iter().any(|m| m == module) {
                return OpError::failed(
                    "module_disabled",
                    format!("this server does not have the {module} module enabled"),
                );
            }
        }
    }
    e.into()
}

fn state_name(s: CheckState) -> &'static str {
    s.as_str()
}

/// One line per check: state, name, reporter, time, then summary and link.
pub(crate) fn check_line(c: &CheckView) -> String {
    let mut line = format!(
        "{:<9} {:<24} {:<16} {}",
        state_name(c.state),
        c.name,
        c.reporter.name,
        format_time(c.updated_at)
    );
    if let Some(s) = &c.summary {
        line.push_str("  ");
        line.push_str(s.lines().next().unwrap_or_default());
    }
    if let Some(u) = &c.url {
        line.push_str("  ");
        line.push_str(u);
    }
    line
}

/// Runs `lode checks`.
pub async fn checks(ctx: &Ctx, args: ChecksArgs) -> OpResult<Value> {
    let spec = parse_spec(&args.target)?;
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
    let query = match spec {
        Spec::Revision(n) => ChecksQuery {
            target_kind: CheckTargetKind::Revision,
            target_id: n,
            target_version: None,
        },
        Spec::Shelf(id, version) => {
            let version = match version {
                Some(v) => v,
                None => match client.shelf(&depot, id, false).await {
                    Ok(s) => s.version,
                    Err(e) => return Err(module_missing(&client, e, "shelves").await),
                },
            };
            ChecksQuery {
                target_kind: CheckTargetKind::Shelf,
                target_id: id,
                target_version: Some(version),
            }
        }
    };
    let list: CheckList = match client.checks(&depot, &query).await {
        Ok(l) => l,
        Err(e) => return Err(module_missing(&client, e, "checks").await),
    };
    let what = match query.target_version {
        Some(v) => format!("shelf {} version {v}", query.target_id),
        None => format!("r{}", query.target_id),
    };
    if list.checks.is_empty() {
        ctx.out.line(format!("No checks on {what}."));
    } else {
        ctx.out.line(format!("Checks on {what}:"));
        for c in &list.checks {
            ctx.out.line(check_line(c));
        }
    }
    let mut target = json!({ "kind": query.target_kind.as_str(), "id": query.target_id });
    if let Some(v) = query.target_version {
        target["version"] = json!(v);
    }
    let mut out =
        serde_json::to_value(&list).map_err(|e| OpError::failed("internal", e.to_string()))?;
    out["target"] = target;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(parse_spec("r214").unwrap(), Spec::Revision(214));
        assert_eq!(parse_spec("37").unwrap(), Spec::Shelf(37, None));
        assert_eq!(parse_spec("37@3").unwrap(), Spec::Shelf(37, Some(3)));
        for bad in ["", "r", "r0", "x1", "37@", "@3", "37@x", "-1"] {
            assert!(parse_spec(bad).is_err(), "{bad}");
        }
    }
}
