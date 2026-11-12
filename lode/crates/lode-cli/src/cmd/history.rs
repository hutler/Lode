//! `lode log`, `lode cat`, `lode ls`: reading history from the server, in a workspace or
//! without one (`--server` and `--depot`).

use std::path::Path;

use base64::Engine as _;
use lode_client::Client;
use lode_core::{ChangeAction, DepotPath, EntryKind};
use lode_proto::read::{RevisionListQuery, RevisionView, TreeEntryView};
use lode_proto::PrincipalKind;
use lode_workspace::db::keys;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncWriteExt;

use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local;

/// Where to read from, for commands that also work outside a workspace.
#[derive(Debug, Clone, clap::Args)]
pub struct Source {
    /// Depot to read, instead of the workspace's (paths are then depot paths).
    #[arg(long)]
    pub depot: Option<String>,
    /// Branch to read (default: the workspace's, else `main`).
    #[arg(long, short = 'b')]
    pub branch: Option<String>,
}

/// `lode log` options.
#[derive(Debug, clap::Args)]
pub struct LogArgs {
    /// Only revisions that changed this file or directory.
    pub path: Option<String>,
    /// At most this many revisions; 0 for all.
    #[arg(short = 'n', long = "max-count", default_value_t = 50)]
    pub max: u32,
    /// Follow a file through copies and renames.
    #[arg(long)]
    pub follow: bool,
    /// List the changed paths of each revision.
    #[arg(long, short = 'v')]
    pub verbose: bool,
    #[command(flatten)]
    pub source: Source,
}

/// `lode cat` options.
#[derive(Debug, clap::Args)]
pub struct CatArgs {
    /// `path` or `path@rev` (`@12` or `@r12`).
    pub path: String,
    #[command(flatten)]
    pub source: Source,
}

/// `lode ls` options.
#[derive(Debug, clap::Args)]
pub struct LsArgs {
    /// Directory or file, optionally `@rev`; the working directory (or, with --depot, the
    /// root) when missing.
    pub path: Option<String>,
    /// Long form: kind, mode, size and id.
    #[arg(short = 'l')]
    pub long: bool,
    #[command(flatten)]
    pub source: Source,
}

/// A resolved source: client, depot, branch, and how to read paths.
struct Target {
    client: Client,
    depot: String,
    branch: String,
    /// The workspace's synced revision, when reading the workspace's own branch.
    synced: Option<u64>,
    /// Workspace root, when paths are local.
    root: Option<std::path::PathBuf>,
}

fn target(ctx: &Ctx, src: &Source) -> CliResult<Target> {
    let ws = if src.depot.is_some() {
        None
    } else {
        Some(local::open(ctx).map_err(|e| {
            if e.code == "not_a_workspace" {
                CliError::usage("not in a workspace: run this inside one, or give --depot")
            } else {
                e
            }
        })?)
    };
    let server = ctx.server_url(ws.as_ref())?;
    let client = ctx.client(&server)?;
    let Some(ws) = ws else {
        return Ok(Target {
            client,
            depot: src.depot.clone().unwrap_or_default(),
            branch: src.branch.clone().unwrap_or_else(|| "main".into()),
            synced: None,
            root: None,
        });
    };
    let depot = ws
        .db()
        .config(keys::DEPOT)?
        .ok_or_else(|| CliError::failed("workspace", "workspace has no depot"))?;
    let ws_branch = ws
        .db()
        .config(keys::BRANCH)?
        .unwrap_or_else(|| "main".into());
    let branch = src.branch.clone().unwrap_or_else(|| ws_branch.clone());
    let synced = if branch == ws_branch {
        Some(local::synced_rev(&ws)?)
    } else {
        None
    };
    Ok(Target {
        client,
        depot,
        branch,
        synced,
        root: Some(ws.root().to_path_buf()),
    })
}

/// Splits `path@rev`. A trailing `@` part that is not a revision stays in the path.
fn split_rev(arg: &str) -> CliResult<(&str, Option<u64>)> {
    if let Some((p, r)) = arg.rsplit_once('@') {
        let digits = r.strip_prefix('r').unwrap_or(r);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            let rev = digits
                .parse()
                .map_err(|_| CliError::usage(format!("bad revision in {arg}")))?;
            return Ok((p, Some(rev)));
        }
    }
    Ok((arg, None))
}

