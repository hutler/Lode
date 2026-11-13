//! Applying changes made elsewhere to the workspace: `unshelve` and `merge`.

use std::collections::BTreeMap;
use std::fs;

use lode_core::{Change, ChangeAction, DepotPath, EntryKind, LodeConfig, ObjectId};
use lode_workspace::classify::{class_by_config, class_by_content};
use lode_workspace::{ConflictRow, FileClass, FileRow, FileState, ScanEntry, Workspace};
use serde_json::json;

use crate::cmd::diff::TEXT_LIMIT;
use crate::cmd::update::{disk_id, local_config, relabel, write_through_tmp};
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::local::{self, Bases};
use crate::materialize::remove_and_prune;

/// What applying a change did to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Applied,
    Merged,
    Conflicted,
    Unchanged,
    Skipped,
}

impl Outcome {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Merged => "merged",
            Outcome::Conflicted => "conflicted",
            Outcome::Unchanged => "unchanged",
            Outcome::Skipped => "skipped",
        }
    }
}

/// Applies changes made elsewhere (a shelf, a range of another branch) on top of the
/// workspace: what is unchanged here since the change's base takes the new side, the rest is
/// merged three-way or left in conflict. Rows keep the workspace's base.
pub(crate) struct Apply<'a> {
    bases: Bases<'a>,
    config: Option<LodeConfig>,
    /// Names the incoming side in conflict markers, such as `shelf 4`.
    label: String,
    /// Recorded on conflicts: [`local::FROM_SHELF`] or the source revision.
    theirs_rev: u64,
}

impl Apply<'_> {
    fn is_text(&self, path: &DepotPath, sides: &[&[u8]]) -> bool {
        match class_by_config(path, self.config.as_ref()) {
            Some(c) => c == FileClass::Text,
            None => sides
                .iter()
                .all(|s| s.len() as u64 <= TEXT_LIMIT && class_by_content(s) == FileClass::Text),
        }
    }

    /// A row for `c.path` now that the incoming content is on disk: the row it had (deleted
    /// no more), or an add.
    fn put_row(ws: &mut Workspace, c: &Change, row: Option<FileRow>) -> CliResult<()> {
        let r = match row {
            Some(mut r) => {
                if r.state == FileState::Deleted {
                    r.state = FileState::Normal;
                }
                r.mtime_ns = 0;
                r
            }
            None => {
                let mut r = crate::cmd::files::added_row(&ws.local_path(&c.path), c.path.clone())?;
                r.copy_from = c.copy_from.clone();
                r.mtime_ns = 0;
                r
            }
        };
        ws.db_mut().write(|tx| tx.put_file(&r))?;
        Ok(())
    }

    /// Records a conflict: the row keeps the workspace's base, the conflict holds the incoming
    /// side.
    fn conflict(
        &self,
        ws: &mut Workspace,
        c: &Change,
        row: Option<FileRow>,
        mine_id: Option<ObjectId>,
    ) -> CliResult<Outcome> {
        let mut r = row.unwrap_or(FileRow {
            path: c.path.clone(),
            base_id: None,
            kind: c.kind.unwrap_or(EntryKind::File),
            executable: c.executable,
            size: 0,
            mtime_ns: 0,
            state: FileState::Added,
            copy_from: c.copy_from.clone(),
            changelist_id: None,
            lock_held: false,
        });
        r.state = FileState::Conflicted;
        r.mtime_ns = 0;
        let conflict = ConflictRow {
            path: c.path.clone(),
            base_id: c.base_id,
            theirs_id: c.new_id,
            theirs_rev: self.theirs_rev,
            mine_id,
        };
        ws.db_mut().write(|tx| {
            tx.put_file(&r)?;
            tx.put_conflict(&conflict)
        })?;
        Ok(Outcome::Conflicted)
    }

    /// An add or modify: the incoming side has `new_id` at the path.
    async fn put(
        &mut self,
        ws: &mut Workspace,
        c: &Change,
        e: Option<&ScanEntry>,
    ) -> CliResult<Outcome> {
        let Some(theirs) = c.new_id else {
            return Ok(Outcome::Unchanged);
        };
        let kind = c.kind.unwrap_or(EntryKind::File);
        let row = e.and_then(|e| e.row.clone());
        let mine = match e {
            Some(e) => disk_id(ws, e)?,
            None => None,
        };
        let root = ws.root().to_path_buf();
        if mine == Some(theirs) {
            if row.as_ref().map_or(true, |r| r.state == FileState::Deleted) {
                Self::put_row(ws, c, row)?;
            }
            return Ok(Outcome::Unchanged);
        }
        // Nothing changed here since the shelf's base (or nothing is here at all).
        if mine == c.base_id {
            self.bases
                .place(&root, &c.path, &theirs, kind, c.executable)
                .await?;
            Self::put_row(ws, c, row)?;
            return Ok(Outcome::Applied);
        }
        let Some(disk) = e.and_then(|e| e.disk.as_ref()) else {
            // Deleted here, changed there.
            return self.conflict(ws, c, row, None);
        };
        let local = ws.local_path(&c.path);
        let small = disk.size <= TEXT_LIMIT && c.size.unwrap_or(0) <= TEXT_LIMIT;
        if kind == EntryKind::File && disk.kind == EntryKind::File && small {
            let base = match c.base_id {
                Some(b) => self.bases.read(&b).await?,
                None => Vec::new(),
            };
            let their_bytes = self.bases.read(&theirs).await?;
            let mine_bytes = fs::read(&local)?;
            if self.is_text(&c.path, &[&base, &mine_bytes, &their_bytes]) {
                let mut opts = diffy::MergeOptions::new();
                opts.set_conflict_style(diffy::ConflictStyle::Merge)
                    .set_incomplete_hunk_style(diffy::IncompleteHunkStyle::Git);
                return match opts.merge_bytes(&base, &mine_bytes, &their_bytes) {
                    Ok(merged) => {
                        write_through_tmp(ws, &c.path, &merged, c.executable)?;
                        Self::put_row(ws, c, row)?;
                        Ok(Outcome::Merged)
                    }
                    Err(conflicted) => {
                        let mine_id = lode_core::blob_id(&mine_bytes[..])?;
                        let keep = local::mine_path(ws, &mine_id);
                        fs::create_dir_all(ws.layout().conflicts())?;
                        if !keep.exists() {
                            fs::write(&keep, &mine_bytes)?;
                        }
                        write_through_tmp(
                            ws,
                            &c.path,
                            &relabel(conflicted, &self.label),
                            disk.executable,
                        )?;
                        self.conflict(ws, c, row, Some(mine_id))
                    }
                };
            }
        }
        // Binary or a symlink: keep the local file, put the incoming one beside it.
        if kind == EntryKind::File {
            let content = self.bases.read(&theirs).await?;
            fs::write(local::theirs_path(&local, self.theirs_rev), content)?;
        }
        self.conflict(ws, c, row, None)
    }

    /// A delete: the incoming side removes the path.
    fn delete(
        &mut self,
        ws: &mut Workspace,
        c: &Change,
        e: Option<&ScanEntry>,
    ) -> CliResult<Outcome> {
        let Some(row) = e.and_then(|e| e.row.clone()) else {
            return Ok(Outcome::Unchanged);
        };
        let mine = match e {
            Some(e) => disk_id(ws, e)?,
            None => None,
        };
        let gone = |ws: &mut Workspace, row: FileRow| -> CliResult<()> {
            if row.base_id.is_none() {
                ws.db_mut().write(|tx| tx.remove_file(&row.path))?;
            } else {
                let mut r = row;
                r.state = FileState::Deleted;
                r.mtime_ns = 0;
                ws.db_mut().write(|tx| tx.put_file(&r))?;
            }
            Ok(())
        };
        if mine.is_none() {
            if row.state == FileState::Deleted {
                return Ok(Outcome::Unchanged);
            }
            gone(ws, row)?;
            return Ok(Outcome::Applied);
        }
        if mine == c.base_id {
            remove_and_prune(ws.root(), &c.path)?;
            gone(ws, row)?;
            return Ok(Outcome::Applied);
        }
        // Changed here, deleted there: the local file stays.
        self.conflict(ws, c, Some(row), None)
    }
}

