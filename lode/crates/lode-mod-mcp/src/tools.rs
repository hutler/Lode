//! The tools that read core data: who am I, depots, branches, history, directories, files and
//! diffs. Each goes through the `ModuleCtx` services, so scopes and access rules apply exactly
//! as they do to the HTTP API.

use std::future::Future;
use std::sync::Arc;

use lode_core::{DepotPath, EntryKind, ObjectId};
use lode_meta::{DepotRow, PrincipalKind};
use lode_proto::read::{RevisionListQuery, RevisionView};
use lode_server::auth::SCOPE_READ;
use lode_server::module::{ModuleCtx, ToolDef};
use lode_server::{ApiError, ApiResult, Caller};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// Default and largest output of `read_file`, in bytes.
const READ_FILE_DEFAULT: usize = 256 * 1024;
const READ_FILE_MAX: usize = 1024 * 1024;
/// Default and largest text of `diff`, in bytes.
const DIFF_DEFAULT: usize = 256 * 1024;
const DIFF_MAX: usize = 1024 * 1024;
/// Files larger than this are listed by `diff` but not compared: comparing two very
/// different large files costs time that grows with the product of their sizes.
const DIFF_FILE_MAX: u64 = 256 * 1024;
/// Changed files `diff` lists at most.
const DIFF_FILES_MAX: usize = 1000;
/// Bytes looked at to tell binary content from text.
const SNIFF: usize = 8 * 1024;
/// Revisions per page of `log` and `search_history`.
const PAGE_DEFAULT: u32 = 20;
const PAGE_MAX: u32 = 100;
/// Revisions `search_history` looks through per call before it hands back a cursor.
const SEARCH_SCAN: usize = 2000;

fn bad_input(e: serde_json::Error) -> ApiError {
    ApiError::invalid_request(format!("bad input: {e}"))
}

fn to_value<T: serde::Serialize>(v: &T) -> ApiResult<Value> {
    serde_json::to_value(v).map_err(|_| ApiError::internal())
}

