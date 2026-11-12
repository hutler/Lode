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
mod materialize;
mod output;

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
