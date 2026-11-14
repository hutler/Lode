//! Importing history from other version control systems (T086). Git is the first source.
//!
//! [`import_git`] replays the first-parent history of a Git branch onto a Lode branch, one
//! revision per Git commit that changes anything. Each revision keeps the commit's message,
//! and its provenance records the Git commit id, author and date ([`ImportedFrom`]); the
//! Lode author is the importing account. Renames that Git detects become Lode renames, so
//! history follows the file. Git LFS pointers are replaced by their content when it is in the
//! repository's local LFS store. Submodules are skipped.
//!
//! The import is resumable: the provenance of the branch head names the last imported
//! commit, and a rerun continues after it. Commit request ids are derived from the Git
//! commit id, so a commit whose answer was lost is not made twice.

#![forbid(unsafe_code)]

mod git;
mod lfs;
mod svn;
mod svn_import;
mod xml;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::{StreamExt, TryStreamExt};
use lode_client::Client;
use lode_core::{Change, ChangeAction, CopyFrom, DepotPath, EntryKind, ObjectId};
use lode_proto::commit::{CommitRequest, ImportedFrom, Provenance};

pub use git::{GitChange, GitCommit, GitRepo, Mode};
pub use svn::{SvnAction, SvnKind, SvnPath, SvnProps, SvnRepo, SvnRevision};
pub use svn_import::{import_svn, SvnImport, SvnProgress, SvnReport};

/// Parallel blob uploads.
const PARALLEL_UPLOADS: usize = 4;

/// Why an import stopped.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The `git` command cannot be run.
    #[error("cannot run `git`; install Git and make sure it is on the PATH")]
    GitMissing,
    /// The folder is not in a Git repository.
    #[error("{} is not a Git repository", .0.display())]
    NotARepository(PathBuf),
    /// The Git branch does not exist.
    #[error("the Git repository has no branch or commit `{0}`")]
    UnknownBranch(String),
    /// A `git` command failed.
    #[error("`{command}` failed: {message}")]
    Git {
        /// The command.
        command: String,
        /// Its error output.
        message: String,
    },
    /// Git printed something this importer does not understand.
    #[error("unexpected output from git: {0}")]
    Protocol(String),
    /// The Lode branch has revisions that did not come from this import.
    #[error(
        "branch {branch} of depot {depot} already has revisions that were not imported from {system}"
    )]
    DepotNotEmpty {
        /// Depot.
        depot: String,
        /// Lode branch.
        branch: String,
        /// `Git` or `Subversion`.
        system: &'static str,
    },
    /// The `svn` command cannot be run.
    #[error("cannot run `svn`; install Subversion's command-line client and make sure it is on the PATH")]
    SvnMissing,
    /// The URL is not a directory of a Subversion repository, or cannot be reached.
    #[error("{url} is not a Subversion directory: {message}")]
    NotSvn {
        /// The URL given.
        url: String,
        /// What svn said.
        message: String,
    },
    /// An `svn` command failed.
    #[error("`{command}` failed: {message}")]
    Svn {
        /// The command.
        command: String,
        /// Its error output.
        message: String,
    },
    /// The depot was imported from a later Subversion revision than the repository has.
    #[error(
        "the depot was imported up to r{rev} of Subversion, but the repository ends at r{youngest}"
    )]
    SvnBehind {
        /// The last imported revision.
        rev: u64,
        /// The newest revision of the repository.
        youngest: u64,
    },
    /// The last imported commit is not in the Git branch's history.
    #[error("the depot was imported up to Git commit {commit}, which is not in the first-parent history of `{branch}`")]
    Diverged {
        /// The last imported commit.
        commit: String,
        /// The Git branch.
        branch: String,
    },
    /// A Git path is not allowed in a depot.
    #[error("Git commit {commit} has the path `{path}`, which a depot cannot hold: {reason}")]
    InvalidPath {
        /// The commit.
        commit: String,
        /// The path.
        path: String,
        /// Why.
        reason: String,
    },
    /// The Lode branch head does not hold what the Git history says it should.
    #[error("the depot does not match Git commit {commit} at `{path}`")]
    Mismatch {
        /// The Git commit being imported.
        commit: String,
        /// The path.
        path: String,
    },
    /// The server refused or could not be reached.
    #[error(transparent)]
    Client(#[from] lode_client::Error),
    /// Local I/O.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// What to import, and where to.
#[derive(Debug, Clone)]
pub struct GitImport<'a> {
    /// A folder in the Git repository.
    pub repo: &'a Path,
    /// The Git branch (or any commit-ish) whose first-parent history is imported.
    pub git_branch: &'a str,
    /// The depot.
    pub depot: &'a str,
    /// The Lode branch the revisions go to.
    pub branch: &'a str,
}

