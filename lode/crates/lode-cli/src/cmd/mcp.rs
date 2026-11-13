//! `lode mcp`: a local MCP server over stdio for a coding agent working in one workspace.
//!
//! Each tool runs the same function as the matching command with `--json` and answers its
//! JSON. Paths in tool input are workspace-relative and may not leave the workspace. Stdout
//! carries only protocol messages: commands print nothing there with `--json`, and
//! diagnostics go to stderr.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use lode_proto::commit::AgentProvenance;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, OnceCell};

use crate::cmd;
use crate::context::{Ctx, Globals};
use crate::error::{CliError, CliResult};

/// `lode mcp` options.
#[derive(Debug, clap::Args)]
#[command(
    long_about = "Serve the workspace to a coding agent over MCP on stdin and stdout.
\nRun it from the workspace or with -C. Tools: status, diff, add, remove, move, revert, update,
lock, unlock, shelve, unshelve, commit (only with a token that has the `write` scope), log and
read_file_at_revision. Each answers the JSON of the matching command with --json.
\nThe --agent-* options (or LODE_AGENT_MODEL, LODE_AGENT_SESSION, LODE_AGENT_TOOL) are recorded
as provenance on the shelves and commits made through this server."
)]
pub struct McpArgs {
    /// Model name to record, such as the agent's model id.
    #[arg(long, value_name = "NAME")]
    pub agent_model: Option<String>,
    /// Session id to record.
    #[arg(long, value_name = "ID")]
    pub agent_session: Option<String>,
    /// Tool name to record, such as the agent's product name.
    #[arg(long, value_name = "NAME")]
    pub agent_tool: Option<String>,
}

const INSTRUCTIONS: &str = "Tools for one Lode workspace (a checkout of a depot branch). Paths \
are relative to the workspace root and use `/`. Edit files on disk with your own tools, then \
use `status` and `diff` to review, `add`/`remove`/`move` to record new, deleted and moved \
files, and `shelve` to propose the changes on the server for a person to review. `commit` is \
offered only when your token may write. Errors carry a stable `code`.";