impl Target {
    /// A path argument: local (relative to the working directory) in a workspace, a depot
    /// path otherwise. `None` for the root.
    fn path(&self, ctx: &Ctx, arg: &str) -> CliResult<Option<DepotPath>> {
        if arg.is_empty() {
            return Ok(None);
        }
        match &self.root {
            Some(root) => Ok(lode_workspace::paths::to_depot(
                root,
                &ctx.cwd,
                Path::new(arg),
            )?),
            None => {
                let trimmed = arg.trim_matches('/');
                if trimmed.is_empty() {
                    return Ok(None);
                }
                DepotPath::parse(trimmed)
                    .map(Some)
                    .map_err(|e| CliError::usage(format!("{arg}: {e}")))
            }
        }
    }

    /// The revision to read: the one given, else the workspace's synced revision on its own
    /// branch, else the branch head.
    async fn rev(&self, given: Option<u64>) -> CliResult<u64> {
        if let Some(r) = given.or(self.synced) {
            return Ok(r);
        }
        Ok(self
            .client
            .branch(&self.depot, &self.branch)
            .await?
            .head_rev)
    }
}

fn when(r: &RevisionView) -> String {
    format_time(r.created_at)
}

/// A time as `2026-11-12 10:30:05Z`.
pub(crate) fn format_time(t: time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    let text = t.format(&Rfc3339).unwrap_or_default();
    // `2026-11-12T10:30:05Z` → `2026-11-12 10:30:05Z`, without fractions.
    let text = text.replacen('T', " ", 1);
    match text.split_once('.') {
        Some((head, _)) => format!("{head}Z"),
        None => text,
    }
}

pub(crate) fn who(name: &str, kind: PrincipalKind) -> String {
    match kind {
        PrincipalKind::Agent => format!("{name} [agent]"),
        PrincipalKind::User => name.to_owned(),
    }
}

fn print_revision(ctx: &Ctx, r: &RevisionView, verbose: bool) {
    let mut head = format!(
        "r{}  {}  {}",
        r.rev,
        when(r),
        who(&r.author.name, r.author.kind)
    );
    if r.submitter.name != r.author.name {
        head.push_str(&format!(
            "  (submitted by {})",
            who(&r.submitter.name, r.submitter.kind)
        ));
    }
    if r.branch != "main" {
        head.push_str(&format!("  [{}]", r.branch));
    }
    ctx.out.line(head);
    for m in &r.merged_from {
        ctx.out.line(format!(
            "    merged {} r{}..r{}",
            m.branch, m.from_rev, m.to_rev
        ));
    }
    for line in r.message.lines() {
        if line.trim().is_empty() {
            ctx.out.line("");
        } else {
            ctx.out.line(format!("    {line}"));
        }
    }
    if verbose {
        if let Some(changes) = &r.changes {
            ctx.out.line("");
            for c in changes {
                let letter = match c.action {
                    ChangeAction::Add => 'A',
                    ChangeAction::Modify => 'M',
                    ChangeAction::Delete => 'D',
                };
                let mut line = format!("    {letter} {}", c.path);
                if let Some(from) = &c.copy_from {
                    line.push_str(&format!("  (from {}@r{})", from.path, from.rev));
                }
                ctx.out.line(line);
            }
            if r.changes_truncated {
                ctx.out.line("    …");
            }
        }
    }
    ctx.out.line("");
}

/// Runs `lode log`.
pub async fn log(ctx: &Ctx, args: LogArgs) -> CliResult<Value> {
    let t = target(ctx, &args.source)?;
    let path = match &args.path {
        Some(p) => t.path(ctx, p)?,
        None => None,
    };
    if args.follow && path.is_none() {
        return Err(CliError::usage("--follow needs a path"));
    }
    let mut out: Vec<RevisionView> = Vec::new();
    let mut before = None;
    let mut more = true;
    while more && (args.max == 0 || out.len() < args.max as usize) {
        let want = if args.max == 0 {
            None
        } else {
            Some(args.max - out.len() as u32)
        };
        let page = t
            .client
            .revisions(
                &t.depot,
                &RevisionListQuery {
                    branch: Some(t.branch.clone()),
                    path: path.clone(),
                    before,
                    follow: args.follow,
                    changes: args.verbose,
                    limit: want,
                    ..Default::default()
                },
            )
            .await?;
        more = page.more;
        let Some(last) = page.revisions.last() else {
            break;
        };
        before = Some(last.rev);
        out.extend(page.revisions);
    }
    if args.max != 0 {
        out.truncate(args.max as usize);
    }
    for r in &out {
        print_revision(ctx, r, args.verbose);
    }
    Ok(json!({
        "depot": t.depot,
        "branch": t.branch,
        "revisions": serde_json::to_value(&out).map_err(|e| CliError::failed("internal", e.to_string()))?,
        "more": more,
    }))
}

