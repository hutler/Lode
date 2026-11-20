//! Applying changes made elsewhere to the workspace: `unshelve` and `merge`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;

use lode_core::{Change, ChangeAction, CopyFrom, DepotPath, EntryKind, LodeConfig, ObjectId};
use lode_workspace::classify::{class_by_config, class_by_content};
use lode_workspace::{ConflictRow, FileClass, FileRow, FileState, ScanEntry, Workspace};
use serde_json::json;

use crate::cmd::diff::TEXT_LIMIT;
use crate::cmd::update::{disk_id, local_config, relabel, write_through_tmp};
use crate::context::Ctx;
use crate::error::{OpError, OpResult};
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
    /// Renamed differently on both sides: the incoming name waits in conflict.
    RenameConflict,
}

impl Outcome {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Merged => "merged",
            Outcome::Conflicted => "conflicted",
            Outcome::Unchanged => "unchanged",
            Outcome::Skipped => "skipped",
            Outcome::RenameConflict => "rename_conflict",
        }
    }
}

/// Renames, old path → new path.
pub(crate) type Renames = BTreeMap<DepotPath, DepotPath>;

/// What applying changes did: an outcome per path, and the paths whose change arrived under
/// another name (path → the name the change was made to).
#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub outcomes: BTreeMap<String, Outcome>,
    pub from: BTreeMap<String, String>,
}

/// The renames this workspace has not committed: an added row copied from a path whose own
/// row is deleted or gone.
pub(crate) fn local_renames(ws: &Workspace) -> OpResult<Renames> {
    let rows = ws.db().files()?;
    let by_path: HashMap<&DepotPath, &FileRow> = rows.iter().map(|r| (&r.path, r)).collect();
    let mut out = Renames::new();
    for r in &rows {
        let Some(cf) = &r.copy_from else { continue };
        if !matches!(r.state, FileState::Added | FileState::Conflicted) || r.base_id.is_some() {
            continue;
        }
        if by_path
            .get(&cf.path)
            .is_none_or(|o| o.state == FileState::Deleted)
        {
            out.insert(cf.path.clone(), r.path.clone());
        }
    }
    Ok(out)
}

