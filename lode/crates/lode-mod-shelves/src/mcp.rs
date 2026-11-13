//! Workspace-free shelves for agents (ARCHITECTURE §9.3): `create_shelf`, `update_shelf` and
//! `submit_shelf`. Edits carry whole new file content; the server stores the blobs, derives
//! each `base_id` from the base revision and makes the shelf exactly as `POST …/shelves`
//! would.

use std::sync::Arc;

use base64::Engine as _;
use lode_core::{
    blob_id, Change, ChangeAction, CopyFrom, DepotPath, EntryKind, ErrorCode, ObjectKind,
};
use lode_meta::DepotRow;
use lode_proto::commit::{AgentProvenance, Provenance};
use lode_proto::shelves::{CreateShelfRequest, SubmitShelfRequest, UpdateShelfRequest};
use lode_server::auth::SCOPE_WRITE;
use lode_server::module::{ModuleCtx, ToolDef};
use lode_server::{ApiError, ApiResult, Caller};
use lode_store::PutOptions;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{create_shelf, get_shelf, submit_shelf, update_shelf, SCOPE_SHELVE};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    action: String,
    text: Option<String>,
    base64: Option<String>,
    executable: Option<bool>,
    copy_from: Option<CopyFromInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyFromInput {
    path: String,
    rev: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    depot: String,
    branch: String,
    base_rev: Option<u64>,
    description: String,
    edits: Vec<EditInput>,
    agent: Option<AgentProvenance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    depot: String,
    id: i64,
    base_rev: Option<u64>,
    description: Option<String>,
    edits: Vec<EditInput>,
    agent: Option<AgentProvenance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitInput {
    depot: String,
    id: i64,
    expected_version: u64,
    message: Option<String>,
}

fn bad_input(e: serde_json::Error) -> ApiError {
    ApiError::invalid_request(format!("bad input: {e}"))
}

fn too_large(message: String) -> ApiError {
    ApiError::new(ErrorCode::TooLarge, message)
}

fn edits_schema() -> Value {
    json!({
        "type": "array",
        "description": "Every file the shelf changes, each with its whole new content",
        "items": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, such as `src/main.rs`" },
                "action": { "type": "string", "enum": ["add", "modify", "delete"] },
                "text": { "type": "string", "description": "New content as UTF-8 text" },
                "base64": { "type": "string", "description": "New content as base64, for binary files" },
                "executable": { "type": "boolean", "description": "Executable flag (default: unchanged, or false for a new file)" },
                "copy_from": {
                    "type": "object",
                    "description": "For `add`: the file was copied from this path and revision. Without content, the copy is exact.",
                    "properties": {
                        "path": { "type": "string" },
                        "rev": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["path", "rev"],
                    "additionalProperties": false
                }
            },
            "required": ["path", "action"],
            "additionalProperties": false
        }
    })
}

fn agent_schema() -> Value {
    json!({
        "type": "object",
        "description": "Who made this, self-reported: string fields such as `model`, `session`, `tool`, `ticket` (4 KiB at most)",
        "additionalProperties": { "type": "string" }
    })
}