/// Runs `lode cat`.
pub async fn cat(ctx: &Ctx, args: CatArgs) -> CliResult<Value> {
    let t = target(ctx, &args.source)?;
    let (p, given) = split_rev(&args.path)?;
    let path = t
        .path(ctx, p)?
        .ok_or_else(|| CliError::usage("name a file to print"))?;
    let rev = t.rev(given).await?;
    let file = t.client.file(&t.depot, rev, &path).await?;
    let (id, kind, executable) = (file.id, file.kind, file.executable);
    if ctx.g.json {
        let mut buf = Vec::new();
        let size = file.write_to(&mut buf).await?;
        let content = match String::from_utf8(buf) {
            Ok(s) => json!({ "content": s }),
            Err(e) => json!({
                "content_base64": base64::engine::general_purpose::STANDARD.encode(e.into_bytes())
            }),
        };
        let mut v = json!({
            "path": path.as_str(),
            "rev": rev,
            "id": id.to_hex(),
            "kind": kind,
            "executable": executable,
            "size": size,
        });
        if let (Some(m), Value::Object(c)) = (v.as_object_mut(), content) {
            m.extend(c);
        }
        return Ok(v);
    }
    let mut stdout = tokio::io::stdout();
    let size = file.write_to(&mut stdout).await?;
    stdout.flush().await?;
    Ok(json!({ "path": path.as_str(), "rev": rev, "size": size }))
}

fn mode(e: &TreeEntryView) -> &'static str {
    match (e.kind, e.executable) {
        (EntryKind::Dir, _) => "dir ",
        (EntryKind::Symlink, _) => "link",
        (_, true) => "exec",
        _ => "file",
    }
}

/// Runs `lode ls`.
pub async fn ls(ctx: &Ctx, args: LsArgs) -> CliResult<Value> {
    let t = target(ctx, &args.source)?;
    // In a workspace, no path means the working directory.
    let here = if t.root.is_some() { "." } else { "" };
    let (p, given) = split_rev(args.path.as_deref().unwrap_or(here))?;
    let path = t.path(ctx, p)?;
    let rev = t.rev(given).await?;
    let listing = t.client.tree(&t.depot, rev, path.as_ref()).await;
    let (dir, entries) = match (listing, &path) {
        (Ok(listing), _) => (path.clone(), listing.entries),
        // A file: list its directory and keep that one entry.
        (Err(e), Some(file)) if e.api().is_some_and(|a| a.status != 401 && a.status != 403) => {
            let parent = file.parent();
            let listing = t
                .client
                .tree(&t.depot, rev, parent.as_ref())
                .await
                .map_err(|_| e)?;
            let one: Vec<TreeEntryView> = listing
                .entries
                .into_iter()
                .filter(|x| x.name == file.name() && x.kind != EntryKind::Dir)
                .collect();
            if one.is_empty() {
                return Err(local::not_found(
                    "no such path",
                    &[file.as_str().to_owned()],
                ));
            }
            (parent, one)
        }
        (Err(e), _) => return Err(e.into()),
    };
    let width = entries
        .iter()
        .map(|e| e.size.to_string().len())
        .max()
        .unwrap_or(1);
    for e in &entries {
        let name = if e.kind == EntryKind::Dir {
            format!("{}/", e.name)
        } else {
            e.name.clone()
        };
        if args.long {
            let hex = e.id.to_hex();
            ctx.out.line(format!(
                "{}  {:>width$}  {}  {name}",
                mode(e),
                e.size,
                &hex[..12]
            ));
        } else {
            ctx.out.line(name);
        }
    }
    Ok(json!({
        "depot": t.depot,
        "rev": rev,
        "path": dir.as_ref().map(DepotPath::as_str),
        "entries": serde_json::to_value(&entries).map_err(|e| CliError::failed("internal", e.to_string()))?,
    }))
}