/// `first` followed by `then`: `a→b` and `b→c` give `a→c`.
pub(crate) fn compose(first: &Renames, then: &Renames) -> Renames {
    let mut out: Renames = first
        .iter()
        .map(|(old, new)| (old.clone(), then.get(new).unwrap_or(new).clone()))
        .collect();
    let moved: HashSet<&DepotPath> = first.values().collect();
    for (old, new) in then {
        if !moved.contains(old) {
            out.insert(old.clone(), new.clone());
        }
    }
    out.retain(|old, new| old != new);
    out
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
    fn put_row(ws: &mut Workspace, c: &Change, row: Option<FileRow>) -> OpResult<()> {
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
    ) -> OpResult<Outcome> {
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
    ) -> OpResult<Outcome> {
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
            if row.as_ref().is_none_or(|r| r.state == FileState::Deleted) {
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
                return match crate::structured::merge3(
                    &c.path,
                    self.config.as_ref(),
                    &base,
                    &mine_bytes,
                    &their_bytes,
                ) {
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

    /// A rename there of a file edited here: the local file moves to the new name (the old row
    /// is marked deleted, the new one added as its copy), then the incoming content is merged
    /// into it with the old base as common ancestor.
    async fn carry_over(
        &mut self,
        ctx: &Ctx,
        ws: &mut Workspace,
        c: &Change,
        del: &Change,
        old: &DepotPath,
    ) -> OpResult<Outcome> {
        let from = ws.local_path(old);
        let to = ws.local_path(&c.path);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        crate::materialize::remove_and_prune(ws.root(), old)?;
        if let Some(mut r) = ws.db().file(old)? {
            r.state = FileState::Deleted;
            r.mtime_ns = 0;
            ws.db_mut().write(|tx| tx.put_file(&r))?;
        }
        let mut row = crate::cmd::files::added_row(&to, c.path.clone())?;
        row.copy_from = match &c.copy_from {
            Some(cf) => Some(cf.clone()),
            None => Some(CopyFrom {
                path: old.clone(),
                rev: local::synced_rev(ws)?,
            }),
        };
        row.mtime_ns = 0;
        ws.db_mut().write(|tx| tx.put_file(&row))?;
        let report = local::scan(ctx, ws, std::slice::from_ref(&c.path), false)?;
        let mut moved = c.clone();
        moved.action = ChangeAction::Modify;
        moved.base_id = del.base_id;
        self.put(ws, &moved, report.get(&c.path)).await
    }

    /// A delete: the incoming side removes the path.
    fn delete(
        &mut self,
        ws: &mut Workspace,
        c: &Change,
        e: Option<&ScanEntry>,
    ) -> OpResult<Outcome> {
        let Some(row) = e.and_then(|e| e.row.clone()) else {
            return Ok(Outcome::Unchanged);
        };
        let mine = match e {
            Some(e) => disk_id(ws, e)?,
            None => None,
        };
        let gone = |ws: &mut Workspace, row: FileRow| -> OpResult<()> {
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
///
/// Renames are followed (ARCHITECTURE §10.5): an add with `copy_from` whose source the same
/// changes delete is a rename there, and `here` lists renames on this side (old → new; the
/// workspace's uncommitted ones are added). A change to a file renamed on the other side
/// carries local edits to the new name; a change to a file renamed here lands on the local
/// name; renamed differently on both sides, the incoming name waits as a conflict.
pub(crate) async fn apply_changes(
    ctx: &Ctx,
    ws: &mut Workspace,
    changes: &[Change],
    label: String,
    theirs_rev: u64,
    here: &Renames,
) -> OpResult<Applied> {
    let view = ws.view_spec()?.compile()?;
    let here = compose(here, &local_renames(ws)?);
    // Only renames whose new name this workspace has.
    let tracked: HashSet<DepotPath> = ws
        .db()
        .files()?
        .into_iter()
        .filter(|r| r.state != FileState::Deleted)
        .map(|r| r.path)
        .collect();
    let here: Renames = here
        .into_iter()
        .filter(|(_, new)| tracked.contains(new))
        .collect();
    let deletes: HashMap<&DepotPath, &Change> = changes
        .iter()
        .filter(|c| c.action == ChangeAction::Delete)
        .map(|c| (&c.path, c))
        .collect();
    // Renames there: new path → the deleted old path.
    let there: HashMap<&DepotPath, &DepotPath> = changes
        .iter()
        .filter(|c| c.action == ChangeAction::Add)
        .filter_map(|c| {
            let cf = c.copy_from.as_ref()?;
            (cf.path != c.path && deletes.contains_key(&cf.path)).then_some((&c.path, &cf.path))
        })
        .collect();
    let paired: HashSet<&DepotPath> = there.values().copied().collect();
    let mut paths: Vec<DepotPath> = changes.iter().map(|c| c.path.clone()).collect();
    paths.extend(changes.iter().filter_map(|c| here.get(&c.path)).cloned());
    paths.extend(there.values().filter_map(|old| here.get(*old)).cloned());
    let report = local::scan(ctx, ws, &paths, false)?;
    let mut run = Apply {
        bases: Bases::new(ctx, ws)?,
        config: local_config(ws),
        label,
        theirs_rev,
    };
    let mut out = Applied::default();
    for c in changes {
        let name = c.path.as_str().to_owned();
        if !view.contains(&c.path) {
            out.outcomes.insert(name, Outcome::Skipped);
            continue;
        }
        let e = report.get(&c.path);
        // The old half of a rename there is settled with its new half.
        if c.action == ChangeAction::Delete && paired.contains(&c.path) {
            continue;
        }
        if let Some(old) = there.get(&c.path).copied() {
            let del = deletes[old];
            let old_e = report.get(old);
            match here.get(old) {
                // The same rename on both sides.
                Some(b) if b == &c.path => {
                    let mut cb = c.clone();
                    cb.action = ChangeAction::Modify;
                    cb.base_id = del.base_id;
                    let o = run.put(ws, &cb, e).await?;
                    out.outcomes.insert(name, o);
                    out.outcomes
                        .insert(old.as_str().to_owned(), Outcome::Unchanged);
                }
                // Renamed differently: edits go to the local name, the incoming name waits.
                Some(b) => {
                    let mut cb = c.clone();
                    cb.path = b.clone();
                    cb.action = ChangeAction::Modify;
                    cb.base_id = del.base_id;
                    cb.copy_from = None;
                    let ob = run.put(ws, &cb, report.get(b)).await?;
                    out.outcomes.insert(b.as_str().to_owned(), ob);
                    out.from
                        .insert(b.as_str().to_owned(), old.as_str().to_owned());
                    let mut ca = c.clone();
                    ca.base_id = None;
                    run.conflict(ws, &ca, None, None)?;
                    out.outcomes.insert(name.clone(), Outcome::RenameConflict);
                    out.from.insert(name, old.as_str().to_owned());
                    out.outcomes
                        .insert(old.as_str().to_owned(), Outcome::Unchanged);
                }
                None => {
                    let mine_old = match old_e {
                        Some(oe) => disk_id(ws, oe)?,
                        None => None,
                    };
                    let target_free = e.is_none_or(|e| e.disk.is_none() && e.row.is_none());
                    if mine_old.is_some() && mine_old != del.base_id && target_free {
                        // Edited here: move the local file to the new name, merge there.
                        let o = run.carry_over(ctx, ws, c, del, old).await?;
                        out.outcomes
                            .insert(old.as_str().to_owned(), Outcome::Applied);
                        out.outcomes.insert(name.clone(), o);
                        out.from.insert(name, old.as_str().to_owned());
                    } else {
                        let od = run.delete(ws, del, old_e)?;
                        out.outcomes.insert(old.as_str().to_owned(), od);
                        let o = run.put(ws, c, e).await?;
                        out.outcomes.insert(name.clone(), o);
                        out.from.insert(name, old.as_str().to_owned());
                    }
                }
            }
            continue;
        }
        // Renamed here, changed there: the change lands on the local name.
        if c.action != ChangeAction::Delete {
            if let Some(b) = here.get(&c.path) {
                let gone_here = e.is_none_or(|e| e.disk.is_none());
                if gone_here && view.contains(b) {
                    let mut cb = c.clone();
                    cb.path = b.clone();
                    cb.action = ChangeAction::Modify;
                    let o = run.put(ws, &cb, report.get(b)).await?;
                    out.outcomes.insert(b.as_str().to_owned(), o);
                    out.from.insert(b.as_str().to_owned(), name);
                    continue;
                }
            }
        }
        let outcome = match c.action {
            ChangeAction::Delete => run.delete(ws, c, e)?,
            ChangeAction::Add | ChangeAction::Modify => run.put(ws, c, e).await?,
        };
        out.outcomes.insert(name, outcome);
    }
    let mut touched = paths;
    touched.extend(out.outcomes.keys().filter_map(|p| DepotPath::parse(p).ok()));
    crate::cmd::locks::protect(ws, &touched)?;
    Ok(out)
}

/// The `files` list of a JSON result: `{path, outcome, from?, tool?}`; `tool` is the merge
/// tool the depot names for a conflicted file (`lode resolve --tool`).
pub(crate) fn file_list(a: &Applied, config: Option<&LodeConfig>) -> Vec<serde_json::Value> {
    a.outcomes
        .iter()
        .map(|(p, o)| {
            let mut v = json!({ "path": p, "outcome": o.name() });
            if let Some(f) = a.from.get(p) {
                v["from"] = json!(f);
            }
            if matches!(o, Outcome::Conflicted | Outcome::RenameConflict) {
                if let Some(t) = DepotPath::parse(p)
                    .ok()
                    .and_then(|dp| crate::cmd::mergetool::tool_for(config, &dp))
                {
                    v["tool"] = json!(t);
                }
            }
            v
        })
        .collect()
}

/// Prints the paths that did not simply apply.
pub(crate) fn print_outcomes(ctx: &Ctx, a: &Applied) {
    for (p, o) in &a.outcomes {
        let from = a
            .from
            .get(p)
            .map(|f| format!(" (from {f})"))
            .unwrap_or_default();
        if *o != Outcome::Applied || !from.is_empty() {
            ctx.out.line(format!("{:<15} {p}{from}", o.name()));
        }
    }
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
pub(crate) fn refuse_conflicts(ws: &Workspace) -> OpResult<()> {
    let conflicts = ws.db().conflicts()?;
    if conflicts.is_empty() {
        return Ok(());
    }
    let paths: Vec<String> = conflicts
        .iter()
        .map(|c| c.path.as_str().to_owned())
        .collect();
    Err(OpError::failed(
        "conflicted",
        format!(
            "resolve these conflicts first (`lode resolve`): {}",
            paths.join(", ")
        ),
    )
    .with_details(json!({ "paths": paths })))
}
