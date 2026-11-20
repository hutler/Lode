//! Merge tools (T164): `lode resolve --tool` and `lode merge-tool list`.
//!
//! A depot names a tool per path pattern (`[merge.tools]` in `.lodeconfig`); what a name runs
//! is set only in the person's own `settings.toml` (`[merge_tools.NAME]`), so checking out a
//! depot never makes Lode start a program the person did not set up. Tools are started only by
//! `lode resolve --tool`, never by a merge or an update.

use std::fs;
use std::path::{Path, PathBuf};

use lode_core::{DepotPath, LodeConfig};
use lode_workspace::db::ConflictRow;
use serde_json::{json, Value};

use crate::cmd::update::{self, local_config, ResolveArgs};
use crate::context::{Ctx, MergeToolSetting};
use crate::error::{OpError, OpResult};
use crate::local::{self, Bases};

/// `lode merge-tool` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum MergeToolCmd {
    /// The tools this depot names and the ones set up in your settings.
    List,
}

/// `lode merge-tool` options.
#[derive(Debug, clap::Args)]
#[command(long_about = "Merge tools for `lode resolve --tool`.
\nA depot names a tool for some files in `.lodeconfig`:
\n  [merge.tools]\n  \"**/*.uasset\" = \"unreal\"
\nand you say what the name runs in settings.toml (next to credentials):
\n  [merge_tools.unreal]\n  command = [\"UnrealEditor.exe\", \"-diff\", \"{base}\", \"{mine}\", \"{theirs}\", \"{merged}\"]\n  trust_exit_code = true
\nWith --json, list prints {\"tools\":[{\"name\",\"patterns\":[..],\"configured\":bool,\"command\":[..]|null}]}.")]
pub struct MergeToolArgs {
    #[command(subcommand)]
    pub command: MergeToolCmd,
}

/// The tool the depot names for `path`, if any.
pub(crate) fn tool_for(config: Option<&LodeConfig>, path: &DepotPath) -> Option<String> {
    config?.merge_tool(path).map(|r| r.tool.clone())
}

fn snippet(name: &str) -> String {
    format!(
        "[merge_tools.{name}]\ncommand = [\"path/to/tool\", \"{{base}}\", \"{{mine}}\", \"{{theirs}}\", \"{{merged}}\"]"
    )
}

fn unknown(ctx: &Ctx, name: &str) -> OpError {
    let file = ctx.config_dir.join("settings.toml");
    OpError::failed(
        "merge_tool_unknown",
        format!(
            "no command is set up for the merge tool `{name}`; add this to {}:\n{}",
            file.display(),
            snippet(name)
        ),
    )
    .with_details(json!({ "tool": name, "settings": file, "snippet": snippet(name) }))
}

/// `name.EXT` → `name.SIDE.EXT`, so tools that look at extensions still recognise the file.
fn side_name(path: &DepotPath, side: &str) -> String {
    let name = path.as_str().rsplit('/').next().unwrap_or("file");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.{side}.{ext}"),
        _ => format!("{name}.{side}"),
    }
}

struct Sides {
    dir: PathBuf,
    base: PathBuf,
    mine: PathBuf,
    theirs: PathBuf,
    merged: PathBuf,
}

