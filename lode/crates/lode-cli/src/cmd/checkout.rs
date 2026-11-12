//! `lode checkout <server-url>/<depot> [dir]`.

use std::path::PathBuf;

use futures::TryStreamExt;
use lode_client::{Client, ServerUrl};
use lode_proto::read::ManifestEntry;
use lode_workspace::db::keys;
use lode_workspace::{ViewSpec, Workspace, WorkspaceInit};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::materialize::fetch_entries;
use crate::output::human_bytes;

/// Config key: revision a checkout is filling in; removed when it is complete.
pub const CHECKOUT_REV: &str = "checkout_rev";

/// `lode checkout` options.
#[derive(Debug, clap::Args)]
pub struct CheckoutArgs {
    /// `<server-url>/<depot>`, or a depot name with --server or LODE_SERVER.
    pub target: String,
    /// Directory to create (default: the depot name).
    pub dir: Option<PathBuf>,
    /// Branch to check out.
    #[arg(long, short = 'b', default_value = "main")]
    pub branch: String,
    /// Only paths matching this glob (repeatable).
    #[arg(long)]
    pub include: Vec<String>,
    /// Leave out paths matching this glob (repeatable).
    #[arg(long)]
    pub exclude: Vec<String>,
}

/// Splits `https://host/prefix/depot` into the server and the depot.
fn split_target(ctx: &Ctx, target: &str) -> CliResult<(ServerUrl, String)> {
    if target.contains("://") {
        let trimmed = target.trim_end_matches('/');
        let (server, depot) = trimmed
            .rsplit_once('/')
            .filter(|(s, d)| !d.is_empty() && s.contains("://") && !s.ends_with('/'))
            .ok_or_else(|| {
                CliError::usage(format!("`{target}` has no depot: use <server-url>/<depot>"))
            })?;
        if server.ends_with(':') || server.ends_with("://") {
            return Err(CliError::usage(format!(
                "`{target}` has no depot: use <server-url>/<depot>"
            )));
        }
        Ok((ctx.parse_server(server)?, depot.to_owned()))
    } else {
        Ok((ctx.server_url(None)?, target.to_owned()))
    }
}

/// Fetches the full manifest at `rev` for `view` and returns it with the access epoch.
pub async fn full_manifest(
    client: &Client,
    depot: &str,
    rev: u64,
    view: &ViewSpec,
) -> CliResult<(Vec<ManifestEntry>, Option<u64>)> {
    let stream = client
        .manifest(depot, rev, &view.include, &view.exclude)
        .await?;
    let epoch = stream.access_epoch();
    let entries: Vec<ManifestEntry> = stream.try_collect().await?;
    Ok((entries, epoch))
}

/// Creates (or resumes) a workspace and fills it.
pub async fn checkout(ctx: &Ctx, args: CheckoutArgs) -> CliResult<Value> {
    let (server, depot) = split_target(ctx, &args.target)?;
    let dir = ctx.local(args.dir.as_deref().unwrap_or(std::path::Path::new(&depot)));
    let view = ViewSpec {
        include: args.include.clone(),
        exclude: args.exclude.clone(),
    };
    view.compile()?;
    let client = ctx.client(&server)?;

    let resuming = dir
        .join(lode_workspace::layout::DOT_LODE)
        .join(lode_workspace::layout::DB_FILE)
        .is_file();
    let mut ws = if resuming {
        let ws = Workspace::open(&lode_workspace::paths::canonical(&dir)?)?;
        let same = ws.db().config(keys::DEPOT)?.as_deref() == Some(depot.as_str())
            && ws.db().config(keys::BRANCH)?.as_deref() == Some(args.branch.as_str());
        if ws.db().config(CHECKOUT_REV)?.is_none() || !same {
            return Err(CliError::failed(
                "already_a_workspace",
                format!(
                    "{} is already a workspace; use `lode update` there",
                    dir.display()
                ),
            ));
        }
        ws
    } else {
        if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
            return Err(CliError::failed(
                "not_empty",
                format!("{} exists and is not empty", dir.display()),
            ));
        }
        // Ask the server first, so a wrong depot or branch leaves nothing behind.
        let head = client.branch(&depot, &args.branch).await?.head_rev;
        let mut ws = Workspace::create(
            &dir,
            &WorkspaceInit {
                server_url: server.to_string(),
                depot: depot.clone(),
                branch: args.branch.clone(),
                view: view.clone(),
            },
        )?;
        ws.db_mut()
            .write(|tx| tx.set_config(CHECKOUT_REV, &head.to_string()))?;
        ws
    };
    let _lock = ws.lock()?;

    let rev = match ws.db().config_u64(CHECKOUT_REV)? {
        Some(r) => r,
        None => {
            let head = client.branch(&depot, &args.branch).await?.head_rev;
            ws.db_mut()
                .write(|tx| tx.set_config(CHECKOUT_REV, &head.to_string()))?;
            head
        }
    };
    let view = ws.view_spec()?;
    let (entries, epoch) = full_manifest(&client, &depot, rev, &view).await?;
    let count = entries.len();
    let fetched = fetch_entries(&mut ws, &client, &depot, entries, &ctx.out).await?;
    ws.db_mut().write(|tx| {
        tx.set_config(keys::SYNCED_REV, &rev.to_string())?;
        if let Some(e) = epoch {
            tx.set_config(keys::ACCESS_EPOCH, &e.to_string())?;
        }
        tx.remove_config(CHECKOUT_REV)
    })?;
    let root = ws.root().to_path_buf();
    ctx.out.line(format!(
        "Checked out {depot}@{} r{rev} into {}: {count} files ({}).",
        args.branch,
        root.display(),
        human_bytes(fetched.bytes)
    ));
    Ok(json!({
        "workspace": root,
        "server": server.as_str(),
        "depot": depot,
        "branch": args.branch,
        "rev": rev,
        "files": count,
        "fetched": fetched.files,
        "downloaded": fetched.downloaded,
        "resumed": resuming,
    }))
}
