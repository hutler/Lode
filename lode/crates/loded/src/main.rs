//! `loded`, the Lode server.

#![forbid(unsafe_code)]

mod service;
mod setup;
#[cfg(windows)]
mod winsvc;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use lode_meta::Reads;
use lode_server::{BoundServer, ServerConfig, SystemClock};

/// Lode version control server.
#[derive(Parser, Debug)]
#[command(name = "loded", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a data directory: database, object store and a default loded.toml.
    Init {
        /// Directory to create or fill.
        data_dir: PathBuf,
    },
    /// Offline recovery: revoke the `admin` user's tokens and print a new one.
    ResetAdmin {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
    },
    /// Check depots for missing, damaged or inconsistent data. Exits with 1 on problems.
    Fsck {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Also decode and re-hash every stored object.
        #[arg(long)]
        full: bool,
        /// Depots to check; all when none are given.
        depots: Vec<String>,
    },
    /// Copy the data directory to an empty directory while the server keeps running.
    Backup {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Destination directory; must be missing or empty.
        dest: PathBuf,
    },
    /// Move file content to the S3 bucket named in [storage], then switch loded.toml to it.
    /// Safe to repeat; each run copies only what the bucket lacks.
    MigrateStorage {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Where to move the content.
        #[arg(long, value_enum)]
        to: StorageTarget,
        /// Copy but do not switch loded.toml (a first pass while the server runs).
        #[arg(long)]
        copy_only: bool,
    },
    /// Run the server.
    Serve {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Run under the Windows service manager (set by `loded service install`).
        #[arg(long, hide = true)]
        service: bool,
    },
    /// First run: ask a few questions, write a checked loded.toml, initialize the data
    /// directory and set the admin password. Every answer can be given as a flag.
    Setup(setup::SetupArgs),
    /// Register loded as a system service (Windows service, or systemd on Linux).
    #[command(subcommand)]
    Service(service::ServiceCommand),
}

