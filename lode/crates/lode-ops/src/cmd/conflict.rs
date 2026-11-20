//! A conflict's sides, for tools that resolve it by hand (Lode Desk's three-way view).
//!
//! An update, switch, merge or unshelve that cannot merge a file leaves a conflict row with the
//! common ancestor (`base_id`), the incoming side (`theirs_id`, from `theirs_rev`) and, when
//! conflict markers were written into the file, the local content they replaced (`mine_id`,
//! kept in `.lode/conflicts/`). A binary file or a symlink is left as it was, with the incoming
//! file beside it as `*.theirs-rREV`.

use std::fs;
use std::path::Path;

use lode_workspace::db::ConflictRow;
use lode_workspace::Workspace;

use crate::cmd::diff::{one_path, Content};
use crate::cmd::update::{self, ResolveArgs};
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::local::{self, Bases, FROM_SHELF};

/// The sides of one conflicted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictSides {
    /// Depot path.
    pub path: String,
    /// The revision the incoming side came from; None when it came from a shelf.
    pub theirs_rev: Option<u64>,
    /// Whether conflict markers were written into the file (a text merge); otherwise the
    /// local file was left as it was (binary, symlink, or deleted on one side).
    pub text: bool,
    /// The common ancestor; None when the file is new on both sides.
    pub base: Option<Content>,
    /// The local side; None when it was deleted here.
    pub mine: Option<Content>,
    /// The incoming side; None when it was deleted there.
    pub theirs: Option<Content>,
    /// The file as it is now (with conflict markers for a text merge); None when not on disk.
    pub result: Option<Content>,
    /// The incoming file beside the local one, for a binary conflict (workspace-relative).
    pub theirs_file: Option<String>,
    /// The merge tool the depot's `[merge.tools]` names for this file (T164).
    pub tool: Option<String>,
}

fn conflict_of(ctx: &Ctx, ws: &Workspace, path: &str) -> OpResult<ConflictRow> {
    let p = one_path(ctx, ws, path)?;
    ws.db()
        .conflicts()?
        .into_iter()
        .find(|c| c.path == p)
        .ok_or_else(|| local::not_found("not in conflict", &[p.as_str().to_owned()]))
}

fn read_file(path: &Path, limit: u64) -> OpResult<Option<Content>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        return Ok(Some(Content {
            size: bytes.len() as u64,
            bytes: Some(bytes),
        }));
    }
    let size = meta.len();
    let bytes = if size > limit {
        None
    } else {
        Some(fs::read(path)?)
    };
    Ok(Some(Content { size, bytes }))
}

fn in_memory(bytes: Vec<u8>, limit: u64) -> Content {
    let size = bytes.len() as u64;
    Content {
        size,
        bytes: (size <= limit).then_some(bytes),
    }
}

/// The sides of the conflicted file `path`, each read into memory up to `limit` bytes.
pub async fn conflict_sides(ctx: &Ctx, path: &str, limit: u64) -> OpResult<ConflictSides> {
    let ws = local::open(ctx)?;
    let c = conflict_of(ctx, &ws, path)?;
    let local_path = ws.local_path(&c.path);
    let mut bases = Bases::new(ctx, &ws)?;
    let result = read_file(&local_path, limit)?;
    let theirs_rev = (c.theirs_rev != FROM_SHELF).then_some(c.theirs_rev);
    let tool = crate::cmd::mergetool::tool_for(update::local_config(&ws).as_ref(), &c.path);
    if let Some(mine_id) = c.mine_id {
        // A text merge: every side is small enough to have been merged in memory.
        let base = match c.base_id {
            Some(id) => Some(in_memory(bases.read(&id).await?, limit)),
            None => None,
        };
        let theirs = match c.theirs_id {
            Some(id) => Some(in_memory(bases.read(&id).await?, limit)),
            None => None,
        };
        let mine = read_file(&local::mine_path(&ws, &mine_id), limit)?.ok_or_else(|| {
            OpError::failed(
                "workspace",
                format!(
                    "the local version of {} is gone; resolve it with theirs or as it is now",
                    c.path
                ),
            )
        })?;
        return Ok(ConflictSides {
            path: c.path.as_str().to_owned(),
            theirs_rev,
            text: true,
            base,
            mine: Some(mine),
            theirs,
            result,
            theirs_file: None,
            tool,
        });
    }
    // Left as it was: the local file is mine; theirs is beside it, or deleted there.
    let beside = local::theirs_path(&local_path, c.theirs_rev);
    let theirs = match read_file(&beside, limit)? {
        Some(t) => Some(t),
        None => match c.theirs_id {
            Some(id) => Some(in_memory(bases.read(&id).await?, limit)),
            None => None,
        },
    };
    let theirs_file = beside
        .strip_prefix(ws.root())
        .ok()
        .filter(|_| beside.exists())
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    Ok(ConflictSides {
        path: c.path.as_str().to_owned(),
        theirs_rev,
        text: false,
        base: None,
        mine: result.clone(),
        theirs,
        result,
        theirs_file,
        tool,
    })
}

/// Writes `content` as the resolved file `path` and marks it resolved (`lode resolve --merged`
/// after editing by hand).
pub async fn resolve_with(ctx: &Ctx, path: &str, content: &[u8]) -> OpResult<serde_json::Value> {
    {
        let ws = local::open(ctx)?;
        let c = conflict_of(ctx, &ws, path)?;
        let _lock = ws.lock()?;
        let exec = ws.db().file(&c.path)?.is_some_and(|r| r.executable);
        update::write_through_tmp(&ws, &c.path, content, exec)?;
    }
    update::resolve(
        ctx,
        ResolveArgs {
            paths: vec![path.into()],
            merged: true,
            ..Default::default()
        },
    )
    .await
}
