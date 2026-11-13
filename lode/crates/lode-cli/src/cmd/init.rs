//! `lode init <server-url>/<depot> [--template T] [--dry-run]` (T085): turns the current
//! folder, with its files, into a new depot and its first workspace.
//!
//! Steps: create the depot (administrators only; an existing empty depot is accepted), write
//! `.lodeignore` and `.lodeconfig` from the template unless the folder has them, make the
//! folder a workspace of the depot, and commit every file that is not ignored. The workspace
//! remembers that an init is under way, so a rerun after an interruption finishes the same
//! commit (an interrupted commit resumes as it does for `lode commit`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lode_core::path::DepotPath;
use lode_workspace::db::keys;
use lode_workspace::layout::{DOT_LODE, IGNORE_FILE};
use lode_workspace::{IgnoreStack, ViewSpec, Workspace, WorkspaceInit};
use serde_json::{json, Value};

use crate::cmd::checkout::split_target;
use crate::cmd::commit::{commit, CommitArgs};
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::human_bytes;
use crate::templates::{self, Template};

/// Workspace config key: an init is under way; the value is the template name.
pub const INIT_TEMPLATE: &str = "init_template";

/// `lode init` options.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// `<server-url>/<depot>`, or a depot name with --server or LODE_SERVER.
    pub target: String,
    /// Ignore and lock rules to start with: generic, unreal, unity, godot, rust, node.
    #[arg(long, short = 't', default_value = "generic")]
    pub template: String,
    /// Show how many files and bytes would be committed, and change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// The commit message.
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,
}

/// Files a template's rules leave in, and their total size.
struct Plan {
    files: u64,
    bytes: u64,
    invalid: Vec<String>,
}

/// Walks `root` as the first commit would see it, with the root ignore rules taken from
/// `root_ignore` (the template's, when the folder has no `.lodeignore` yet).
fn plan(root: &Path, root_ignore: &str) -> CliResult<Plan> {
    let (stack, problem) = IgnoreStack::with_text(&IgnoreStack::empty(), root, root_ignore);
    if let Some(p) = problem {
        return Err(CliError::failed("bad_ignore", p));
    }
    let mut out = Plan {
        files: 0,
        bytes: 0,
        invalid: Vec::new(),
    };
    let mut todo: Vec<(PathBuf, Arc<IgnoreStack>, String)> =
        vec![(root.to_owned(), stack, String::new())];
    while let Some((dir, stack, prefix)) = todo.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefix.is_empty() && name == DOT_LODE {
                continue;
            }
            let path = entry.path();
            let ft = entry.file_type()?;
            if stack.is_ignored(&path, ft.is_dir()) {
                continue;
            }
            let depot_path = format!("{prefix}{name}");
            if DepotPath::parse(&depot_path).is_err() {
                out.invalid.push(depot_path);
                continue;
            }
            if ft.is_dir() {
                let has_ignore = path.join(IGNORE_FILE).is_file();
                let (inner, _) = IgnoreStack::enter(&stack, &path, has_ignore);
                todo.push((path, inner, format!("{depot_path}/")));
            } else {
                out.files += 1;
                out.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(out)
}

/// Writes `name` with `text` unless the folder already has it; returns whether it wrote.
fn write_unless_present(root: &Path, name: &str, text: &str) -> CliResult<bool> {
    let path = root.join(name);
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, text)?;
    Ok(true)
}

fn template(name: &str) -> CliResult<&'static Template> {
    templates::find(name).ok_or_else(|| {
        CliError::usage(format!(
            "no template `{name}`; the templates are:{}",
            templates::names()
        ))
    })
}