/// A storage `loded migrate-storage` moves to.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum StorageTarget {
    /// The S3 bucket in [storage].
    S3,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Command::Serve {
        config,
        service: true,
    } = &cli.command
    {
        #[cfg(windows)]
        return winsvc::run(config.clone());
        #[cfg(not(windows))]
        {
            let _ = config;
            anyhow::bail!("--service is for Windows; on Linux, systemd runs `loded serve`");
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LODED_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init { data_dir } => {
            let report = lode_server::init(&data_dir).await?;
            println!(
                "Initialized Lode data directory {}",
                report.data_dir.display()
            );
            match report.config_written {
                Some(path) => println!("Wrote default configuration {}", path.display()),
                None => println!("Kept the existing loded.toml"),
            }
            println!();
            println!("Admin token (shown only now; store it safely):");
            println!("{}", report.admin_token);
            Ok(())
        }
        Command::ResetAdmin { config } => {
            let cfg = ServerConfig::load(&config)?;
            let token = lode_server::reset_admin(&cfg).await?;
            println!("Revoked every token of the admin user.");
            println!("New admin token (shown only now; store it safely):");
            println!("{token}");
            Ok(())
        }
        Command::Fsck {
            config,
            full,
            depots,
        } => {
            let cfg = ServerConfig::load(&config)?;
            let state = lode_server::open_state(cfg, Arc::new(SystemClock), Vec::new()).await?;
            let all = state.meta().read(|tx| tx.depots()).await?;
            let mut bad = false;
            for d in &all {
                if !depots.is_empty() && !depots.contains(&d.name) {
                    continue;
                }
                let report = lode_server::maintenance::fsck(
                    &state,
                    d.id,
                    lode_server::maintenance::FsckOptions { full },
                )
                .await
                .map_err(|e| anyhow::anyhow!("fsck of {} failed: {}", d.name, e.body().message))?;
                println!(
                    "{}: {} objects checked, {} problem(s)",
                    d.name,
                    report.objects_checked,
                    report.problems.len()
                );
                for p in &report.problems {
                    println!("  {p}");
                }
                bad |= !report.problems.is_empty();
            }
            if let Some(missing) = depots.iter().find(|n| !all.iter().any(|d| &d.name == *n)) {
                anyhow::bail!("no depot `{missing}`");
            }
            if bad {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Backup { config, dest } => {
            let cfg = ServerConfig::load(&config)?;
            let state = lode_server::open_state(cfg, Arc::new(SystemClock), Vec::new()).await?;
            let report = lode_server::maintenance::backup(&state, &dest).await?;
            if report.content_in_bucket {
                println!("Backed up the database to {}", dest.display());
                println!(
                    "File content stays in the S3 bucket: protect it with the bucket's \
                     versioning or replication."
                );
            } else {
                println!(
                    "Backed up the database and {} objects ({} bytes) to {}",
                    report.objects,
                    report.bytes,
                    dest.display()
                );
            }
            Ok(())
        }
        Command::MigrateStorage {
            config,
            to: StorageTarget::S3,
            copy_only,
        } => {
            let report = lode_server::maintenance::migrate_to_s3(
                &config,
                lode_server::maintenance::MigrateOptions { copy_only },
            )
            .await?;
            println!(
                "{} depot(s): copied {} objects ({} bytes); {} were already in the bucket",
                report.depots, report.copied, report.bytes, report.already_there
            );
            match report.switched {
                Some(path) => {
                    println!(
                        "Switched {} to S3 storage ([storage] kind = \"s3\").",
                        path.display()
                    );
                    println!(
                        "Start the server. The local copies under depots/ are no longer used; \
                         remove them once the server works from the bucket."
                    );
                }
                None => println!(
                    "loded.toml is unchanged. Stop the server and run this again without \
                     --copy-only to copy the rest and switch."
                ),
            }
            Ok(())
        }
        Command::Serve { config, .. } => {
            let cfg = ServerConfig::load(&config)?;
            serve(cfg, lode_server::shutdown_signal()).await
        }
        Command::Setup(args) => setup::run(args).await,
        Command::Service(cmd) => service::run(cmd),
    }
}

/// Runs the server until `shutdown` completes: modules' workers, the event dispatcher and
/// scheduled backups start and stop with it.
pub(crate) async fn serve(
    cfg: ServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let modules = lode_server::module::select_enabled(&cfg.modules, available_modules());
    let state = lode_server::open_state(cfg.clone(), Arc::new(SystemClock), modules).await?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let dispatcher = lode_server::events::Dispatcher::spawn(state.clone(), async move {
        let _ = stop_rx.await;
    });
    let (stop_workers, stop) = lode_server::module::Stop::new();
    let mut workers = lode_server::module::spawn_workers(&state, &stop);
    if let Some(schedule) = cfg.backup.schedule() {
        tracing::info!(
            dir = %schedule.dir.display(),
            every_seconds = schedule.every.as_secs(),
            keep = schedule.keep,
            "scheduled backups on"
        );
        workers.push(tokio::spawn(lode_server::maintenance::run_backups(
            state.clone(),
            schedule,
            stop.clone(),
        )));
    }
    let router = lode_server::router(state);
    let server = BoundServer::bind(&cfg, router).await?;
    // Tests and wrapper scripts read this line to find the port.
    println!(
        "loded listening on {}://{}",
        server.scheme(),
        server.local_addr()
    );
    tracing::info!(addr = %server.local_addr(), scheme = server.scheme(), "listening");
    server.run(shutdown).await.context("server error")?;
    let _ = stop_tx.send(());
    let _ = stop_workers.send(true);
    let _ = dispatcher.await;
    for w in workers {
        let _ = w.await;
    }
    tracing::info!("stopped");
    Ok(())
}

/// Every module compiled into this build. `[modules]` decides which are enabled. Keep in step
/// with `lode_testkit::available_modules`.
fn available_modules() -> lode_server::module::Modules {
    vec![
        lode_mod_locks::Locks::module(),
        lode_mod_shelves::Shelves::module(),
        lode_mod_mcp::Mcp::module(),
        lode_mod_webhooks::Webhooks::module(),
        lode_mod_checks::Checks::module(),
    ]
}