/// Writes the three sides and a starting result (the local version) for one conflict.
async fn write_sides(
    ws: &lode_workspace::Workspace,
    bases: &mut Bases<'_>,
    c: &ConflictRow,
    n: usize,
) -> OpResult<Sides> {
    let dir = ws
        .layout()
        .tmp()
        .join("merge")
        .join(format!("{}-{n}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let local_path = ws.local_path(&c.path);
    let base = match c.base_id {
        Some(id) => bases.read(&id).await?,
        None => Vec::new(),
    };
    // Text conflicts keep the local version aside and put markers in the file.
    let mine = match c.mine_id {
        Some(id) => fs::read(local::mine_path(ws, &id))?,
        None => fs::read(&local_path).unwrap_or_default(),
    };
    let beside = local::theirs_path(&local_path, c.theirs_rev);
    let theirs = match fs::read(&beside) {
        Ok(b) => b,
        Err(_) => match c.theirs_id {
            Some(id) => bases.read(&id).await?,
            None => Vec::new(),
        },
    };
    let s = Sides {
        base: dir.join(side_name(&c.path, "BASE")),
        mine: dir.join(side_name(&c.path, "MINE")),
        theirs: dir.join(side_name(&c.path, "THEIRS")),
        merged: dir.join(side_name(&c.path, "MERGED")),
        dir,
    };
    fs::write(&s.base, &base)?;
    fs::write(&s.mine, &mine)?;
    fs::write(&s.theirs, &theirs)?;
    fs::write(&s.merged, &mine)?;
    Ok(s)
}

fn argv(setting: &MergeToolSetting, s: &Sides) -> Vec<String> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    setting
        .command
        .iter()
        .map(|a| {
            a.replace("{base}", &text(&s.base))
                .replace("{mine}", &text(&s.mine))
                .replace("{theirs}", &text(&s.theirs))
                .replace("{merged}", &text(&s.merged))
        })
        .collect()
}

/// Runs `lode resolve --tool [NAME] [paths]`.
pub async fn resolve_with_tool(
    ctx: &Ctx,
    paths: &[PathBuf],
    name: Option<String>,
) -> OpResult<Value> {
    let settings = ctx.settings()?;
    let ws = local::open(ctx)?;
    let named = local::depot_paths(ctx, &ws, paths)?;
    let conflicts: Vec<ConflictRow> = ws
        .db()
        .conflicts()?
        .into_iter()
        .filter(|c| named.is_empty() || named.iter().any(|n| local::under(&c.path, n)))
        .collect();
    let not_conflicted: Vec<String> = named
        .iter()
        .filter(|n| !conflicts.iter().any(|c| local::under(&c.path, n)))
        .map(|n| n.as_str().to_owned())
        .collect();
    if !not_conflicted.is_empty() {
        return Err(local::not_found("not in conflict", &not_conflicted));
    }
    let config = local_config(&ws);
    let mut plan: Vec<(ConflictRow, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for c in conflicts {
        match name.clone().or_else(|| tool_for(config.as_ref(), &c.path)) {
            Some(t) => plan.push((c, t)),
            None => skipped.push(c.path.as_str().to_owned()),
        }
    }
    if plan.is_empty() && !named.is_empty() {
        return Err(OpError::failed(
            "no_merge_tool",
            format!(
                "the depot names no merge tool for {}; give one: `lode resolve --tool NAME`",
                skipped.join(", ")
            ),
        )
        .with_details(json!({ "paths": skipped })));
    }
    // Every tool must be set up before anything runs.
    for (_, t) in &plan {
        if !settings.merge_tools.contains_key(t) {
            return Err(unknown(ctx, t));
        }
    }
    let mut bases = Bases::new(ctx, &ws)?;
    let mut resolved: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for (n, (c, t)) in plan.iter().enumerate() {
        let setting = &settings.merge_tools[t];
        if setting.command.is_empty() {
            return Err(OpError::failed(
                "bad_settings",
                format!("[merge_tools.{t}] command is empty"),
            ));
        }
        let sides = write_sides(&ws, &mut bases, c, n).await?;
        let before = fs::read(&sides.merged)?;
        let full = argv(setting, &sides);
        ctx.out
            .line(format!("Running {t} for {} …", c.path.as_str()));
        let status = std::process::Command::new(&full[0])
            .args(&full[1..])
            .status();
        let path = c.path.as_str().to_owned();
        let status = match status {
            Ok(s) => s,
            Err(e) => {
                failed.push(json!({ "path": path, "tool": t, "reason": format!("cannot start {}: {e}", full[0]) }));
                let _ = fs::remove_dir_all(&sides.dir);
                continue;
            }
        };
        let after = fs::read(&sides.merged).unwrap_or_default();
        let ok = if setting.trust_exit_code.unwrap_or(true) {
            status.success()
        } else {
            after != before
        };
        if !ok {
            let reason = if status.success() {
                "the merged file was not changed".to_owned()
            } else {
                format!("exited with {status}")
            };
            failed.push(json!({ "path": path, "tool": t, "reason": reason }));
            let _ = fs::remove_dir_all(&sides.dir);
            continue;
        }
        {
            let _lock = ws.lock()?;
            let exec = ws.db().file(&c.path)?.is_some_and(|r| r.executable);
            update::write_through_tmp(&ws, &c.path, &after, exec)?;
        }
        Box::pin(update::resolve(
            ctx,
            ResolveArgs {
                paths: vec![ws.local_path(&c.path)],
                merged: true,
                ..Default::default()
            },
        ))
        .await?;
        let _ = fs::remove_dir_all(&sides.dir);
        resolved.push(path);
    }
    for f in &failed {
        ctx.out.warn(format!(
            "{} not resolved: {} ({})",
            f["path"].as_str().unwrap_or_default(),
            f["reason"].as_str().unwrap_or_default(),
            f["tool"].as_str().unwrap_or_default()
        ));
    }
    if !skipped.is_empty() {
        ctx.out.line(format!(
            "No merge tool for: {} (resolve with --mine, --theirs or --merged)",
            skipped.join(", ")
        ));
    }
    Ok(json!({
        "resolved": resolved,
        "using": "tool",
        "failed": failed,
        "skipped": skipped,
    }))
}

/// Runs `lode merge-tool`.
pub async fn merge_tool(ctx: &Ctx, args: MergeToolArgs) -> OpResult<Value> {
    match args.command {
        MergeToolCmd::List => {
            let settings = ctx.settings()?;
            let config = local::open(ctx).ok().and_then(|ws| local_config(&ws));
            let mut names: std::collections::BTreeMap<String, Vec<String>> = settings
                .merge_tools
                .keys()
                .map(|k| (k.clone(), Vec::new()))
                .collect();
            if let Some(c) = &config {
                for r in &c.merge_tools {
                    names
                        .entry(r.tool.clone())
                        .or_default()
                        .push(r.pattern.clone());
                }
            }
            let tools: Vec<Value> = names
                .iter()
                .map(|(name, patterns)| {
                    let set = settings.merge_tools.get(name);
                    ctx.out.line(format!(
                        "{name:<16} {}  {}",
                        if set.is_some() { "set up " } else { "missing" },
                        if patterns.is_empty() {
                            "(not used by this depot)".to_owned()
                        } else {
                            patterns.join(", ")
                        }
                    ));
                    json!({
                        "name": name,
                        "patterns": patterns,
                        "configured": set.is_some(),
                        "command": set.map(|s| s.command.clone()),
                    })
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
    }
}
