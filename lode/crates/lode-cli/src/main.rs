//! `lode`, the Lode command line client.
//!
//! Human output goes to stdout and diagnostics to stderr. With `--json` stdout carries
//! exactly one JSON document, `{"ok":true,…}` or `{"ok":false,"error":{…}}`. Exit codes follow
//! SPEC §9: 0 success, 1 operation failed, 2 usage, 3 authentication or permission, 4
//! network or server.

#![forbid(unsafe_code)]

mod cmd;
mod context;
mod error;
mod external;
mod local;
mod materialize;
mod output;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use crate::context::{Ctx, Globals};
use crate::error::{CliError, CliResult};

/// Lode version control client.
#[derive(Parser, Debug)]
#[command(name = "lode", about, disable_version_flag = true)]
struct Cli {
    /// Print exactly one JSON document to stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Server URL, overriding the workspace's and LODE_SERVER.
    #[arg(long, global = true, value_name = "URL")]
    server: Option<String>,
    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "DIR")]
    chdir: Option<PathBuf>,
    /// Allow plain HTTP to hosts other than this machine.
    #[arg(long, global = true)]
    insecure: bool,
    /// Print the version.
    #[arg(long, short = 'V')]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Sign in to a server and remember the token.
    Login(cmd::login::LoginArgs),
    /// Forget (and revoke) the stored token.
    Logout,
    /// Show who you are signed in as.
    Whoami,
    /// Create a workspace from a depot.
    Checkout(cmd::checkout::CheckoutArgs),
    /// Show or change which paths are checked out.
    #[command(subcommand)]
    View(cmd::view::ViewCmd),
    /// Show changed files.
    Status(cmd::status::StatusArgs),
    /// Start tracking files.
    Add(cmd::files::AddArgs),
    /// Delete files (or stop tracking them with --keep).
    Rm(cmd::files::RmArgs),
    /// Move or rename files, keeping their history.
    Mv(cmd::files::MoveArgs),
    /// Copy files, recording where the copy came from.
    Cp(cmd::files::MoveArgs),
    /// Discard local changes.
    Revert(cmd::revert::RevertArgs),
    /// Show local changes as a unified diff.
    Diff(cmd::diff::DiffArgs),
    /// Delete untracked files.
    Clean(cmd::revert::CleanArgs),
    /// Send local changes to the server as a new revision.
    Commit(cmd::commit::CommitArgs),
    /// Bring the workspace to a newer (or older) revision, merging local changes.
    Update(cmd::update::UpdateArgs),
    /// Settle a conflict left by an update.
    Resolve(cmd::update::ResolveArgs),
    /// Take exclusive locks on files.
    Lock(cmd::locks::LockArgs),
    /// Release locks.
    Unlock(cmd::locks::UnlockArgs),
    /// List the locks of this branch.
    Locks(cmd::locks::LocksArgs),
    /// Group pending changes into named changelists.
    Changelist(cmd::changelist::ChangelistArgs),
    /// Show revision history.
    Log(cmd::history::LogArgs),
    /// Print a file as of a revision.
    Cat(cmd::history::CatArgs),
    /// List a directory as of a revision.
    Ls(cmd::history::LsArgs),
    /// Any other command runs `lode-<command>` from PATH.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

async fn run(cli: Cli) -> CliResult<Value> {
    let ctx = Ctx::new(Globals {
        json: cli.json,
        server: cli.server,
        dir: cli.chdir,
        insecure: cli.insecure,
    })?;
    if cli.version {
        let v = env!("CARGO_PKG_VERSION");
        ctx.out.line(format!("lode {v}"));
        return Ok(json!({ "version": v }));
    }
    match cli.command {
        None => Err(CliError::usage("no command given; see `lode --help`")),
        Some(Command::Login(a)) => cmd::login::login(&ctx, a).await,
        Some(Command::Logout) => cmd::login::logout(&ctx).await,
        Some(Command::Whoami) => cmd::login::whoami(&ctx).await,
        Some(Command::Checkout(a)) => cmd::checkout::checkout(&ctx, a).await,
        Some(Command::View(c)) => cmd::view::view(&ctx, c).await,
        Some(Command::Status(a)) => cmd::status::status(&ctx, a).await,
        Some(Command::Add(a)) => cmd::files::add(&ctx, a).await,
        Some(Command::Rm(a)) => cmd::files::rm(&ctx, a).await,
        Some(Command::Mv(a)) => cmd::files::mv(&ctx, a).await,
        Some(Command::Cp(a)) => cmd::files::cp(&ctx, a).await,
        Some(Command::Revert(a)) => cmd::revert::revert(&ctx, a).await,
        Some(Command::Diff(a)) => cmd::diff::diff(&ctx, a).await,
        Some(Command::Clean(a)) => cmd::revert::clean(&ctx, a).await,
        Some(Command::Commit(a)) => cmd::commit::commit(&ctx, a).await,
        Some(Command::Update(a)) => cmd::update::update(&ctx, a).await,
        Some(Command::Resolve(a)) => cmd::update::resolve(&ctx, a).await,
        Some(Command::Lock(a)) => cmd::locks::lock(&ctx, a).await,
        Some(Command::Unlock(a)) => cmd::locks::unlock(&ctx, a).await,
        Some(Command::Locks(a)) => cmd::locks::locks(&ctx, a).await,
        Some(Command::Changelist(a)) => cmd::changelist::changelist(&ctx, a).await,
        Some(Command::Log(a)) => cmd::history::log(&ctx, a).await,
        Some(Command::Cat(a)) => cmd::history::cat(&ctx, a).await,
        Some(Command::Ls(a)) => cmd::history::ls(&ctx, a).await,
        Some(Command::External(args)) => {
            // The child owns stdout; `lode` adds nothing to it and passes its exit code on.
            let code = external::run(&ctx, &args)?;
            std::process::exit(code);
        }
    }
}

fn wants_json() -> bool {
    std::env::args().any(|a| a == "--json")
}

fn finish(json: bool, result: CliResult<Value>) -> ExitCode {
    match result {
        Ok(v) => {
            if json {
                let mut doc = json!({ "ok": true });
                if let (Some(d), Value::Object(m)) = (doc.as_object_mut(), v) {
                    d.extend(m);
                }
                println!("{doc}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                println!("{}", e.to_json());
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::from(e.exit as u8)
        }
    }
}

fn main() -> ExitCode {
    let json = wants_json();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if json && !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                return finish(true, Err(CliError::usage(e.to_string())));
            }
            e.exit();
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return finish(json, Err(e.into())),
    };
    let result = runtime.block_on(run(cli));
    finish(json, result)
}