/// Runs the server until the client closes stdin.
pub async fn serve(ctx: &Ctx, args: McpArgs) -> CliResult<Value> {
    let ws = ctx.workspace()?;
    let root = ws.root().to_path_buf();
    drop(ws);
    let worker = Worker {
        root,
        g: Globals {
            json: true,
            dir: None,
            agent: agent(&args),
            ..ctx.g.clone()
        },
    };
    let server = Server {
        jobs: worker.spawn()?,
        can_write: Arc::new(OnceCell::new()),
    };
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| CliError::failed("mcp", format!("cannot start the MCP server: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| CliError::failed("mcp", format!("the MCP server stopped: {e}")))?;
    // Nothing more may go to stdout: it belonged to the protocol.
    std::process::exit(0)
}

fn agent(args: &McpArgs) -> Option<AgentProvenance> {
    let pick = |flag: &Option<String>, env: &str| {
        flag.clone()
            .or_else(|| std::env::var(env).ok())
            .filter(|v| !v.is_empty())
    };
    let mut a = BTreeMap::new();
    for (key, flag, env) in [
        ("model", &args.agent_model, "LODE_AGENT_MODEL"),
        ("session", &args.agent_session, "LODE_AGENT_SESSION"),
        ("tool", &args.agent_tool, "LODE_AGENT_TOOL"),
    ] {
        if let Some(v) = pick(flag, env) {
            a.insert(key.to_owned(), v);
        }
    }
    (!a.is_empty()).then_some(a)
}

/// One tool call for the worker, with where to send the answer.
struct Job {
    name: String,
    input: Value,
    reply: oneshot::Sender<CliResult<Value>>,
}

/// Name of the job that answers the stored token's scopes.
const SCOPES_JOB: &str = "\u{0}scopes";

/// Runs the commands, one at a time, on a thread of its own: they share the workspace, and
/// its database handle cannot move between threads.
struct Worker {
    root: PathBuf,
    g: Globals,
}

#[derive(Clone)]
struct Server {
    jobs: mpsc::UnboundedSender<Job>,
    /// Whether the stored token may commit, asked once.
    can_write: Arc<OnceCell<bool>>,
}

impl Server {
    async fn call(&self, name: &str, input: Value) -> CliResult<Value> {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            name: name.to_owned(),
            input,
            reply,
        };
        let gone = || CliError::failed("mcp", "the command worker stopped");
        self.jobs.send(job).map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    /// Whether the stored token may commit. Unknown (server unreachable) counts as no.
    async fn can_write(&self) -> bool {
        *self
            .can_write
            .get_or_init(|| async {
                self.call(SCOPES_JOB, Value::Null).await.is_ok_and(|v| {
                    v["scopes"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|s| s == "write"))
                })
            })
            .await
    }
}

impl Worker {
    fn spawn(self) -> CliResult<mpsc::UnboundedSender<Job>> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("lode-mcp-worker".into())
            .spawn(move || {
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        let answer = self.run(&job.name, job.input).await;
                        let _ = job.reply.send(answer);
                    }
                });
            })?;
        Ok(tx)
    }

    /// A context like the one `lode -C <root> --json` would have.
    fn ctx(&self) -> CliResult<Ctx> {
        Ctx::new(Globals {
            dir: Some(self.root.clone()),
            ..self.g.clone()
        })
    }

    async fn run(&self, name: &str, input: Value) -> CliResult<Value> {
        let ctx = self.ctx()?;
        let root = &self.root;
        match name {
            SCOPES_JOB => cmd::login::whoami(&ctx).await,
            "status" => {
                let i: Paths = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                cmd::status::status(&ctx, cmd::status::StatusArgs { paths }).await
            }
            "diff" => {
                let i: Paths = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                cmd::diff::diff(&ctx, cmd::diff::DiffArgs { paths, stat: false }).await
            }
            "add" => {
                let i: Add = parse(input)?;
                let paths = confine_some(root, &i.paths)?;
                let args = cmd::files::AddArgs {
                    paths,
                    force: false,
                    changelist: i.changelist,
                };
                cmd::files::add(&ctx, args).await
            }
            "remove" => {
                let i: Remove = parse(input)?;
                let paths = confine_some(root, &i.paths)?;
                let args = cmd::files::RmArgs {
                    paths,
                    keep: i.keep,
                    force: false,
                    changelist: None,
                };
                cmd::files::rm(&ctx, args).await
            }
            "move" => {
                let i: Move = parse(input)?;
                let paths = vec![confine(root, &i.from)?, confine(root, &i.to)?];
                let args = cmd::files::MoveArgs {
                    paths,
                    after: i.after,
                    changelist: None,
                };
                cmd::files::mv(&ctx, args).await
            }
            "revert" => {
                let i: Revert = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                if paths.is_empty() && !i.all {
                    return Err(CliError::usage("name the paths to revert, or pass `all`"));
                }
                if !paths.is_empty() && i.all {
                    return Err(CliError::usage("pass `paths` or `all`, not both"));
                }
                let args = cmd::revert::RevertArgs {
                    paths,
                    all: i.all,
                    merge: false,
                };
                cmd::revert::revert(&ctx, args).await
            }
            "update" => {
                let i: Update = parse(input)?;
                cmd::update::update(&ctx, cmd::update::UpdateArgs { rev: i.rev }).await
            }
            "lock" => {
                let i: Lock = parse(input)?;
                let paths = confine_some(root, &i.paths)?;
                let args = cmd::locks::LockArgs {
                    paths,
                    comment: i.comment,
                };
                cmd::locks::lock(&ctx, args).await
            }
            "unlock" => {
                let i: Unlock = parse(input)?;
                let paths = confine_some(root, &i.paths)?;
                let args = cmd::locks::UnlockArgs {
                    paths,
                    force: i.force,
                };
                cmd::locks::unlock(&ctx, args).await
            }
            "shelve" => {
                let i: Shelve = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                let args = cmd::shelve::ShelveArgs {
                    paths,
                    message: Some(i.description),
                    changelist: i.changelist,
                    keep: i.keep,
                    update: i.update,
                };
                cmd::shelve::shelve(&ctx, args).await
            }
            "unshelve" => {
                let i: Unshelve = parse(input)?;
                cmd::shelve::unshelve(&ctx, cmd::shelve::UnshelveArgs { id: i.id }).await
            }
            "commit" => {
                let i: Commit = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                let args = cmd::commit::CommitArgs {
                    paths,
                    message: Some(i.message),
                    file: None,
                    add_all: i.add_all,
                    keep_locks: i.keep_locks,
                    changelist: None,
                };
                cmd::commit::commit(&ctx, args).await
            }
            "log" => {
                let i: Log = parse(input)?;
                let path = i
                    .path
                    .as_deref()
                    .map(|p| confine(root, p))
                    .transpose()?
                    .map(|p| p.to_string_lossy().replace('\\', "/"));
                let args = cmd::history::LogArgs {
                    path,
                    max: i.limit.unwrap_or(20).clamp(1, 1000),
                    follow: i.follow,
                    verbose: i.verbose,
                    source: no_source(),
                };
                cmd::history::log(&ctx, args).await
            }
            "read_file_at_revision" => {
                let i: ReadAt = parse(input)?;
                let path = confine(root, &i.path)?;
                let mut arg = path.to_string_lossy().replace('\\', "/");
                if let Some(r) = i.rev {
                    arg = format!("{arg}@{r}");
                }
                let args = cmd::history::CatArgs {
                    path: arg,
                    source: no_source(),
                };
                cmd::history::cat(&ctx, args).await
            }
            other => Err(CliError::usage(format!("unknown tool `{other}`"))),
        }
    }
}

