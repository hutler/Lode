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
use crate::error::{OpError, OpResult};

/// `lode mcp` options.
#[derive(Debug, clap::Args)]
#[command(
    long_about = "Serve the workspace to a coding agent over MCP on stdin and stdout.
\nRun it from the workspace or with -C. Tools: status, diff, add, remove, move, revert, update,
lock, unlock, shelve, unshelve, log, read_file_at_revision, branches, locks, conflicts, switch,
merge, resolve, changelist, wait_for_events, and commit (only with a token that has the
`write` scope). Each answers the JSON of the matching command with --json.
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
pub async fn serve(ctx: &Ctx, args: McpArgs) -> OpResult<Value> {
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
    // A second worker for `wait_for_events`, so a long wait never holds up other tools.
    let waiter = Worker {
        root: worker.root.clone(),
        g: worker.g.clone(),
    };
    let server = Server {
        jobs: worker.spawn()?,
        waits: waiter.spawn()?,
        can_write: Arc::new(OnceCell::new()),
    };
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| OpError::failed("mcp", format!("cannot start the MCP server: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| OpError::failed("mcp", format!("the MCP server stopped: {e}")))?;
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
    reply: oneshot::Sender<OpResult<Value>>,
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
    /// The worker that runs `wait_for_events`.
    waits: mpsc::UnboundedSender<Job>,
    /// Whether the stored token may commit, asked once.
    can_write: Arc<OnceCell<bool>>,
}

impl Server {
    async fn call(&self, name: &str, input: Value) -> OpResult<Value> {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            name: name.to_owned(),
            input,
            reply,
        };
        let gone = || OpError::failed("mcp", "the command worker stopped");
        let queue = if name == "wait_for_events" {
            &self.waits
        } else {
            &self.jobs
        };
        queue.send(job).map_err(|_| gone())?;
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
    fn spawn(self) -> OpResult<mpsc::UnboundedSender<Job>> {
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
    fn ctx(&self) -> OpResult<Ctx> {
        Ctx::new(
            Globals {
                dir: Some(self.root.clone()),
                ..self.g.clone()
            },
            std::sync::Arc::new(crate::term::Protocol),
        )
    }

    async fn run(&self, name: &str, input: Value) -> OpResult<Value> {
        let ctx = self.ctx()?;
        let root = &self.root;
        match name {
            SCOPES_JOB => cmd::login::whoami(&ctx).await,
            "status" => {
                let i: Status = parse(input)?;
                let paths = confine_all(root, &i.paths)?;
                cmd::status::status(
                    &ctx,
                    cmd::status::StatusArgs {
                        paths,
                        unchanged: false,
                        remote: i.remote,
                    },
                )
                .await
            }
            "branches" => {
                let i: Branches = parse(input)?;
                let args = cmd::branch::BranchArgs {
                    command: cmd::branch::BranchCmd::List { all: i.archived },
                };
                cmd::branch::branch(&ctx, args).await
            }
            "locks" => {
                let i: Locks = parse(input)?;
                let mut paths = confine_all(root, &i.paths)?;
                if paths.len() > 1 {
                    return Err(OpError::usage("`locks` takes at most one path"));
                }
                let args = cmd::locks::LocksArgs {
                    path: paths.pop(),
                    mine: i.mine,
                };
                cmd::locks::locks(&ctx, args).await
            }
            "conflicts" => {
                let i: Conflicts = parse(input)?;
                conflicts(&ctx, root, i).await
            }
            "switch" => {
                let i: Switch = parse(input)?;
                let args = cmd::branch::SwitchArgs {
                    branch: i.branch,
                    rev: i.rev,
                };
                cmd::branch::switch(&ctx, args).await
            }
            "merge" => {
                let i: Merge = parse(input)?;
                let args = cmd::merge::MergeArgs {
                    source: i.branch,
                    rev: i.rev,
                    force_flow: false,
                };
                cmd::merge::merge(&ctx, args).await
            }
            "resolve" => {
                let i: Resolve = parse(input)?;
                resolve(&ctx, root, i).await
            }
            "changelist" => {
                let i: Changelist = parse(input)?;
                changelist(&ctx, root, i).await
            }
            "wait_for_events" => {
                let i: WaitForEvents = parse(input)?;
                let wait = i
                    .timeout_s
                    .unwrap_or(DEFAULT_WAIT_SECONDS)
                    .min(MAX_WAIT_SECONDS);
                let args = cmd::events::EventsArgs {
                    depot: i.depot,
                    after: i.after,
                    kinds: i.kinds.map(|k| k.join(",")),
                    wait: Some(wait),
                };
                cmd::events::events(&ctx, args).await
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
                    return Err(OpError::usage("name the paths to revert, or pass `all`"));
                }
                if !paths.is_empty() && i.all {
                    return Err(OpError::usage("pass `paths` or `all`, not both"));
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
                    no_verify: false,
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
                    force_flow: false,
                    override_reason: None,
                    no_verify: false,
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
                    before: None,
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
                    output: None,
                    source: no_source(),
                };
                cmd::history::cat(&ctx, args).await
            }
            other => Err(OpError::usage(format!("unknown tool `{other}`"))),
        }
    }
}

/// Content of one side of a conflict for the agent: text when it is UTF-8 and was read,
/// otherwise only its size (`cut` says it was over the limit).
fn side(c: Option<cmd::diff::Content>, text: bool) -> Value {
    match c {
        None => Value::Null,
        Some(c) => {
            let mut v = json!({ "size": c.size });
            match c.bytes {
                Some(b) if text => match String::from_utf8(b) {
                    Ok(t) => v["text"] = json!(t),
                    Err(_) => v["binary"] = json!(true),
                },
                Some(_) => v["binary"] = json!(true),
                None => v["cut"] = json!(true),
            }
            v
        }
    }
}

/// Largest side `conflicts` returns, as `read_file` on the server.
const MAX_SIDE_BYTES: u64 = 256 * 1024;

async fn conflicts(ctx: &Ctx, root: &Path, i: Conflicts) -> OpResult<Value> {
    let limit = i.max_bytes.unwrap_or(MAX_SIDE_BYTES).min(MAX_SIDE_BYTES);
    if let Some(p) = i.path {
        let path = confine(root, &p)?;
        let p = path.to_string_lossy().replace('\\', "/");
        let c = cmd::conflict::conflict_sides(ctx, &p, limit).await?;
        return Ok(json!({
            "path": c.path,
            "theirs_rev": c.theirs_rev,
            "text": c.text,
            "base": side(c.base, c.text),
            "mine": side(c.mine, c.text),
            "theirs": side(c.theirs, c.text),
            "result": side(c.result, c.text),
            "theirs_file": c.theirs_file,
        }));
    }
    let status = cmd::status::status(ctx, cmd::status::StatusArgs::default()).await?;
    let mut out = Vec::new();
    for f in status["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if f["status"] != "conflicted" {
            continue;
        }
        let Some(p) = f["path"].as_str() else {
            continue;
        };
        let c = cmd::conflict::conflict_sides(ctx, p, 0).await?;
        out.push(json!({ "path": c.path, "text": c.text, "theirs_rev": c.theirs_rev }));
    }
    Ok(json!({ "conflicts": out }))
}

async fn resolve(ctx: &Ctx, root: &Path, i: Resolve) -> OpResult<Value> {
    let path = confine(root, &i.path)?;
    let content = match (i.text, i.base64) {
        (Some(_), Some(_)) => return Err(OpError::usage("give `text` or `base64`, not both")),
        (Some(t), None) => Some(t.into_bytes()),
        (None, Some(b)) => {
            use base64::Engine as _;
            Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b.as_bytes())
                    .map_err(|e| OpError::usage(format!("invalid base64: {e}")))?,
            )
        }
        (None, None) => None,
    };
    match (i.take.as_deref(), content) {
        (Some(_), Some(_)) => Err(OpError::usage("give `take` or new content, not both")),
        (None, None) => Err(OpError::usage(
            "give `take` (`mine`, `theirs` or `as_is`) or the new content",
        )),
        (None, Some(bytes)) => {
            if bytes.len() as u64 > MAX_RESOLVE_BYTES {
                return Err(OpError::new(
                    crate::error::Exit::Failed,
                    "too_large",
                    format!("at most {MAX_RESOLVE_BYTES} bytes of content per call"),
                ));
            }
            let p = path.to_string_lossy().replace('\\', "/");
            cmd::conflict::resolve_with(ctx, &p, &bytes).await
        }
        (Some(take), None) => {
            let (mine, theirs, merged) = match take {
                "mine" => (true, false, false),
                "theirs" => (false, true, false),
                "as_is" => (false, false, true),
                other => {
                    return Err(OpError::usage(format!(
                        "`take` is `mine`, `theirs` or `as_is`, not `{other}`"
                    )))
                }
            };
            let args = cmd::update::ResolveArgs {
                paths: vec![path],
                mine,
                theirs,
                merged,
                tool: None,
            };
            cmd::update::resolve(ctx, args).await
        }
    }
}