/// Progress reports while importing.
pub trait Progress {
    /// The import is about to start: `total` commits on the Git branch, `done` of them
    /// imported earlier.
    fn start(&mut self, _total: usize, _done: usize) {}
    /// Commit number `index` (counted from 0 over the whole branch) became revision `rev`;
    /// `None` when it changed nothing a depot holds and was skipped.
    fn committed(&mut self, _index: usize, _commit: &GitCommit, _rev: Option<u64>) {}
}

/// No progress reports.
impl Progress for () {}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Commits in the Git branch's first-parent history.
    pub total: usize,
    /// Commits imported by an earlier run.
    pub already: usize,
    /// Revisions made by this run.
    pub imported: usize,
    /// Commits that changed nothing a depot holds (empty commits, submodule-only changes).
    pub skipped: usize,
    /// The Lode branch head afterwards.
    pub head_rev: u64,
    /// The last Git commit now in the depot.
    pub last_commit: Option<String>,
    /// LFS pointers replaced by their content.
    pub lfs_replaced: usize,
    /// Paths whose LFS content is not in the local store; they were imported as pointers.
    pub lfs_missing: Vec<String>,
    /// Submodule paths, which are not imported.
    pub submodules: Vec<String>,
}

/// A file of the depot head, as far as the import needs it.
type State = HashMap<DepotPath, ObjectId>;

/// A blob the import has the id of.
#[derive(Debug, Clone)]
struct Blob {
    id: ObjectId,
    size: u64,
    /// Where the content is until uploaded.
    source: Option<PathBuf>,
}

/// Imports the first-parent history of `opts.git_branch` into `opts.depot`.
pub async fn import_git(
    client: &Client,
    opts: &GitImport<'_>,
    progress: &mut dyn Progress,
) -> Result<ImportReport, ImportError> {
    let repo = GitRepo::open(opts.repo).await?;
    let tip = repo.resolve(opts.git_branch).await?;
    let commits = repo.first_parent(&tip).await?;
    let head = client.branch(opts.depot, opts.branch).await?.head_rev;

    // Where an earlier run stopped: the head revision names its Git commit.
    let mut start = 0;
    let mut prev: Option<String> = None;
    if head > 1 {
        let r = client.revision(opts.depot, head).await?;
        let last = r
            .provenance
            .and_then(|p| p.imported)
            .filter(|i| i.system == "git")
            .ok_or_else(|| ImportError::DepotNotEmpty {
                depot: opts.depot.to_owned(),
                branch: opts.branch.to_owned(),
                system: "Git",
            })?;
        let at = commits
            .iter()
            .position(|c| c.id == last.id)
            .ok_or_else(|| ImportError::Diverged {
                commit: last.id.clone(),
                branch: opts.git_branch.to_owned(),
            })?;
        start = at + 1;
        prev = Some(last.id);
    }
    let mut report = ImportReport {
        total: commits.len(),
        already: start,
        head_rev: head,
        last_commit: prev.clone(),
        ..ImportReport::default()
    };
    progress.start(commits.len(), start);
    if start == commits.len() {
        return Ok(report);
    }

    let mut state: State = HashMap::new();
    let mut manifest = client.manifest(opts.depot, head, &[], &[]).await?;
    while let Some(e) = manifest.try_next().await? {
        state.insert(e.path, e.id);
    }

    let mut run = Run {
        client,
        depot: opts.depot,
        repo: &repo,
        reader: repo.blobs()?,
        tmp: tempfile::tempdir()?,
        known: HashMap::new(),
        report: &mut report,
    };
    let mut head = head;
    for (index, commit) in commits.iter().enumerate().skip(start) {
        let changes = repo.changes(prev.as_deref(), &commit.id).await?;
        let (lode, uploads) = run.translate(commit, &changes, &state, head).await?;
        let rev = if lode.is_empty() {
            run.report.skipped += 1;
            None
        } else {
            run.upload(&uploads).await?;
            let resp = client
                .commit(
                    opts.depot,
                    opts.branch,
                    &CommitRequest {
                        request_id: format!("import-git-{}", commit.id),
                        message: commit.message.clone(),
                        changes: lode.clone(),
                        merged_from: Vec::new(),
                        provenance: Some(Provenance {
                            imported: Some(ImportedFrom {
                                system: "git".into(),
                                id: commit.id.clone(),
                                author: Some(format!(
                                    "{} <{}>",
                                    commit.author_name, commit.author_email
                                )),
                                date: Some(commit.author_date.clone()),
                            }),
                            ..Provenance::default()
                        }),
                        extensions: Default::default(),
                    },
                )
                .await?;
            for c in &lode {
                match c.new_id {
                    Some(id) => state.insert(c.path.clone(), id),
                    None => state.remove(&c.path),
                };
            }
            run.forget_sources(&uploads);
            head = resp.rev;
            run.report.imported += 1;
            run.report.head_rev = head;
            run.report.last_commit = Some(commit.id.clone());
            Some(head)
        };
        progress.committed(index, commit, rev);
        prev = Some(commit.id.clone());
    }
    Ok(report)
}

