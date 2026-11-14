//! Replaying a Subversion path's history onto a Lode branch (T121).
//!
//! Each SVN revision that changes the imported path becomes one Lode revision with the same
//! message, and provenance `imported` with system `svn`, the revision number, author and
//! date. The changes are worked out by applying the revision's changed paths to the depot's
//! files (deletes first, then adds and edits) and comparing before and after, so a replace
//! or a rename comes out as the right adds, edits and deletes. A copy from inside the
//! imported path whose content is unchanged becomes a Lode copy, so history follows renames.
//!
//! Resumable like the Git import: the head's provenance names the last imported revision,
//! and request ids are `import-svn-<rev>`, so a revision whose answer was lost is not made
//! twice.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use futures::TryStreamExt;
use lode_client::Client;
use lode_core::{Change, ChangeAction, CopyFrom, DepotPath, EntryKind, ObjectId};
use lode_proto::commit::{CommitRequest, ImportedFrom, Provenance};

use crate::svn::{SvnAction, SvnKind, SvnPath, SvnRepo, SvnRevision};
use crate::{delete, upload_blobs, Blob, ImportError};

/// What to import, and where to.
#[derive(Debug, Clone)]
pub struct SvnImport<'a> {
    /// URL of the directory whose history is imported, such as `https://svn.example.com/game/trunk`.
    pub url: &'a str,
    /// The depot.
    pub depot: &'a str,
    /// The Lode branch the revisions go to.
    pub branch: &'a str,
}

/// Progress reports while importing.
pub trait SvnProgress {
    /// About to start: `total` revisions change the path, `done` of them imported earlier.
    fn start(&mut self, _total: usize, _done: usize) {}
    /// Revision number `index` (counted from 0 over the whole history) became Lode revision
    /// `rev`; `None` when it changed nothing a depot holds.
    fn committed(&mut self, _index: usize, _svn: &SvnRevision, _rev: Option<u64>) {}
}

/// No progress reports.
impl SvnProgress for () {}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SvnReport {
    /// Revisions that change the imported path.
    pub total: usize,
    /// Revisions imported by an earlier run.
    pub already: usize,
    /// Lode revisions made by this run.
    pub imported: usize,
    /// Revisions that changed nothing a depot holds (empty directories, properties only).
    pub skipped: usize,
    /// The Lode branch head afterwards.
    pub head_rev: u64,
    /// The last SVN revision now in the depot.
    pub last_revision: Option<u64>,
    /// Directories with `svn:externals`, which are not imported.
    pub externals: Vec<String>,
}

/// A file of the depot, as far as the import needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct File {
    id: ObjectId,
    size: u64,
    executable: bool,
    symlink: bool,
}