/// Most content `resolve` takes in one call, as `create_shelf`'s inline limit.
const MAX_RESOLVE_BYTES: u64 = 16 * 1024 * 1024;

async fn changelist(ctx: &Ctx, root: &Path, i: Changelist) -> OpResult<Value> {
    use cmd::changelist::ChangelistCommand as C;
    let name = || {
        i.name
            .clone()
            .ok_or_else(|| OpError::usage(format!("`{}` needs `name`", i.action)))
    };
    let command = match i.action.as_str() {
        "new" => C::New {
            name: name()?,
            description: i.description.clone().unwrap_or_default(),
        },
        "list" => C::List,
        "move" => C::Move {
            name: name()?,
            paths: confine_some(root, &i.paths)?,
        },
        "delete" => C::Delete { name: name()? },
        other => {
            return Err(OpError::usage(format!(
                "`action` is `new`, `list`, `move` or `delete`, not `{other}`"
            )))
        }
    };
    cmd::changelist::changelist(ctx, cmd::changelist::ChangelistArgs { command }).await
}

fn no_source() -> cmd::history::Source {
    cmd::history::Source {
        depot: None,
        branch: None,
    }
}

fn parse<T: DeserializeOwned>(input: Value) -> OpResult<T> {
    serde_json::from_value(input).map_err(|e| OpError::usage(format!("bad input: {e}")))
}

