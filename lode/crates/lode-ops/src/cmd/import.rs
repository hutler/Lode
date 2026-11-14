//! `lode import git <repo> --branch <b> <server-url>/<depot>` (T086): moves a Git branch's
//! first-parent history into a depot, one revision per Git commit. The depot is created when
//! it does not exist (administrators only). A rerun continues after the last imported commit.

use std::path::PathBuf;

use lode_import::{GitCommit, GitImport, ImportError, Progress};
use serde_json::{json, Value};

use crate::cmd::checkout::split_target;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};

/// `lode import` sources.
#[derive(Debug, clap::Subcommand)]
pub enum ImportCmd {
    /// Import a Git branch's history.
    Git(GitArgs),
}

/// `lode import git` options.
#[derive(Debug, Default, clap::Args)]
pub struct GitArgs {
    /// A folder in the Git repository.
    pub repo: PathBuf,
    /// `<server-url>/<depot>`, or a depot name with --server or LODE_SERVER.
    pub target: String,
    /// The Git branch to import; its first-parent history becomes the depot's `main`.
    #[arg(long, short = 'b')]
    pub branch: String,
}

struct Report<'a> {
    ctx: &'a Ctx,
    bar: Option<crate::output::Progress>,
}

impl Progress for Report<'_> {
    fn start(&mut self, total: usize, done: usize) {
        if done > 0 && done < total {
            self.ctx.out.line(format!(
                "Resuming: {done} of {total} commits were imported earlier."
            ));
        }
        self.bar = Some(
            self.ctx
                .out
                .progress("Importing commits", total.saturating_sub(done) as u64),
        );
    }

    fn committed(&mut self, index: usize, _commit: &GitCommit, _rev: Option<u64>) {
        if let Some(bar) = &mut self.bar {
            bar.step(0);
        }
        crate::crash::point(&format!("import.after.{}", index + 1));
    }
}

fn cli_error(e: ImportError) -> OpError {
    match e {
        ImportError::Client(c) => c.into(),
        ImportError::Io(io) => io.into(),
        other => {
            let code = match &other {
                ImportError::GitMissing => "git_not_found",
                ImportError::NotARepository(_) => "not_a_git_repository",
                ImportError::UnknownBranch(_) => "unknown_git_branch",
                ImportError::DepotNotEmpty { .. } => "depot_not_empty",
                ImportError::Diverged { .. } => "import_diverged",
                ImportError::InvalidPath { .. } => "invalid_path",
                ImportError::Mismatch { .. } => "import_mismatch",
                _ => "git_failed",
            };
            OpError::failed(code, other.to_string())
        }
    }
}

/// Runs `lode import`.
pub async fn import(ctx: &Ctx, cmd: ImportCmd) -> OpResult<Value> {
    let ImportCmd::Git(args) = cmd;
    let (server, depot) = split_target(ctx, &args.target)?;
    let repo = ctx.local(&args.repo);
    let client = ctx.client(&server)?;
    match client.create_depot(&depot).await {
        Ok(_) => ctx.out.line(format!("Created depot {depot}.")),
        // Continuing an import, or importing into an empty depot made earlier.
        Err(lode_client::Error::Api(e)) if e.status == 409 => {}
        Err(e) => return Err(e.into()),
    }
    let mut progress = Report { ctx, bar: None };
    let report = lode_import::import_git(
        &client,
        &GitImport {
            repo: &repo,
            git_branch: &args.branch,
            depot: &depot,
            branch: "main",
        },
        &mut progress,
    )
    .await
    .map_err(cli_error)?;
    if let Some(bar) = &mut progress.bar {
        bar.finish();
    }

    ctx.out.line(format!(
        "Imported {} commits of {} into {server}/{depot}; the head is r{}.",
        report.imported, args.branch, report.head_rev
    ));
    if report.skipped > 0 {
        ctx.out.line(format!(
            "Skipped {} commits that change nothing a depot holds (empty or submodule-only).",
            report.skipped
        ));
    }
    if report.lfs_replaced > 0 {
        ctx.out.line(format!(
            "Replaced {} Git LFS pointers with their content.",
            report.lfs_replaced
        ));
    }
    if !report.lfs_missing.is_empty() {
        ctx.out.line(format!(
            "The LFS content of these files is not in the local store, so they were imported as pointers (run `git lfs fetch --all` and import again into a new depot to include it): {}",
            report.lfs_missing.join(", ")
        ));
    }
    if !report.submodules.is_empty() {
        ctx.out.line(format!(
            "Submodules are not imported: {}",
            report.submodules.join(", ")
        ));
    }
    Ok(json!({
        "depot": depot,
        "server": server.as_str(),
        "git_branch": args.branch,
        "total": report.total,
        "already": report.already,
        "imported": report.imported,
        "skipped": report.skipped,
        "rev": report.head_rev,
        "last_commit": report.last_commit,
        "lfs_replaced": report.lfs_replaced,
        "lfs_missing": report.lfs_missing,
        "submodules": report.submodules,
    }))
}