/// Imports the history of `opts.url` into `opts.depot`.
pub async fn import_svn(
    client: &Client,
    opts: &SvnImport<'_>,
    progress: &mut dyn SvnProgress,
) -> Result<SvnReport, ImportError> {
    let repo = SvnRepo::open(opts.url).await?;
    let log = repo.log().await?;
    let head = client.branch(opts.depot, opts.branch).await?.head_rev;

    let mut last: Option<u64> = None;
    if head > 1 {
        let r = client.revision(opts.depot, head).await?;
        let imported = r
            .provenance
            .and_then(|p| p.imported)
            .filter(|i| i.system == "svn")
            .and_then(|i| i.id.parse::<u64>().ok())
            .ok_or_else(|| ImportError::DepotNotEmpty {
                depot: opts.depot.to_owned(),
                branch: opts.branch.to_owned(),
                system: "Subversion",
            })?;
        if imported > repo.youngest() {
            return Err(ImportError::SvnBehind {
                rev: imported,
                youngest: repo.youngest(),
            });
        }
        last = Some(imported);
    }
    let start = log
        .iter()
        .position(|r| last.is_none_or(|l| r.rev > l))
        .unwrap_or(log.len());
    let mut report = SvnReport {
        total: log.len(),
        already: start,
        head_rev: head,
        last_revision: last,
        externals: repo.externals().await?,
        ..SvnReport::default()
    };
    progress.start(log.len(), start);
    if start == log.len() {
        return Ok(report);
    }

    let mut state: HashMap<DepotPath, File> = HashMap::new();
    let mut manifest = client.manifest(opts.depot, head, &[], &[]).await?;
    while let Some(e) = manifest.try_next().await? {
        if e.kind == EntryKind::Dir {
            continue;
        }
        state.insert(
            e.path,
            File {
                id: e.id,
                size: e.size,
                executable: e.executable,
                symlink: e.kind == EntryKind::Symlink,
            },
        );
    }

    let tmp = tempfile::tempdir()?;
    let mut head = head;
    for (index, svn_rev) in log.iter().enumerate().skip(start) {
        let mut step = Step {
            repo: &repo,
            rev: svn_rev.rev,
            tmp: tmp.path(),
            before: &state,
            after: state.clone(),
            touched: BTreeSet::new(),
            copies: HashMap::new(),
            uploads: Vec::new(),
            serial: 0,
        };
        step.apply(&svn_rev.paths).await?;
        let (changes, after, uploads) = step.changes(head);
        let rev = if changes.is_empty() {
            report.skipped += 1;
            None
        } else {
            upload_blobs(client, opts.depot, &uploads).await?;
            let resp = client
                .commit(
                    opts.depot,
                    opts.branch,
                    &CommitRequest {
                        request_id: format!("import-svn-{}", svn_rev.rev),
                        message: svn_rev.message.clone(),
                        changes,
                        merged_from: Vec::new(),
                        provenance: Some(Provenance {
                            imported: Some(ImportedFrom {
                                system: "svn".into(),
                                id: svn_rev.rev.to_string(),
                                author: svn_rev.author.clone(),
                                date: svn_rev.date.clone(),
                            }),
                            ..Provenance::default()
                        }),
                        extensions: Default::default(),
                    },
                )
                .await?;
            head = resp.rev;
            report.imported += 1;
            report.head_rev = head;
            Some(head)
        };
        for b in &uploads {
            if let Some(source) = &b.source {
                let _ = std::fs::remove_file(source);
            }
        }
        state = after;
        report.last_revision = Some(svn_rev.rev);
        progress.committed(index, svn_rev, rev);
    }
    Ok(report)
}

/// One SVN revision being worked out.
struct Step<'a> {
    repo: &'a SvnRepo,
    rev: u64,
    tmp: &'a std::path::Path,
    /// The depot before the revision.
    before: &'a HashMap<DepotPath, File>,
    /// The depot after it, as far as applied.
    after: HashMap<DepotPath, File>,
    /// Paths the revision touched.
    touched: BTreeSet<DepotPath>,
    /// Added paths copied from another imported path.
    copies: HashMap<DepotPath, DepotPath>,
    uploads: Vec<Blob>,
    serial: u64,
}

fn join(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (true, _) => name.to_owned(),
        (_, true) => dir.to_owned(),
        _ => format!("{dir}/{name}"),
    }
}