/// A workspace-relative path from tool input, refused if it is absolute or climbs out of
/// the workspace. The result is relative to the root, `.` for the root itself.
fn confine(root: &Path, p: &str) -> OpResult<PathBuf> {
    let outside = || {
        OpError::new(
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

fn confine_all(root: &Path, paths: &[String]) -> OpResult<Vec<PathBuf>> {
    paths.iter().map(|p| confine(root, p)).collect()
}

/// Like [`confine_all`], for tools that need at least one path.
fn confine_some(root: &Path, paths: &[String]) -> OpResult<Vec<PathBuf>> {
    if paths.is_empty() {
        return Err(OpError::usage("`paths` is empty"));
    }
    confine_all(root, paths)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    remote: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Branches {
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Locks {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    mine: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Conflicts {
    path: Option<String>,
    max_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Switch {
    branch: String,
    rev: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Merge {
    branch: String,
    rev: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    path: String,
    take: Option<String>,
    text: Option<String>,
    base64: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Changelist {
    action: String,
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForEvents {
    depot: Option<String>,
    after: Option<i64>,
    kinds: Option<Vec<String>>,
    timeout_s: Option<u64>,
}

/// Default and longest wait of `wait_for_events`, below common MCP client tool timeouts.
const DEFAULT_WAIT_SECONDS: u64 = 25;
const MAX_WAIT_SECONDS: u64 = 50;

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
             conflicted, with the workspace's branch and revision, and a `summary` of counts \
             (changed, needs_lock, conflicted, untracked, pending_merge). With `remote`, also \
             asks the server which files are newer at the branch head and who holds their \
             locks. Call it with `remote` at the start of a task and before editing files \
             that need a lock.",
            schema(
                json!({
                    "paths": paths_prop("Only these files or directories"),
                    "remote": { "type": "boolean", "description": "Also ask the server about newer versions and locks" }
                }),
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
        Tool::new(
            "branches",
            "The depot's branches with their head revisions, and which one this workspace is \
             on. With `archived`, archived branches too.",
            schema(
                json!({ "archived": { "type": "boolean", "description": "Include archived branches" } }),
                &[],
            ),
        ),
        Tool::new(
            "locks",
            "Locks on the workspace's branch: path, owner, workspace, comment and whether it \
             is yours. Check before editing files that need a lock.",
            schema(
                json!({
                    "paths": paths_prop("Only locks under this one file or directory"),
                    "mine": { "type": "boolean", "description": "Only your own locks" }
                }),
                &[],
            ),
        ),
        Tool::new(
            "conflicts",
            "Without `path`, the files left in conflict by an update, switch, merge or \
             unshelve. With `path`, that file's three sides: `base` (common ancestor), `mine`, \
             `theirs` and `result` (the file now, with conflict markers for text). Text sides \
             come as `text`, cut at `max_bytes` (at most 256 KiB, then `cut`); binary sides \
             give only their size, and `theirs_file` names the incoming copy on disk.",
            schema(
                json!({
                    "path": { "type": "string", "description": "A conflicted file" },
                    "max_bytes": { "type": "integer", "minimum": 0, "description": "Largest side to return (default and most: 262144)" }
                }),
                &[],
            ),
        ),
        Tool::new(
            "switch",
            "Move the workspace to another branch (its head, or `rev`), carrying local \
             changes over. Conflicts are left for `conflicts` and `resolve`. Locks stay on \
             the old branch.",
            schema(
                json!({
                    "branch": { "type": "string", "description": "Branch to switch to" },
                    "rev": { "type": "integer", "minimum": 1, "description": "Revision of that branch (default: head)" }
                }),
                &["branch"],
            ),
        ),
        Tool::new(
            "merge",
            "Merge another branch (or one revision of it) into the workspace. Files that \
             cannot be merged are left in conflict. The merge is recorded by the next shelve \
             or commit; shelve it so a person reviews the result.",
            schema(
                json!({
                    "branch": { "type": "string", "description": "Branch to merge from" },
                    "rev": { "type": "integer", "minimum": 1, "description": "Only this revision of it" }
                }),
                &["branch"],
            ),
        ),
        Tool::new(
            "resolve",
            "Settle one conflicted file: `take` `mine`, `theirs` or `as_is` (the file as it \
             is now on disk), or give its resolved content as `text` or `base64` (at most \
             16 MiB). Then shelve, describing each choice, for a person to review.",
            schema(
                json!({
                    "path": { "type": "string", "description": "The conflicted file" },
                    "take": { "type": "string", "enum": ["mine", "theirs", "as_is"] },
                    "text": { "type": "string", "description": "Resolved content as UTF-8 text" },
                    "base64": { "type": "string", "description": "Resolved content as base64" }
                }),
                &["path"],
            ),
        ),
        Tool::new(
            "changelist",
            "Named groups of pending changes in this workspace: `new` (with `name` and \
             `description`), `list`, `move` (`name` and `paths`; `default` takes files out of \
             any) or `delete` (`name`). `shelve` and `commit` take a changelist.",
            schema(
                json!({
                    "action": { "type": "string", "enum": ["new", "list", "move", "delete"] },
                    "name": { "type": "string", "description": "The changelist" },
                    "description": { "type": "string", "description": "For `new`: what the changes are for" },
                    "paths": paths_prop("For `move`: files to put into the changelist")
                }),
                &["action"],
            ),
        ),
        Tool::new(
            "wait_for_events",
            "Wait for something to happen in the workspace's depot: a new revision, a branch \
             created or archived, a lock taken or released, a shelf created, updated, \
             submitted or deleted, or a check result. Answers at once when events after \
             `after` exist, otherwise waits up to `timeout_s` seconds (default 25, at most \
             50). Without `after` it answers no events and the current cursor. Keep `next` \
             and pass it as `after` next time. Events carry ids only: read details with \
             `log`, `locks` or `status` with `remote`. If `truncated` is true, events were \
             dropped (they are kept for 7 days): read the history again.",
            schema(
                json!({
                    "after": { "type": "integer", "minimum": 0, "description": "The `next` of the previous call" },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string", "enum": lode_proto::feed::EVENTS },
                        "description": "Only these events (default: all)"
                    },
                    "timeout_s": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECONDS, "description": "Seconds to wait (default 25)" },
                    "depot": { "type": "string", "description": "Another depot on the same server (default: the workspace's)" }
                }),
                &[],
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
            let e = OpError::new(
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