/// Applies `changes` to the workspace and returns what happened to each path. Paths outside
/// the workspace's view are skipped.
pub(crate) async fn apply_changes(
    ctx: &Ctx,
    ws: &mut Workspace,
    changes: &[Change],
    label: String,
    theirs_rev: u64,
) -> CliResult<BTreeMap<String, Outcome>> {
    let view = ws.view_spec()?.compile()?;
    let paths: Vec<DepotPath> = changes.iter().map(|c| c.path.clone()).collect();
    let report = local::scan(ctx, ws, &paths, false)?;
    let mut run = Apply {
        bases: Bases::new(ctx, ws)?,
        config: local_config(ws),
        label,
        theirs_rev,
    };
    let mut outcomes = BTreeMap::new();
    for c in changes {
        let outcome = if !view.contains(&c.path) {
            Outcome::Skipped
        } else {
            let e = report.get(&c.path);
            match c.action {
                ChangeAction::Delete => run.delete(ws, c, e)?,
                ChangeAction::Add | ChangeAction::Modify => run.put(ws, c, e).await?,
            }
        };
        outcomes.insert(c.path.as_str().to_owned(), outcome);
    }
    crate::cmd::locks::protect(ws, &paths)?;
    Ok(outcomes)
}

/// Counts outcomes by name.
pub(crate) fn count(outcomes: &BTreeMap<String, Outcome>) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for o in outcomes.values() {
        *counts.entry(o.name()).or_default() += 1;
    }
    counts
}

/// Fails while conflicts wait to be resolved.
pub(crate) fn refuse_conflicts(ws: &Workspace) -> CliResult<()> {
    let conflicts = ws.db().conflicts()?;
    if conflicts.is_empty() {
        return Ok(());
    }
    let paths: Vec<String> = conflicts
        .iter()
        .map(|c| c.path.as_str().to_owned())
        .collect();
    Err(CliError::failed(
        "conflicted",
        format!(
            "resolve these conflicts first (`lode resolve`): {}",
            paths.join(", ")
        ),
    )
    .with_details(json!({ "paths": paths })))
}