/// Makes a tool from an async function over typed input.
fn tool<I, F, Fut>(ctx: &ModuleCtx, name: &str, description: &str, schema: Value, f: F) -> ToolDef
where
    I: DeserializeOwned + Send + 'static,
    F: Fn(ModuleCtx, Caller, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<Value>> + Send + 'static,
{
    let ctx = ctx.clone();
    let f = Arc::new(f);
    ToolDef {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        scope: SCOPE_READ.to_owned(),
        handler: Arc::new(move |caller, input| {
            let ctx = ctx.clone();
            let f = f.clone();
            Box::pin(async move {
                let input: I = serde_json::from_value(input).map_err(bad_input)?;
                f(ctx, caller, input).await
            })
        }),
    }
}

/// Schema of an object with these properties; `required` lists the mandatory ones.
fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn depot_prop() -> Value {
    json!({ "type": "string", "description": "Depot name, as list_depots shows it" })
}

fn rev_props(props: &mut Value) {
    props["rev"] = json!({
        "type": "integer", "minimum": 1,
        "description": "Revision number. Takes precedence over `branch`."
    });
    props["branch"] = json!({
        "type": "string",
        "description": "Branch whose head revision to read when `rev` is not given (default `main`)"
    });
}

/// Every core tool.
pub(crate) fn core_tools(ctx: &ModuleCtx) -> Vec<ToolDef> {
    let mut list_dir_props = json!({
        "depot": depot_prop(),
        "path": { "type": "string", "description": "Directory, such as `src/net`; the root when omitted" }
    });
    rev_props(&mut list_dir_props);
    let mut read_file_props = json!({
        "depot": depot_prop(),
        "path": { "type": "string", "description": "File path, such as `src/main.rs`" },
        "start_line": { "type": "integer", "minimum": 1, "description": "First line to return (1-based, default 1)" },
        "end_line": { "type": "integer", "minimum": 1, "description": "Last line to return, inclusive (default: to the end)" },
        "max_bytes": { "type": "integer", "minimum": 1, "maximum": READ_FILE_MAX,
                       "description": "Most bytes of text to return (default 262144)" }
    });
    rev_props(&mut read_file_props);
    let mut blame_props = json!({
        "depot": depot_prop(),
        "path": { "type": "string", "description": "Text file path, such as `src/main.rs`" },
        "start_line": { "type": "integer", "minimum": 1, "description": "First line to return (1-based, default 1)" },
        "end_line": { "type": "integer", "minimum": 1, "description": "Last line to return, inclusive" }
    });
    rev_props(&mut blame_props);
    vec![
        tool(
            ctx,
            "whoami",
            "Who this token belongs to: name, kind (`user` or `agent`), the owner of an agent, \
             and the token's scopes. Scopes decide which tools you can use: `read` reads, \
             `shelve` proposes changes as shelves, `write` commits.",
            object(json!({}), &[]),
            whoami,
        ),
        tool(
            ctx,
            "list_depots",
            "List the depots (repositories) you can see on this server.",
            object(json!({}), &[]),
            list_depots,
        ),
        tool(
            ctx,
            "list_branches",
            "List the branches of a depot that you can read, with each branch's head revision \
             and the branch it was created from.",
            object(
                json!({
                    "depot": depot_prop(),
                    "include_archived": { "type": "boolean", "description": "Also list archived branches" }
                }),
                &["depot"],
            ),
            list_branches,
        ),
        tool(
            ctx,
            "log",
            "Revision history of a depot, newest first: number, branch, author, submitter, time \
             and message. Narrow it to a branch (including the history it was branched from), \
             a file or directory, or an author. Page with `before`: pass the smallest `rev` \
             you got while `more` is true.",
            object(
                json!({
                    "depot": depot_prop(),
                    "branch": { "type": "string", "description": "Only this branch and the history it came from" },
                    "path": { "type": "string", "description": "Only revisions that touched this file or directory" },
                    "author": { "type": "string", "description": "Only revisions by this principal" },
                    "author_kind": { "type": "string", "enum": ["user", "agent"],
                                     "description": "Only revisions by people or only by agents" },
                    "before": { "type": "integer", "minimum": 1, "description": "Only revisions older than this one" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": PAGE_MAX,
                               "description": "Revisions per page (default 20)" }
                }),
                &["depot"],
            ),
            log,
        ),
        tool(
            ctx,
            "show_revision",
            "One revision in full: author, submitter, time, message, provenance (for agent \
             work: model, session, tool) and the list of changed files. Use `diff` to see the \
             changes themselves.",
            object(
                json!({
                    "depot": depot_prop(),
                    "rev": { "type": "integer", "minimum": 1, "description": "Revision number" }
                }),
                &["depot", "rev"],
            ),
            show_revision,
        ),
        tool(
            ctx,
            "list_dir",
            "List one directory of a depot at a revision: names, kinds (`file`, `dir`, \
             `symlink`), sizes and content ids. Directories are not expanded; call again with \
             a deeper `path`.",
            object(list_dir_props, &["depot"]),
            list_dir,
        ),
        tool(
            ctx,
            "read_file",
            "Read a text file at a revision, whole or a range of lines. Output is capped \
             (default 256 KiB); `truncated` is true when there is more, and `end_line` tells \
             you where to continue. Binary files are not returned: you get `binary: true` with \
             the size and content id instead.",
            object(read_file_props, &["depot", "path"]),
            read_file,
        ),
        tool(
            ctx,
            "diff",
            "Unified text diff between two revisions of a depot, optionally only below a path. \
             Without `from_rev` it shows what `to_rev` changed against its parent. Lists every \
             changed file with its action; binary and very large files are listed but not \
             compared. The text is capped (default 256 KiB); `truncated` is true when cut.",
            object(
                json!({
                    "depot": depot_prop(),
                    "from_rev": { "type": "integer", "minimum": 1, "description": "Old side (default: the parent of `to_rev`)" },
                    "to_rev": { "type": "integer", "minimum": 1, "description": "New side" },
                    "path": { "type": "string", "description": "Only files at or below this path" },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": DIFF_MAX,
                                   "description": "Most bytes of diff text (default 262144)" }
                }),
                &["depot", "to_rev"],
            ),
            diff,
        ),
        tool(
            ctx,
            "blame",
            "Which revision last changed each line of a text file, with the author, time and \
             message summary of each revision named. History is followed through copies and \
             renames and into the branch the file came from. At most 2000 lines per call; \
             `next_line` is set when there are more. `truncated` means the history walk \
             stopped early, so the oldest revision named may stand for older ones.",
            object(blame_props, &["depot", "path"]),
            blame,
        ),
        tool(
            ctx,
            "search_history",
            "Find revisions whose message or changed paths contain some text (case does not \
             matter), newest first. Each hit names the paths that matched. Looks through up \
             to 2000 revisions per call; when `next_before` is set, call again with it as \
             `before` to search older history.",
            object(
                json!({
                    "depot": depot_prop(),
                    "query": { "type": "string", "minLength": 1, "description": "Text to look for" },
                    "branch": { "type": "string", "description": "Only this branch and the history it came from" },
                    "path": { "type": "string", "description": "Only revisions that touched this file or directory" },
                    "before": { "type": "integer", "minimum": 1, "description": "Only revisions older than this one" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": PAGE_MAX,
                               "description": "Most hits to return (default 20)" }
                }),
                &["depot", "query"],
            ),
            search_history,
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DepotInput {
    depot: String,
    #[serde(default)]
    include_archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogInput {
    depot: String,
    branch: Option<String>,
    path: Option<String>,
    author: Option<String>,
    author_kind: Option<String>,
    before: Option<u64>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevInput {
    depot: String,
    rev: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirInput {
    depot: String,
    path: Option<String>,
    rev: Option<u64>,
    branch: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileInput {
    depot: String,
    path: String,
    rev: Option<u64>,
    branch: Option<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlameInput {
    depot: String,
    path: String,
    rev: Option<u64>,
    branch: Option<String>,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffInput {
    depot: String,
    from_rev: Option<u64>,
    to_rev: u64,
    path: Option<String>,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    depot: String,
    query: String,
    branch: Option<String>,
    path: Option<String>,
    before: Option<u64>,
    limit: Option<u32>,
}

fn kind_name(k: PrincipalKind) -> &'static str {
    match k {
        PrincipalKind::User => "user",
        PrincipalKind::Agent => "agent",
    }
}

/// A path from tool input; empty or `/` means the root.
fn opt_path(p: Option<&str>) -> ApiResult<Option<DepotPath>> {
    match p.map(|p| p.trim_matches('/')) {
        None | Some("") => Ok(None),
        Some(p) => DepotPath::parse(p)
            .map(Some)
            .map_err(|e| ApiError::invalid_request(format!("invalid path `{p}`: {e}"))),
    }
}

fn path(p: &str) -> ApiResult<DepotPath> {
    opt_path(Some(p))?.ok_or_else(|| ApiError::invalid_request("`path` names no file"))
}

/// The revision to read: `rev`, or the head of `branch` (default `main`).
async fn pick_rev(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &str,
    rev: Option<u64>,
    branch: Option<&str>,
) -> ApiResult<u64> {
    if let Some(r) = rev {
        return Ok(r);
    }
    let name = branch.unwrap_or("main");
    ctx.depots()
        .branches(caller, depot, true)
        .await?
        .branches
        .into_iter()
        .find(|b| b.name == name)
        .map(|b| b.head_rev)
        .ok_or_else(|| ApiError::not_found(format!("no branch `{name}`")))
}

async fn whoami(ctx: ModuleCtx, caller: Caller, _: NoInput) -> ApiResult<Value> {
    let p = &caller.principal;
    let owner = match (p.kind, p.owner_id) {
        (PrincipalKind::Agent, Some(id)) => ctx
            .meta()
            .read(move |tx| tx.principal(id))
            .await?
            .map(|o| o.name),
        _ => None,
    };
    let mut v = json!({
        "name": p.name,
        "kind": kind_name(p.kind),
        "scopes": caller.scopes,
        "server_admin": ctx.auth().is_server_admin(&caller),
    });
    if let Some(d) = &p.display_name {
        v["display_name"] = json!(d);
    }
    if let Some(o) = owner {
        v["owner"] = json!(o);
    }
    Ok(v)
}

async fn list_depots(ctx: ModuleCtx, caller: Caller, _: NoInput) -> ApiResult<Value> {
    to_value(&ctx.depots().list(&caller).await?)
}

async fn list_branches(ctx: ModuleCtx, caller: Caller, input: DepotInput) -> ApiResult<Value> {
    to_value(
        &ctx.depots()
            .branches(&caller, &input.depot, input.include_archived)
            .await?,
    )
}

async fn log(ctx: ModuleCtx, caller: Caller, input: LogInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let author_kind = match input.author_kind.as_deref() {
        None => None,
        Some("user") => Some(lode_proto::PrincipalKind::User),
        Some("agent") => Some(lode_proto::PrincipalKind::Agent),
        Some(k) => {
            return Err(ApiError::invalid_request(format!(
                "author_kind must be `user` or `agent`, not `{k}`"
            )))
        }
    };
    let q = RevisionListQuery {
        branch: input.branch,
        path: opt_path(input.path.as_deref())?,
        before: input.before,
        author: input.author,
        author_kind,
        limit: Some(input.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX)),
        ..RevisionListQuery::default()
    };
    to_value(&ctx.depots().revisions(&caller, &depot, q).await?)
}

async fn show_revision(ctx: ModuleCtx, caller: Caller, input: RevInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    to_value(&ctx.depots().revision(&caller, &depot, input.rev).await?)
}

async fn list_dir(ctx: ModuleCtx, caller: Caller, input: ListDirInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let rev = pick_rev(
        &ctx,
        &caller,
        &input.depot,
        input.rev,
        input.branch.as_deref(),
    )
    .await?;
    let dir = opt_path(input.path.as_deref())?;
    let entries = ctx
        .depots()
        .list_dir(&caller, &depot, rev, dir.as_ref())
        .await?;
    Ok(json!({
        "rev": rev,
        "path": dir.as_ref().map(DepotPath::as_str).unwrap_or(""),
        "entries": entries,
    }))
}

/// Whether bytes look like binary content: a NUL byte, or not UTF-8 (allowing a character
/// cut at the end).
fn is_binary(head: &[u8]) -> bool {
    if head.contains(&0) {
        return true;
    }
    match std::str::from_utf8(head) {
        Ok(_) => false,
        Err(e) => e.error_len().is_some(),
    }
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

async fn read_file(ctx: ModuleCtx, caller: Caller, input: ReadFileInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let rev = pick_rev(
        &ctx,
        &caller,
        &input.depot,
        input.rev,
        input.branch.as_deref(),
    )
    .await?;
    let file_path = path(&input.path)?;
    let max = input
        .max_bytes
        .unwrap_or(READ_FILE_DEFAULT)
        .clamp(1, READ_FILE_MAX);
    let start = input.start_line.unwrap_or(1).max(1);
    if input.end_line.is_some_and(|e| e < start) {
        return Err(ApiError::invalid_request(
            "`end_line` is before `start_line`",
        ));
    }
    let file = ctx
        .depots()
        .open_file(&caller, &depot, rev, &file_path)
        .await?;
    let mut head = Vec::with_capacity(SNIFF);
    let mut reader = file.reader;
    (&mut reader)
        .take(SNIFF as u64)
        .read_to_end(&mut head)
        .await
        .map_err(read_error)?;
    let mut out = json!({
        "path": file_path.as_str(),
        "rev": rev,
        "id": file.entry.id.to_string(),
        "size": file.len,
        "kind": if file.entry.kind == EntryKind::Symlink { "symlink" } else { "file" },
    });
    if is_binary(&head) {
        out["binary"] = json!(true);
        return Ok(out);
    }
    let lines = read_lines(
        std::io::Cursor::new(head).chain(reader),
        start,
        input.end_line,
        max,
    )
    .await?;
    out["binary"] = json!(false);
    out["text"] = json!(lines.text);
    out["start_line"] = json!(start);
    out["end_line"] = json!(lines.last);
    out["truncated"] = json!(lines.truncated);
    Ok(out)
}

fn read_error(e: std::io::Error) -> ApiError {
    tracing::error!(error = %e, "cannot read a stored file");
    ApiError::internal()
}

struct Lines {
    text: String,
    /// Number of the last line returned (`start - 1` when none).
    last: u64,
    /// Whether the cap stopped the read before `end`.
    truncated: bool,
}

/// Lines `start..=end` of `r`, at most `max` bytes. A line that does not fit is cut.
async fn read_lines<R: AsyncRead + Unpin>(
    r: R,
    start: u64,
    end: Option<u64>,
    max: usize,
) -> ApiResult<Lines> {
    let mut r = BufReader::new(r);
    let mut text = String::new();
    let mut line = Vec::new();
    let mut n = 0u64;
    let mut last = start - 1;
    let done = |text, last, truncated| {
        Ok(Lines {
            text,
            last,
            truncated,
        })
    };
    loop {
        if end.is_some_and(|e| n >= e) {
            return done(text, last, false);
        }
        if n + 1 < start {
            if !skip_line(&mut r).await? {
                return done(text, last, false);
            }
            n += 1;
            continue;
        }
        let room = max - text.len();
        line.clear();
        // Read no more of a line than could still fit, plus one byte to notice the overflow.
        let read = (&mut r)
            .take(room as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(read_error)?;
        if read == 0 {
            return done(text, last, false);
        }
        n += 1;
        let s = String::from_utf8_lossy(&line);
        if line.len() > room || s.len() > room {
            text.push_str(cut(&s, room));
            return done(text, last, true);
        }
        text.push_str(&s);
        last = n;
    }
}

/// Skips one line; `false` at the end of the input.
async fn skip_line<R: AsyncBufReadExt + Unpin>(r: &mut R) -> ApiResult<bool> {
    let mut buf = Vec::new();
    let mut any = false;
    loop {
        buf.clear();
        let read = (&mut *r)
            .take(64 * 1024)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(read_error)?;
        if read == 0 {
            return Ok(any);
        }
        any = true;
        if buf.last() == Some(&b'\n') {
            return Ok(true);
        }
    }
}

/// The content of a blob, or why it is not compared.
enum Side {
    Text(String),
    Skipped(&'static str),
}

async fn side(ctx: &ModuleCtx, depot: &DepotRow, id: &ObjectId, size: u64) -> ApiResult<Side> {
    if size > DIFF_FILE_MAX {
        return Ok(Side::Skipped("too_large"));
    }
    if ctx.blobs().is_obliterated(depot.id, id).await? {
        return Ok(Side::Skipped("obliterated"));
    }
    let obj = ctx.blobs().store().get(depot.id, id).await?;
    let mut buf = Vec::new();
    let mut reader = obj.reader;
    (&mut reader)
        .take(DIFF_FILE_MAX + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(read_error)?;
    if buf.contains(&0) {
        return Ok(Side::Skipped("binary"));
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(Side::Text(s)),
        Err(_) => Ok(Side::Skipped("binary")),
    }
}

/// Most lines one `blame` call returns.
const BLAME_LINES_MAX: usize = 2000;

async fn blame(ctx: ModuleCtx, caller: Caller, input: BlameInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let rev = pick_rev(
        &ctx,
        &caller,
        &input.depot,
        input.rev,
        input.branch.as_deref(),
    )
    .await?;
    let file_path = path(&input.path)?;
    let start = input.start_line.unwrap_or(1).max(1);
    if input.end_line.is_some_and(|e| e < start) {
        return Err(ApiError::invalid_request(
            "`end_line` is before `start_line`",
        ));
    }
    let b = ctx.depots().blame(&caller, &depot, rev, &file_path).await?;
    let total = b.lines.len();
    let end = input
        .end_line
        .unwrap_or(total)
        .min(total)
        .min(start.saturating_add(BLAME_LINES_MAX - 1));
    let lines: Vec<Value> = b
        .lines
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(end.saturating_sub(start - 1))
        .map(|(i, l)| json!({ "line": i + 1, "rev": l.rev, "text": l.text }))
        .collect();
    let named: std::collections::BTreeSet<u64> =
        lines.iter().filter_map(|l| l["rev"].as_u64()).collect();
    let revisions: Vec<&lode_proto::read::BlameRevision> = b
        .revisions
        .iter()
        .filter(|r| named.contains(&r.rev))
        .collect();
    let wanted_end = input.end_line.unwrap_or(total).min(total);
    Ok(json!({
        "path": b.path,
        "rev": b.rev,
        "total_lines": total,
        "lines": lines,
        "revisions": revisions,
        "truncated": b.truncated,
        "next_line": (end < wanted_end).then_some(end + 1),
    }))
}

async fn diff(ctx: ModuleCtx, caller: Caller, input: DiffInput) -> ApiResult<Value> {
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let to: RevisionView = ctx.depots().revision(&caller, &depot, input.to_rev).await?;
    let from = match input.from_rev {
        Some(f) => f,
        None => to.parent.unwrap_or(to.rev),
    };
    let under = opt_path(input.path.as_deref())?;
    let max = input.max_bytes.unwrap_or(DIFF_DEFAULT).clamp(1, DIFF_MAX);
    let (changes, more) = ctx
        .depots()
        .changed_files(
            &caller,
            &depot,
            from,
            to.rev,
            under.as_ref(),
            DIFF_FILES_MAX,
        )
        .await?;
    let mut text = String::new();
    let mut truncated = more;
    let mut files = Vec::with_capacity(changes.len());
    for c in &changes {
        let action = match (&c.old, &c.new) {
            (None, _) => "add",
            (_, None) => "delete",
            _ => "modify",
        };
        let mut f = json!({ "path": c.path.as_str(), "action": action });
        if let Some(o) = &c.old {
            f["old_id"] = json!(o.id.to_string());
        }
        if let Some(n) = &c.new {
            f["new_id"] = json!(n.id.to_string());
        }
        if text.len() < max {
            let old = match &c.old {
                Some(e) => side(&ctx, &depot, &e.id, e.size).await?,
                None => Side::Text(String::new()),
            };
            let new = match &c.new {
                Some(e) => side(&ctx, &depot, &e.id, e.size).await?,
                None => Side::Text(String::new()),
            };
            match (old, new) {
                (Side::Text(a), Side::Text(b)) => {
                    let from_name = match action {
                        "add" => "/dev/null".to_owned(),
                        _ => format!("a/{}", c.path.as_str()),
                    };
                    let to_name = match action {
                        "delete" => "/dev/null".to_owned(),
                        _ => format!("b/{}", c.path.as_str()),
                    };
                    let patch = tokio::task::spawn_blocking(move || {
                        let mut opts = diffy::DiffOptions::new();
                        opts.set_original_filename(from_name)
                            .set_modified_filename(to_name);
                        opts.create_patch(&a, &b).to_string()
                    })
                    .await
                    .map_err(|_| ApiError::internal())?;
                    let room = max - text.len();
                    if patch.len() > room {
                        text.push_str(cut(&patch, room));
                        truncated = true;
                    } else {
                        text.push_str(&patch);
                    }
                }
                (Side::Skipped(why), _) | (_, Side::Skipped(why)) => {
                    f["skipped"] = json!(why);
                }
            }
        } else {
            truncated = true;
        }
        files.push(f);
    }
    Ok(json!({
        "from_rev": from,
        "to_rev": to.rev,
        "files": files,
        "diff": text,
        "truncated": truncated,
    }))
}

async fn search_history(ctx: ModuleCtx, caller: Caller, input: SearchInput) -> ApiResult<Value> {
    let needle = input.query.to_lowercase();
    if needle.trim().is_empty() {
        return Err(ApiError::invalid_request("`query` is empty"));
    }
    let (depot, _) = ctx.depots().resolve(&caller, &input.depot).await?;
    let limit = input.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX) as usize;
    let under = opt_path(input.path.as_deref())?;
    let mut before = input.before;
    let mut scanned = 0usize;
    let mut hits = Vec::new();
    let mut next_before = None;
    'pages: loop {
        let q = RevisionListQuery {
            branch: input.branch.clone(),
            path: under.clone(),
            before,
            changes: true,
            limit: Some(PAGE_MAX),
            ..RevisionListQuery::default()
        };
        let page = ctx.depots().revisions(&caller, &depot, q).await?;
        for r in &page.revisions {
            scanned += 1;
            before = Some(r.rev);
            let in_message = r.message.to_lowercase().contains(&needle);
            let paths: Vec<&str> = r
                .changes
                .iter()
                .flatten()
                .map(|c| c.path.as_str())
                .filter(|p| p.to_lowercase().contains(&needle))
                .collect();
            if in_message || !paths.is_empty() {
                hits.push(json!({
                    "rev": r.rev,
                    "branch": r.branch,
                    "author": r.author,
                    "created_at": r.created_at,
                    "message": r.message,
                    "matched_paths": paths,
                }));
                if hits.len() >= limit {
                    next_before = Some(r.rev);
                    break 'pages;
                }
            }
            if scanned >= SEARCH_SCAN {
                next_before = Some(r.rev);
                break 'pages;
            }
        }
        if !page.more {
            break;
        }
    }
    // A search that stopped on its last hit of the last page has nothing older left.
    if next_before.is_some_and(|b| b <= 1) {
        next_before = None;
    }
    Ok(json!({
        "revisions": hits,
        "next_before": next_before,
        "scanned": scanned,
    }))
}