/// The shelf tools that write.
pub(crate) fn tools(ctx: &ModuleCtx) -> Vec<ToolDef> {
    let create_ctx = ctx.clone();
    let update_ctx = ctx.clone();
    let submit_ctx = ctx.clone();
    vec![
        ToolDef {
            name: "create_shelf".into(),
            description: "Propose changes without a workspace: store them on the server as a \
                          new shelf (a change set a person can review and submit) against a \
                          base revision of a branch. Give every changed file with its whole \
                          new content, as `text` or `base64`. Base the shelf on the revision you \
                          read the files at (default: the branch head). At most 500 edits and \
                          16 MiB of content per call. Returns the shelf with its id and version."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "depot": { "type": "string", "description": "Depot name" },
                    "branch": { "type": "string", "description": "Branch the changes are for" },
                    "base_rev": { "type": "integer", "minimum": 1,
                                  "description": "Revision the changes are based on (default: the branch head)" },
                    "description": { "type": "string", "description": "What the changes do and why" },
                    "edits": edits_schema(),
                    "agent": agent_schema()
                },
                "required": ["depot", "branch", "description", "edits"],
                "additionalProperties": false
            }),
            scope: SCOPE_SHELVE.into(),
            handler: Arc::new(move |caller, input| {
                let ctx = create_ctx.clone();
                Box::pin(async move {
                    let input: CreateInput = serde_json::from_value(input).map_err(bad_input)?;
                    let (d, _) = ctx.depots().resolve(&caller, &input.depot).await?;
                    let base_rev = match input.base_rev {
                        Some(r) => r,
                        None => head(&ctx, &caller, &input.depot, &input.branch).await?,
                    };
                    let changes = changes(&ctx, &caller, &d, base_rev, input.edits).await?;
                    let req = CreateShelfRequest {
                        branch: input.branch,
                        base_rev,
                        description: input.description,
                        changes,
                        merged_from: Vec::new(),
                        provenance: Some(provenance(input.agent)),
                    };
                    let view = create_shelf(&ctx, &caller, &input.depot, req).await?;
                    serde_json::to_value(view).map_err(|_| ApiError::internal())
                })
            }),
        },
        ToolDef {
            name: "update_shelf".into(),
            description: "Replace the content of a shelf you own with a new set of edits \
                          (every file the shelf should change, not only the ones that differ \
                          from the last version). The shelf's version goes up by one, so \
                          reviewers see that it changed. The description, base revision and \
                          `agent` stay unless given."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "depot": { "type": "string", "description": "Depot name" },
                    "id": { "type": "integer", "minimum": 1, "description": "Shelf id" },
                    "base_rev": { "type": "integer", "minimum": 1,
                                  "description": "New base revision (default: unchanged)" },
                    "description": { "type": "string", "description": "New description (default: unchanged)" },
                    "edits": edits_schema(),
                    "agent": agent_schema()
                },
                "required": ["depot", "id", "edits"],
                "additionalProperties": false
            }),
            scope: SCOPE_SHELVE.into(),
            handler: Arc::new(move |caller, input| {
                let ctx = update_ctx.clone();
                Box::pin(async move {
                    let input: UpdateInput = serde_json::from_value(input).map_err(bad_input)?;
                    let (d, _) = ctx.depots().resolve(&caller, &input.depot).await?;
                    let current = get_shelf(&ctx, &caller, &input.depot, input.id, false).await?;
                    let base_rev = input.base_rev.unwrap_or(current.base_rev);
                    let changes = changes(&ctx, &caller, &d, base_rev, input.edits).await?;
                    let req = UpdateShelfRequest {
                        base_rev,
                        description: input.description.unwrap_or(current.description),
                        changes,
                        merged_from: current.merged_from,
                        // Without a new `agent`, the shelf keeps the one it had.
                        provenance: Some(provenance(
                            input.agent.or(current.provenance.and_then(|p| p.agent)),
                        )),
                    };
                    let view = update_shelf(&ctx, &caller, &input.depot, input.id, req).await?;
                    serde_json::to_value(view).map_err(|_| ApiError::internal())
                })
            }),
        },
        ToolDef {
            name: "submit_shelf".into(),
            description: "Commit a shelf to its branch. The revision is authored by the \
                          shelf's owner and submitted by you. `expected_version` must be the \
                          version you reviewed; if the shelf changed since, the call fails with \
                          `conflict` and you should review it again."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "depot": { "type": "string", "description": "Depot name" },
                    "id": { "type": "integer", "minimum": 1, "description": "Shelf id" },
                    "expected_version": { "type": "integer", "minimum": 1,
                                          "description": "The version you reviewed" },
                    "message": { "type": "string", "description": "Commit message (default: the shelf's description)" }
                },
                "required": ["depot", "id", "expected_version"],
                "additionalProperties": false
            }),
            scope: SCOPE_WRITE.into(),
            handler: Arc::new(move |caller, input| {
                let ctx = submit_ctx.clone();
                Box::pin(async move {
                    let input: SubmitInput = serde_json::from_value(input).map_err(bad_input)?;
                    let req = SubmitShelfRequest {
                        expected_version: input.expected_version,
                        message: input.message,
                    };
                    let resp = submit_shelf(&ctx, &caller, &input.depot, input.id, req).await?;
                    serde_json::to_value(resp).map_err(|_| ApiError::internal())
                })
            }),
        },
    ]
}

fn provenance(agent: Option<AgentProvenance>) -> Provenance {
    Provenance {
        agent,
        ..Provenance::default()
    }
}

async fn head(ctx: &ModuleCtx, caller: &Caller, depot: &str, branch: &str) -> ApiResult<u64> {
    ctx.depots()
        .branches(caller, depot, false)
        .await?
        .branches
        .into_iter()
        .find(|b| b.name == branch)
        .map(|b| b.head_rev)
        .ok_or_else(|| ApiError::not_found(format!("no branch `{branch}`")))
}

fn parse_path(p: &str) -> ApiResult<DepotPath> {
    DepotPath::parse(p).map_err(|e| ApiError::invalid_request(format!("invalid path `{p}`: {e}")))
}