fn no_source() -> cmd::history::Source {
    cmd::history::Source {
        depot: None,
        branch: None,
    }
}

fn parse<T: DeserializeOwned>(input: Value) -> CliResult<T> {
    serde_json::from_value(input).map_err(|e| CliError::usage(format!("bad input: {e}")))
}

/// A workspace-relative path from tool input, refused if it is absolute or climbs out of
/// the workspace. The result is relative to the root, `.` for the root itself.
fn confine(root: &Path, p: &str) -> CliResult<PathBuf> {
    let outside = || {
        CliError::new(
            crate::error::Exit::Failed,
            "outside_workspace",
            format!("`{p}` is outside the workspace; use a path relative to its root"),
        )
    };
    // Absolute in either convention, whatever this system is.
    let b = p.as_bytes();
    if p.starts_with('/')
        || p.starts_with('\\')
        || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
    {
        return Err(outside());
    }
    let mut out = PathBuf::new();
    for c in Path::new(&p.replace('\\', "/")).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(outside());
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(outside()),
        }
    }
    // Following a symbolic link out of the workspace counts as leaving it too.
    if let Ok(real) = lode_workspace::paths::canonical(&root.join(&out)) {
        if !real.starts_with(root) {
            return Err(outside());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Ok(out)
}

fn confine_all(root: &Path, paths: &[String]) -> CliResult<Vec<PathBuf>> {
    paths.iter().map(|p| confine(root, p)).collect()
}

/// Like [`confine_all`], for tools that need at least one path.
fn confine_some(root: &Path, paths: &[String]) -> CliResult<Vec<PathBuf>> {
    if paths.is_empty() {
        return Err(CliError::usage("`paths` is empty"));
    }
    confine_all(root, paths)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paths {
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Add {
    paths: Vec<String>,
    changelist: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    paths: Vec<String>,
    #[serde(default)]
    keep: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    from: String,
    to: String,
    #[serde(default)]
    after: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revert {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    rev: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    paths: Vec<String>,
    comment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unlock {
    paths: Vec<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shelve {
    description: String,
    #[serde(default)]
    paths: Vec<String>,
    changelist: Option<String>,
    #[serde(default)]
    keep: bool,
    update: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unshelve {
    id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    message: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    add_all: bool,
    #[serde(default)]
    keep_locks: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Log {
    path: Option<String>,
    limit: Option<u32>,
    #[serde(default)]
    follow: bool,
    #[serde(default)]
    verbose: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadAt {
    path: String,
    rev: Option<u64>,
}

fn schema(properties: Value, required: &[&str]) -> Arc<serde_json::Map<String, Value>> {
    let v = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    });
    match v {
        Value::Object(m) => Arc::new(m),
        _ => Arc::new(serde_json::Map::new()),
    }
}

fn paths_prop(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": what })
}

/// Every tool, in the order they are listed. `commit` is last so a list without it ends at
/// the read-only history tools.
fn tools(can_write: bool) -> Vec<Tool> {
    let mut t = vec![
        Tool::new(
            "status",
            "Changed files in the workspace: added, modified, deleted, moved, untracked and \
             conflicted, with the workspace's branch and revision. Optionally only below some \
             paths.",
            schema(
                json!({ "paths": paths_prop("Only these files or directories") }),
                &[],
            ),
        ),
        Tool::new(
            "diff",
            "Unified diff of local changes against the revision the workspace is at, per \
             file. Optionally only some paths.",
            schema(
                json!({ "paths": paths_prop("Only these files or directories") }),
                &[],
            ),
        ),
        Tool::new(
            "add",
            "Start tracking new files (or directories, recursively) so they are part of the \
             next shelve or commit.",
            schema(
                json!({
                    "paths": paths_prop("Files or directories to add"),
                    "changelist": { "type": "string", "description": "Put them in this changelist" }
                }),
                &["paths"],
            ),
        ),
        Tool::new(
            "remove",
            "Delete tracked files from the workspace and mark them deleted. With `keep`, \
             only stop tracking them and leave them on disk.",
            schema(
                json!({
                    "paths": paths_prop("Files to remove"),
                    "keep": { "type": "boolean", "description": "Leave the files on disk" }
                }),
                &["paths"],
            ),
        ),
        Tool::new(
            "move",
            "Move or rename a file or directory, keeping its history. With `after`, record a \
             move you already made on disk.",
            schema(
                json!({
                    "from": { "type": "string", "description": "Current path" },
                    "to": { "type": "string", "description": "New path" },
                    "after": { "type": "boolean", "description": "The move was already made on disk" }
                }),
                &["from", "to"],
            ),
        ),
        Tool::new(
            "revert",
            "Discard local changes to some files (or all with `all`), restoring the \
             workspace's revision. This cannot be undone.",
            schema(
                json!({
                    "paths": paths_prop("Files or directories to revert"),
                    "all": { "type": "boolean", "description": "Revert every change" }
                }),
                &[],
            ),
        ),
        Tool::new(
            "update",
            "Bring the workspace to the branch head (or `rev`), merging local changes. \
             Conflicts are reported and left for a person to resolve.",
            schema(
                json!({ "rev": { "type": "integer", "minimum": 1, "description": "Revision (default: head)" } }),
                &[],
            ),
        ),
        Tool::new(
            "lock",
            "Take exclusive locks on files so nobody else can commit them, for binary files \
             that cannot be merged.",
            schema(
                json!({
                    "paths": paths_prop("Files to lock"),
                    "comment": { "type": "string", "description": "Why, shown to others" }
                }),
                &["paths"],
            ),
        ),
        Tool::new(
            "unlock",
            "Release locks you hold on files.",
            schema(
                json!({
                    "paths": paths_prop("Files to unlock"),
                    "force": { "type": "boolean", "description": "Release someone else's lock (needs admin rights)" }
                }),
                &["paths"],
            ),
        ),
        Tool::new(
            "shelve",
            "Put local changes on a shelf on the server, for a person to review and submit. \
             Afterwards the changes are taken out of the workspace unless `keep`. With \
             `update`, replace the content of an existing shelf of yours.",
            schema(
                json!({
                    "description": { "type": "string", "description": "What the changes do and why" },
                    "paths": paths_prop("Only these files or directories (default: all changes)"),
                    "changelist": { "type": "string", "description": "Only this changelist" },
                    "keep": { "type": "boolean", "description": "Keep the changes in the workspace" },
                    "update": { "type": "integer", "minimum": 1, "description": "Id of a shelf to replace" }
                }),
                &["description"],
            ),
        ),
        Tool::new(
            "unshelve",
            "Apply a shelf's changes to the workspace, merging with local changes.",
            schema(
                json!({ "id": { "type": "integer", "minimum": 1, "description": "Shelf id" } }),
                &["id"],
            ),
        ),
        Tool::new(
            "log",
            "Revision history of the workspace's branch, newest first, optionally for one \
             path. With `verbose`, the changed paths of each revision.",
            schema(
                json!({
                    "path": { "type": "string", "description": "Only revisions that changed this path" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Most revisions (default 20)" },
                    "follow": { "type": "boolean", "description": "Follow a file through copies and renames" },
                    "verbose": { "type": "boolean", "description": "Include changed paths" }
                }),
                &[],
            ),
        ),
        Tool::new(
            "read_file_at_revision",
            "Read a file as it is on the server at a revision (default: the workspace's \
             revision), regardless of local changes. Text comes as `content`, other files as \
             `content_base64`.",
            schema(
                json!({
                    "path": { "type": "string", "description": "File path" },
                    "rev": { "type": "integer", "minimum": 1, "description": "Revision" }
                }),
                &["path"],
            ),
        ),
    ];
    if can_write {
        t.push(Tool::new(
            "commit",
            "Send local changes to the server as a new revision on the workspace's branch. \
             Prefer `shelve` unless you were asked to commit.",
            schema(
                json!({
                    "message": { "type": "string", "description": "Commit message" },
                    "paths": paths_prop("Only these files or directories (default: all changes)"),
                    "add_all": { "type": "boolean", "description": "Also add untracked files" },
                    "keep_locks": { "type": "boolean", "description": "Keep your locks on the committed files" }
                }),
                &["message"],
            ),
        ));
    }
    t
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("lode", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _rc: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools(
            self.can_write().await,
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools(true).into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            ));
        }
        if request.name == "commit" && !self.can_write().await {
            let e = CliError::new(
                crate::error::Exit::Auth,
                "permission_denied",
                "this token lacks the `write` scope; shelve the changes instead",
            );
            return Ok(CallToolResult::structured_error(e.to_json()["error"].clone()).into());
        }
        let input = Value::Object(request.arguments.unwrap_or_default());
        let result = match self.call(&request.name, input).await {
            Ok(v) => CallToolResult::structured(v),
            Err(e) => CallToolResult::structured_error(e.to_json()["error"].clone()),
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside() {
        let root = lode_workspace::paths::canonical(&std::env::temp_dir()).unwrap();
        let ok = |p: &str| confine(&root, p).unwrap();
        assert_eq!(ok("a/b.txt"), PathBuf::from("a/b.txt"));
        assert_eq!(ok("a/../b"), PathBuf::from("b"));
        assert_eq!(ok("./"), PathBuf::from("."));
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../../x",
            "C:/x",
            "c:x",
            "\\\\server\\x",
        ] {
            assert_eq!(
                confine(&root, bad).unwrap_err().code,
                "outside_workspace",
                "{bad}"
            );
        }
    }
}