pub async fn init(ctx: &Ctx, args: InitArgs) -> CliResult<Value> {
    let root = lode_workspace::paths::canonical(&ctx.local(Path::new(".")))?;
    let db = root.join(DOT_LODE).join(lode_workspace::layout::DB_FILE);
    let resuming = db.is_file();
    let mut tpl = template(&args.template)?;

    if args.dry_run {
        let existing = std::fs::read_to_string(root.join(IGNORE_FILE)).ok();
        let p = plan(&root, existing.as_deref().unwrap_or(tpl.ignore))?;
        let mut writes = Vec::new();
        for (name, present) in [
            (IGNORE_FILE, existing.is_some()),
            (
                lode_core::LodeConfig::PATH,
                root.join(lode_core::LodeConfig::PATH).exists(),
            ),
        ] {
            if !present {
                writes.push(name);
            }
        }
        // The template files are committed too.
        let files = p.files + writes.len() as u64;
        ctx.out.line(format!(
            "Would commit {files} files ({}) from {} with template {}{}.",
            human_bytes(p.bytes),
            root.display(),
            tpl.name,
            if writes.is_empty() {
                String::new()
            } else {
                format!(", writing {}", writes.join(" and "))
            }
        ));
        if !p.invalid.is_empty() {
            ctx.out.line(format!(
                "These names are not allowed in a depot and would stop the commit: {}",
                p.invalid.join(", ")
            ));
        }
        return Ok(json!({
            "dry_run": true,
            "root": root,
            "template": tpl.name,
            "files": files,
            "bytes": p.bytes,
            "writes": writes,
            "invalid": p.invalid,
        }));
    }

    let (server, depot) = split_target(ctx, &args.target)?;
    if resuming {
        let ws = Workspace::open(&root)?;
        let Some(name) = ws.db().config(INIT_TEMPLATE)? else {
            return Err(CliError::failed(
                "already_a_workspace",
                format!("{} is already a workspace", root.display()),
            ));
        };
        if ws.db().config(keys::DEPOT)?.as_deref() != Some(depot.as_str()) {
            return Err(CliError::failed(
                "already_a_workspace",
                format!(
                    "{} is being set up for another depot; rerun `lode init` with that one",
                    root.display()
                ),
            ));
        }
        tpl = template(&name)?;
        ctx.out
            .line(format!("Resuming the interrupted init of {depot}."));
    } else {
        let client = ctx.client(&server)?;
        match client.create_depot(&depot).await {
            Ok(_) => ctx.out.line(format!("Created depot {depot}.")),
            // An empty depot made earlier (by an interrupted init, or by hand) is fine.
            Err(lode_client::Error::Api(e)) if e.status == 409 => {
                let head = client.branch(&depot, "main").await?.head_rev;
                if head > 1 {
                    return Err(CliError::failed(
                        "depot_not_empty",
                        format!(
                            "depot {depot} already has revisions; check it out with `lode checkout` instead"
                        ),
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
        let head = client.branch(&depot, "main").await?.head_rev;
        let wrote_ignore = write_unless_present(&root, IGNORE_FILE, tpl.ignore)?;
        let wrote_config = write_unless_present(&root, lode_core::LodeConfig::PATH, tpl.config)?;
        for (wrote, name) in [
            (wrote_ignore, IGNORE_FILE),
            (wrote_config, lode_core::LodeConfig::PATH),
        ] {
            if wrote {
                ctx.out
                    .line(format!("Wrote {name} ({} template).", tpl.name));
            } else {
                ctx.out.line(format!("Kept the existing {name}."));
            }
        }
        let mut ws = Workspace::create(
            &root,
            &WorkspaceInit {
                server_url: server.to_string(),
                depot: depot.clone(),
                branch: "main".to_owned(),
                view: ViewSpec::default(),
            },
        )?;
        ws.db_mut().write(|tx| {
            tx.set_config(keys::SYNCED_REV, &head.to_string())?;
            tx.set_config(INIT_TEMPLATE, tpl.name)
        })?;
    }

    let synced_before = Workspace::open(&root)?
        .db()
        .config_u64(keys::SYNCED_REV)?
        .unwrap_or(0);
    let message = args
        .message
        .clone()
        .unwrap_or_else(|| format!("Initial import ({} template)", tpl.name));
    let result = commit(
        ctx,
        CommitArgs {
            paths: Vec::new(),
            message: Some(message),
            file: None,
            add_all: true,
            keep_locks: false,
            changelist: None,
        },
    )
    .await;
    let committed = match result {
        Ok(v) => v,
        // A rerun after the commit landed but before the init was marked done.
        Err(e) if e.code == "nothing_to_commit" && synced_before > 1 => json!({
            "rev": synced_before,
            "files": Value::Null,
        }),
        Err(e) => return Err(e),
    };
    let mut ws = Workspace::open(&root)?;
    ws.db_mut().write(|tx| tx.remove_config(INIT_TEMPLATE))?;
    ctx.out.line(format!(
        "{} is now the workspace of {server}/{depot}.",
        root.display()
    ));
    Ok(json!({
        "depot": depot,
        "server": server.as_str(),
        "workspace": root,
        "template": tpl.name,
        "rev": committed["rev"],
        "files": committed["files"],
        "resumed": resuming,
    }))
}