/// Turns edits into a change list: checks the limits, derives `base_id` from `base_rev`
/// (which also refuses paths the caller cannot read, as `not_found`) and stores the content.
async fn changes(
    ctx: &ModuleCtx,
    caller: &Caller,
    depot: &DepotRow,
    base_rev: u64,
    edits: Vec<EditInput>,
) -> ApiResult<Vec<Change>> {
    let limits = ctx.limits();
    if edits.len() > limits.mcp_max_edits {
        return Err(too_large(format!(
            "{} edits in one call; at most {} are allowed, use the HTTP API for more",
            edits.len(),
            limits.mcp_max_edits
        )));
    }
    let inline: u64 = edits
        .iter()
        .map(|e| {
            e.text.as_ref().map_or(0, String::len) as u64
                + (e.base64.as_ref().map_or(0, String::len) as u64 / 4 * 3)
        })
        .sum();
    if inline > limits.mcp_max_inline_bytes {
        return Err(too_large(format!(
            "{inline} bytes of content in one call; at most {} are allowed, use the HTTP blob \
             API for more",
            limits.mcp_max_inline_bytes
        )));
    }
    let mut out = Vec::with_capacity(edits.len());
    for e in edits {
        let path = parse_path(&e.path)?;
        let base = ctx.depots().entry(caller, depot, base_rev, &path).await?;
        let base = base.filter(|b| b.kind != EntryKind::Dir);
        let action = match e.action.as_str() {
            "add" => ChangeAction::Add,
            "modify" => ChangeAction::Modify,
            "delete" => ChangeAction::Delete,
            other => {
                return Err(ApiError::invalid_request(format!(
                    "{}: action must be `add`, `modify` or `delete`, not `{other}`",
                    path.as_str()
                )))
            }
        };
        let content = match (e.text, e.base64) {
            (Some(_), Some(_)) => {
                return Err(ApiError::invalid_request(format!(
                    "{}: give `text` or `base64`, not both",
                    path.as_str()
                )))
            }
            (Some(t), None) => Some(t.into_bytes()),
            (None, Some(b)) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b.as_bytes())
                    .map_err(|err| {
                        ApiError::invalid_request(format!(
                            "{}: invalid base64: {err}",
                            path.as_str()
                        ))
                    })?,
            ),
            (None, None) => None,
        };
        let copy_from = match e.copy_from {
            None => None,
            Some(c) => {
                if action != ChangeAction::Add {
                    return Err(ApiError::invalid_request(format!(
                        "{}: `copy_from` goes with `add`",
                        path.as_str()
                    )));
                }
                Some(CopyFrom {
                    path: parse_path(&c.path)?,
                    rev: c.rev,
                })
            }
        };
        let base_id = base.as_ref().map(|b| b.id);
        let change = match action {
            ChangeAction::Delete => {
                if content.is_some() {
                    return Err(ApiError::invalid_request(format!(
                        "{}: a delete carries no content",
                        path.as_str()
                    )));
                }
                Change {
                    path,
                    action,
                    base_id,
                    new_id: None,
                    kind: None,
                    executable: false,
                    size: None,
                    copy_from: None,
                }
            }
            _ => {
                let (new_id, size) = match (content, &copy_from) {
                    (Some(bytes), _) => store(ctx, depot, &bytes).await?,
                    (None, Some(c)) => {
                        let src = ctx
                            .depots()
                            .entry(caller, depot, c.rev, &c.path)
                            .await?
                            .filter(|s| s.kind != EntryKind::Dir)
                            .ok_or_else(|| {
                                ApiError::not_found(format!(
                                    "no file {} in r{}",
                                    c.path.as_str(),
                                    c.rev
                                ))
                            })?;
                        (src.id, src.size)
                    }
                    (None, None) => {
                        return Err(ApiError::invalid_request(format!(
                            "{}: give the new content as `text` or `base64`",
                            path.as_str()
                        )))
                    }
                };
                let executable = e
                    .executable
                    .unwrap_or_else(|| base.as_ref().is_some_and(|b| b.executable()));
                Change {
                    path,
                    action,
                    base_id,
                    new_id: Some(new_id),
                    kind: Some(EntryKind::File),
                    executable,
                    size: Some(size),
                    copy_from,
                }
            }
        };
        out.push(change);
    }
    Ok(out)
}

/// Stores `bytes` as a blob; its id and size.
async fn store(
    ctx: &ModuleCtx,
    depot: &DepotRow,
    bytes: &[u8],
) -> ApiResult<(lode_core::ObjectId, u64)> {
    let id = blob_id(bytes).map_err(|_| ApiError::internal())?;
    let mut reader = bytes;
    ctx.blobs()
        .store()
        .put(
            depot.id,
            ObjectKind::Blob,
            &id,
            &mut reader,
            PutOptions {
                expected_len: Some(bytes.len() as u64),
            },
        )
        .await?;
    Ok((id, bytes.len() as u64))
}