impl Step<'_> {
    fn path(&self, rel: &str) -> Result<DepotPath, ImportError> {
        DepotPath::parse(rel).map_err(|e| ImportError::InvalidPath {
            commit: format!("r{}", self.rev),
            path: rel.to_owned(),
            reason: e.to_string(),
        })
    }

    /// Applies the revision's changed paths: deletes first, then adds and edits.
    async fn apply(&mut self, paths: &[SvnPath]) -> Result<(), ImportError> {
        let mine: Vec<(&SvnPath, String)> = paths
            .iter()
            .filter_map(|p| Some((p, self.repo.relative(&p.path)?.to_owned())))
            .collect();
        for (p, rel) in &mine {
            if matches!(p.action, SvnAction::Delete | SvnAction::Replace) {
                self.remove(rel);
            }
        }
        let mut adds: Vec<&(&SvnPath, String)> = mine
            .iter()
            .filter(|(p, _)| p.action != SvnAction::Delete)
            .collect();
        // Parents first: a file edited inside a directory copied in the same revision is
        // read after the copy.
        adds.sort_by(|a, b| a.1.cmp(&b.1));
        for (p, rel) in adds {
            let kind = match p.kind {
                SvnKind::Unknown => self.repo.kind(rel, self.rev).await?,
                k => k,
            };
            let source = p
                .copy_from
                .as_ref()
                .and_then(|(from, _)| self.repo.relative(from))
                .map(str::to_owned);
            match kind {
                SvnKind::Dir => {
                    if p.copy_from.is_some() || p.action == SvnAction::Replace {
                        for f in self.repo.files_under(rel, self.rev).await? {
                            let src = source.as_deref().map(|s| join(s, &f));
                            self.put(&join(rel, &f), src.as_deref(), true, true).await?;
                        }
                    }
                }
                SvnKind::File | SvnKind::Unknown => {
                    let fresh = p.action != SvnAction::Modify;
                    self.put(
                        rel,
                        source.as_deref(),
                        fresh || p.text_mods,
                        fresh || p.prop_mods,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Removes a file, or every file under a directory.
    fn remove(&mut self, rel: &str) {
        let prefix = format!("{rel}/");
        let gone: Vec<DepotPath> = self
            .after
            .keys()
            .filter(|p| rel.is_empty() || p.as_str() == rel || p.as_str().starts_with(&prefix))
            .cloned()
            .collect();
        for p in gone {
            self.after.remove(&p);
            self.copies.remove(&p);
            self.touched.insert(p);
        }
    }

    /// Reads a file as the revision has it.
    async fn put(
        &mut self,
        rel: &str,
        source: Option<&str>,
        read_content: bool,
        read_props: bool,
    ) -> Result<(), ImportError> {
        let path = self.path(rel)?;
        let old = self.after.get(&path).copied();
        let (executable, symlink) = match (read_props, old) {
            (false, Some(f)) => (f.executable, f.symlink),
            _ => {
                let props = self.repo.props(rel, self.rev).await?;
                (props.executable, props.special)
            }
        };
        let file = match (read_content, old) {
            (false, Some(f)) if f.symlink == symlink => File {
                executable,
                symlink,
                ..f
            },
            _ => {
                self.serial += 1;
                let dest = self.tmp.join(format!("r{}-{}", self.rev, self.serial));
                let (id, size) = self.repo.cat(rel, self.rev, symlink, &dest).await?;
                self.uploads.push(Blob {
                    id,
                    size,
                    source: Some(dest),
                });
                File {
                    id,
                    size,
                    executable: executable && !symlink,
                    symlink,
                }
            }
        };
        if let Some(src) = source {
            let src = self.path(src)?;
            // Only an unchanged copy of something the depot holds is a Lode copy.
            if self.before.get(&src).is_some_and(|f| f.id == file.id) && src != path {
                self.copies.insert(path.clone(), src);
            }
        }
        self.after.insert(path.clone(), file);
        self.touched.insert(path);
        Ok(())
    }

    /// The Lode changes: every touched path, before against after.
    fn changes(self, head: u64) -> (Vec<Change>, HashMap<DepotPath, File>, Vec<Blob>) {
        let mut out = BTreeMap::new();
        for p in &self.touched {
            let before = self.before.get(p);
            let after = self.after.get(p);
            let change = match (before, after) {
                (None, None) => continue,
                (Some(b), None) => delete(p.clone(), b.id),
                (b, Some(a)) => {
                    if b == Some(a) {
                        continue;
                    }
                    Change {
                        path: p.clone(),
                        action: if b.is_some() {
                            ChangeAction::Modify
                        } else {
                            ChangeAction::Add
                        },
                        base_id: b.map(|f| f.id),
                        new_id: Some(a.id),
                        kind: Some(if a.symlink {
                            EntryKind::Symlink
                        } else {
                            EntryKind::File
                        }),
                        executable: a.executable,
                        size: Some(a.size),
                        copy_from: match (b, self.copies.get(p)) {
                            (None, Some(src)) => Some(CopyFrom {
                                path: src.clone(),
                                rev: head,
                            }),
                            _ => None,
                        },
                    }
                }
            };
            out.insert(p.clone(), change);
        }
        (out.into_values().collect(), self.after, self.uploads)
    }
}