/// One run's working state.
struct Run<'a> {
    client: &'a Client,
    depot: &'a str,
    repo: &'a GitRepo,
    reader: git::BlobReader,
    tmp: tempfile::TempDir,
    /// Git blob id → Lode blob, for content met before.
    known: HashMap<String, Blob>,
    report: &'a mut ImportReport,
}

impl Run<'_> {
    fn path(&self, commit: &GitCommit, path: &str) -> Result<DepotPath, ImportError> {
        DepotPath::parse(path).map_err(|e| ImportError::InvalidPath {
            commit: commit.id.clone(),
            path: path.to_owned(),
            reason: e.to_string(),
        })
    }

    /// The Lode blob for Git blob `blob` at `path`, extracting it when new.
    async fn blob(&mut self, blob: &str, mode: Mode, path: &str) -> Result<Blob, ImportError> {
        if let Some(b) = self.known.get(blob) {
            return Ok(b.clone());
        }
        let file = self.tmp.path().join(blob);
        let mut hasher = blake3::Hasher::new();
        let mut out = tokio::fs::File::create(&file).await?;
        let size = self.reader.copy(blob, &mut out, &mut hasher).await?;
        drop(out);
        let mut b = Blob {
            id: ObjectId::from_bytes(*hasher.finalize().as_bytes()),
            size,
            source: Some(file.clone()),
        };
        if mode != Mode::Symlink && size <= lfs::MAX_POINTER_SIZE {
            let text = tokio::fs::read(&file).await?;
            if let Some(pointer) = lfs::parse(&text) {
                match lfs::local_object(self.repo.git_dir(), &pointer) {
                    Some(object) => {
                        let o = object.clone();
                        let id = tokio::task::spawn_blocking(move || {
                            lode_core::blob_id(std::fs::File::open(o)?)
                        })
                        .await
                        .map_err(|e| std::io::Error::other(e.to_string()))??;
                        let _ = tokio::fs::remove_file(&file).await;
                        b = Blob {
                            id,
                            size: pointer.size,
                            source: Some(object),
                        };
                        self.report.lfs_replaced += 1;
                    }
                    None => self.report.lfs_missing.push(path.to_owned()),
                }
            }
        }
        self.known.insert(blob.to_owned(), b.clone());
        Ok(b)
    }

    /// An add or modify of `path` to Git blob `blob`.
    async fn content(
        &mut self,
        path: DepotPath,
        mode: Mode,
        blob: &str,
        base: Option<ObjectId>,
        uploads: &mut Vec<Blob>,
    ) -> Result<Change, ImportError> {
        let b = self.blob(blob, mode, path.as_str()).await?;
        if b.source.is_some() {
            uploads.push(b.clone());
        }
        Ok(Change {
            path,
            action: if base.is_some() {
                ChangeAction::Modify
            } else {
                ChangeAction::Add
            },
            base_id: base,
            new_id: Some(b.id),
            kind: Some(if mode == Mode::Symlink {
                EntryKind::Symlink
            } else {
                EntryKind::File
            }),
            executable: mode == Mode::Executable,
            size: Some(b.size),
            copy_from: None,
        })
    }

    /// The Lode changes for one Git commit, and the blobs they need uploaded.
    async fn translate(
        &mut self,
        commit: &GitCommit,
        changes: &[GitChange],
        state: &State,
        head: u64,
    ) -> Result<(Vec<Change>, Vec<Blob>), ImportError> {
        let mut out = Vec::new();
        let mut uploads = Vec::new();
        let mismatch = |path: &DepotPath| ImportError::Mismatch {
            commit: commit.id.clone(),
            path: path.to_string(),
        };
        for change in changes {
            match change {
                GitChange::Add { path, mode, blob } => {
                    if !mode.has_content() {
                        self.submodule(path);
                        continue;
                    }
                    let p = self.path(commit, path)?;
                    if state.contains_key(&p) {
                        return Err(mismatch(&p));
                    }
                    out.push(self.content(p, *mode, blob, None, &mut uploads).await?);
                }
                GitChange::Modify {
                    path,
                    old_mode,
                    mode,
                    blob,
                } => {
                    let p = self.path(commit, path)?;
                    match (old_mode.has_content(), mode.has_content()) {
                        (false, false) => self.submodule(path),
                        (false, true) => {
                            self.submodule(path);
                            out.push(self.content(p, *mode, blob, None, &mut uploads).await?);
                        }
                        (true, false) => {
                            self.submodule(path);
                            let base = *state.get(&p).ok_or_else(|| mismatch(&p))?;
                            out.push(delete(p, base));
                        }
                        (true, true) => {
                            let base = *state.get(&p).ok_or_else(|| mismatch(&p))?;
                            out.push(
                                self.content(p, *mode, blob, Some(base), &mut uploads)
                                    .await?,
                            );
                        }
                    }
                }
                GitChange::Delete { path, old_mode } => {
                    if !old_mode.has_content() {
                        self.submodule(path);
                        continue;
                    }
                    let p = self.path(commit, path)?;
                    let base = *state.get(&p).ok_or_else(|| mismatch(&p))?;
                    out.push(delete(p, base));
                }
                GitChange::Rename {
                    from,
                    old_mode,
                    path,
                    mode,
                    blob,
                } => {
                    if old_mode.has_content() {
                        let f = self.path(commit, from)?;
                        let base = *state.get(&f).ok_or_else(|| mismatch(&f))?;
                        out.push(delete(f, base));
                    } else {
                        self.submodule(from);
                    }
                    if mode.has_content() {
                        let p = self.path(commit, path)?;
                        let mut c = self.content(p, *mode, blob, None, &mut uploads).await?;
                        if old_mode.has_content() {
                            c.copy_from = Some(CopyFrom {
                                path: self.path(commit, from)?,
                                rev: head,
                            });
                        }
                        out.push(c);
                    } else {
                        self.submodule(path);
                    }
                }
            }
        }
        Ok((out, uploads))
    }

    fn submodule(&mut self, path: &str) {
        if !self.report.submodules.iter().any(|p| p == path) {
            self.report.submodules.push(path.to_owned());
        }
    }

    /// Uploads the blobs the server lacks.
    async fn upload(&self, blobs: &[Blob]) -> Result<(), ImportError> {
        upload_blobs(self.client, self.depot, blobs).await
    }

    /// After a commit: the server has these blobs, so their extracted copies can go.
    fn forget_sources(&mut self, blobs: &[Blob]) {
        for b in blobs {
            let Some(source) = &b.source else { continue };
            if source.starts_with(self.tmp.path()) {
                let _ = std::fs::remove_file(source);
            }
        }
        for b in self.known.values_mut() {
            b.source = None;
        }
    }
}

