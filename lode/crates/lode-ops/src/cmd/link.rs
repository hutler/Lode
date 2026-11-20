//! `lode link`: prints the `lode://` link of a file, a revision or a shelf (T176), the same
//! links Lode Desk copies and opens.

use std::path::Path;

use lode_client::ServerUrl;
use lode_workspace::db::keys;
use serde_json::{json, Value};

use crate::cmd::history::split_rev;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::links::{self, Link};
use crate::local;

/// `lode link` options.
#[derive(Debug, Default, clap::Args)]
pub struct LinkArgs {
    /// A file, optionally `PATH@REV`. In a workspace a local path at the workspace's revision;
    /// with --depot a depot path.
    #[arg(conflicts_with_all = ["rev", "shelf"])]
    pub path: Option<String>,
    /// A revision: `lode link -r 42`.
    #[arg(
        short = 'r',
        long = "rev",
        value_name = "REV",
        conflicts_with = "shelf"
    )]
    pub rev: Option<u64>,
    /// A shelf: `lode link --shelf 7`.
    #[arg(long, value_name = "ID")]
    pub shelf: Option<u64>,
    /// For a file: the head of this branch instead of a fixed revision.
    #[arg(long, short = 'b')]
    pub branch: Option<String>,
    /// Depot, instead of the workspace's.
    #[arg(long)]
    pub depot: Option<String>,
}

/// `https://host[:port]` (or `http://`) of a server address, the form links carry.
pub fn link_server(server: &ServerUrl) -> String {
    server.origin().ascii_serialization()
}

/// The link of shelf `id` in `depot` on `server`, or `None` when the server address cannot
/// be put in a link (a server under a path).
pub fn shelf_link(server: &ServerUrl, depot: &str, id: u64) -> Option<String> {
    links::build(&Link::Shelf {
        server: link_server(server),
        depot: depot.to_owned(),
        id,
    })
    .ok()
}

/// Prints the link and returns it with its parts.
pub async fn link(ctx: &Ctx, args: LinkArgs) -> OpResult<Value> {
    let ws = if args.depot.is_some() {
        None
    } else {
        Some(local::open(ctx).map_err(|e| {
            if e.code == "not_a_workspace" {
                OpError::usage("not in a workspace: run this inside one, or give --depot")
            } else {
                e
            }
        })?)
    };
    let server = link_server(&ctx.server_url(ws.as_ref())?);
    let depot = match (&args.depot, &ws) {
        (Some(d), _) => d.clone(),
        (None, Some(w)) => w
            .db()
            .config(keys::DEPOT)?
            .ok_or_else(|| OpError::failed("workspace", "workspace has no depot"))?,
        (None, None) => return Err(OpError::usage("give --depot")),
    };
    let link = if let Some(id) = args.shelf {
        Link::Shelf { server, depot, id }
    } else if let Some(path) = &args.path {
        let (p, rev) = split_rev(path)?;
        let depot_path = match &ws {
            Some(w) => lode_workspace::paths::to_depot(w.root(), &ctx.cwd, Path::new(p))?
                .ok_or_else(|| OpError::usage("give a file, not the workspace root"))?
                .as_str()
                .to_owned(),
            None => {
                let t = p.trim_matches('/');
                lode_core::DepotPath::parse(t)
                    .map_err(|e| OpError::usage(format!("{p}: {e}")))?
                    .as_str()
                    .to_owned()
            }
        };
        // Without a revision or a branch: the revision the workspace is at, so the link
        // shows what this person sees; outside a workspace, the head of `main`.
        let (rev, branch) = match (rev, &args.branch, &ws) {
            (Some(r), b, _) => (Some(r), b.clone()),
            (None, Some(b), _) => (None, Some(b.clone())),
            (None, None, Some(w)) => (Some(local::synced_rev(w)?), None),
            (None, None, None) => (None, Some("main".to_owned())),
        };
        Link::File {
            server,
            depot,
            path: depot_path,
            rev,
            branch,
        }
    } else if let Some(rev) = args.rev {
        Link::Revision { server, depot, rev }
    } else {
        return Err(OpError::usage(
            "say what to link: a file (PATH[@REV]), -r REV or --shelf ID",
        ));
    };
    let text = links::build(&link).map_err(|e| OpError::failed("bad_link", e.to_string()))?;
    ctx.out.line(&text);
    let mut v = serde_json::to_value(&link).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("link".into(), json!(text));
    }
    Ok(v)
}