/// Uploads the blobs among `blobs` that the server lacks.
async fn upload_blobs(client: &Client, depot: &str, blobs: &[Blob]) -> Result<(), ImportError> {
    let mut by_id: HashMap<ObjectId, &Path> = HashMap::new();
    for b in blobs {
        if let Some(source) = &b.source {
            by_id.entry(b.id).or_insert(source);
        }
    }
    let ids: Vec<ObjectId> = by_id.keys().copied().collect();
    if ids.is_empty() {
        return Ok(());
    }
    let lacking = client.missing_objects(depot, &ids).await?;
    let uploads = futures::stream::iter(lacking.into_iter().filter_map(|id| {
        let source = by_id.get(&id)?.to_path_buf();
        let client = client.clone();
        let depot = depot.to_owned();
        Some(async move { client.put_blob_file(&depot, &id, &source).await })
    }))
    .buffer_unordered(PARALLEL_UPLOADS);
    futures::pin_mut!(uploads);
    while let Some(r) = uploads.next().await {
        r?;
    }
    Ok(())
}

fn delete(path: DepotPath, base: ObjectId) -> Change {
    Change {
        path,
        action: ChangeAction::Delete,
        base_id: Some(base),
        new_id: None,
        kind: None,
        executable: false,
        size: None,
        copy_from: None,
    }
}
