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
#[command(name = "loded", version = lode_build::VERSION, about)]
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
    /// Send the newest scheduled backup to the bucket in [backup.copy_to] now, instead of
    /// waiting for the server to do it after the next backup.
    BackupCopy {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
    },
    /// Download a copy of a backup from the bucket in [backup.copy_to] into an empty
    /// directory, ready to be the data directory. On a new machine, write a loded.toml with
    /// only that table (and its secret file) to point at the bucket.
    Restore {
        /// Path to a loded.toml with [backup.copy_to].
        #[arg(long)]
        config: PathBuf,
        /// List the copies in the bucket instead.
        #[arg(long, conflicts_with_all = ["name", "dest"])]
        list: bool,
        /// The copy to restore, such as lode-backup-20261120T030000Z; the newest when left
        /// out.
        #[arg(long)]
        name: Option<String>,
        /// Destination directory; must be missing or empty.
        #[arg(required_unless_present = "list")]
        dest: Option<PathBuf>,
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
    /// Copy the metadata from meta.db into an empty Postgres database, check it, then switch
    /// loded.toml to it (the old file is kept as loded.toml.before-postgres). Stop the server
    /// first.
    MigrateDb {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Where to move the metadata.
        #[arg(long, value_enum)]
        to: DatabaseTarget,
        /// A file whose first line is the postgres:// URL.
        #[arg(long)]
        url_file: PathBuf,
    },
    /// Run the server.
    Serve {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// With the metadata in Postgres: if another server is active on the database, wait
        /// and take over when it stops, instead of refusing to start.
        #[arg(long)]
        wait_for_lock: bool,
        /// Run under the Windows service manager (set by `loded service install`).
        #[arg(long, hide = true)]
        service: bool,
    },
    /// Run a caching proxy for a remote office: file content is fetched from the main server
    /// once and served from a local cache (see proxy.toml in the operator guide).
    Proxy {
        /// Path to proxy.toml.
        #[arg(long, required_unless_present = "example")]
        config: Option<PathBuf>,
        /// Print an example proxy.toml and exit.
        #[arg(long, conflicts_with = "config")]
        example: bool,
    },
    /// First run: ask a few questions, write a checked loded.toml, initialize the data
    /// directory and set the admin password. Every answer can be given as a flag.
    Setup(setup::SetupArgs),
    /// Register loded as a system service (Windows service, or systemd on Linux).
    #[command(subcommand)]
    Service(service::ServiceCommand),
}

/// A database `loded migrate-db` moves to.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum DatabaseTarget {
    /// A Postgres database.
    Postgres,
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
        ..
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
        Command::BackupCopy { config } => {
            let cfg = ServerConfig::load(&config)?;
            let Some(schedule) = cfg.backup.schedule() else {
                anyhow::bail!("scheduled backups are off: set `every` under [backup]");
            };
            let Some(copy) = cfg.backup.copy_to.clone() else {
                anyhow::bail!("loded.toml has no [backup.copy_to]");
            };
            let Some((_, newest)) = lode_server::maintenance::newest_backup(&schedule.dir) else {
                anyhow::bail!(
                    "there is no scheduled backup in {} yet",
                    schedule.dir.display()
                );
            };
            let bucket = lode_server::maintenance::copy::open_bucket(&copy, &cfg.data_dir).await?;
            let tmp = cfg.data_dir.join("backup-copy-tmp");
            let r =
                lode_server::maintenance::copy::copy_backup(&copy, &bucket, &newest, &tmp).await?;
            println!(
                "Copied {} to {}: {} objects, {} uploaded now, {} bytes sent",
                r.name, copy.s3.bucket, r.objects, r.uploaded, r.bytes
            );
            if !r.removed.is_empty() {
                println!("Removed old copies: {}", r.removed.join(", "));
            }
            Ok(())
        }
        Command::Restore {
            config,
            list,
            name,
            dest,
        } => {
            let cfg = ServerConfig::load(&config)?;
            let Some(copy) = cfg.backup.copy_to.clone() else {
                anyhow::bail!("{} has no [backup.copy_to]", config.display());
            };
            let tmp = std::env::temp_dir().join("loded-restore");
            let bucket = lode_server::maintenance::copy::open_bucket(&copy, &tmp).await?;
            if list {
                let copies = lode_server::maintenance::copy::list_remote(&bucket).await?;
                if copies.is_empty() {
                    println!("{} holds no complete copy", copy.s3.bucket);
                }
                for c in copies {
                    println!("{}", c.name);
                }
                return Ok(());
            }
            let Some(dest) = dest else {
                anyhow::bail!("name the destination directory");
            };
            let r =
                lode_server::maintenance::copy::restore(&bucket, name.as_deref(), &dest).await?;
            println!(
                "Restored {} into {}: {} objects, {} bytes",
                r.name,
                dest.display(),
                r.objects,
                r.bytes
            );
            println!(
                "Next: check it with `loded fsck --config <loded.toml with data_dir = {}>`, \
                 then start the server on it.",
                dest.display()
            );
            Ok(())
        }
        Command::Backup { config, dest } => {
            let cfg = ServerConfig::load(&config)?;
            let state = lode_server::open_state(cfg, Arc::new(SystemClock), Vec::new()).await?;
            let report = lode_server::maintenance::backup(&state, &dest).await?;
            if report.database_external {
                println!(
                    "The metadata is in Postgres and is not in this backup: back it up with \
                     pg_dump (see {} in the backup).",
                    lode_server::maintenance::DATABASE_NOTE_FILE
                );
            }
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
        Command::MigrateDb {
            config,
            to: DatabaseTarget::Postgres,
            url_file,
        } => {
            let report = lode_server::maintenance::migrate_db_to_postgres(
                &config,
                &url_file,
                available_modules(),
            )
            .await?;
            for (table, rows) in &report.copy.tables {
                println!("{table:24} {rows:>10} rows");
            }
            println!(
                "Copied {} rows into Postgres and switched {} to it (the old file is kept as \
                 {}.before-postgres). meta.db is left in place; remove it once the server runs \
                 well on Postgres.",
                report.copy.rows(),
                report.switched.display(),
                report
                    .switched
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("loded.toml"),
            );
            Ok(())
        }
        Command::Serve {
            config,
            wait_for_lock,
            ..
        } => {
            let cfg = ServerConfig::load(&config)?;
            serve(cfg, wait_for_lock, lode_server::shutdown_signal()).await
        }
        Command::Proxy { config, example } => {
            if example {
                print!("{}", lode_proxy::EXAMPLE_CONFIG);
                return Ok(());
            }
            let config = config.context("--config is required")?;
            let cfg = lode_proxy::ProxyConfig::load(&config)?;
            let proxy = lode_proxy::BoundProxy::bind_default(&cfg).await?;
            tracing::info!(
                address = %proxy.local_addr(),
                scheme = proxy.scheme(),
                upstream = %cfg.upstream,
                "caching proxy listening"
            );
            proxy.run(lode_server::shutdown_signal()).await?;
            Ok(())
        }
        Command::Setup(args) => setup::run(args).await,
        Command::Service(cmd) => service::run(cmd),
    }
}

/// Runs the server until `shutdown` completes: modules' workers, the event dispatcher, the
/// trash purge and scheduled backups start and stop with it.
pub(crate) async fn serve(
    cfg: ServerConfig,
    wait_for_lock: bool,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let modules = lode_server::module::select_enabled(&cfg.modules, available_modules());
    let state = lode_server::open_state(cfg.clone(), Arc::new(SystemClock), modules).await?;
    let lock = Arc::new(active_server_lock(&state, wait_for_lock).await?);
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown = {
        let lock = lock.clone();
        let lost = lost.clone();
        async move {
            tokio::select! {
                () = shutdown => {}
                () = lock.lost() => {
                    lost.store(true, std::sync::atomic::Ordering::SeqCst);
                    tracing::error!(
                        "lost the connection that marks this server as the active one; \
                         stopping, because a standby may take over"
                    );
                }
            }
        }
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let dispatcher = lode_server::events::Dispatcher::spawn(state.clone(), async move {
        let _ = stop_rx.await;
    });
    let (stop_workers, stop) = lode_server::module::Stop::new();
    let mut workers = lode_server::module::spawn_workers(&state, &stop);
    workers.push(tokio::spawn(lode_server::trash::run_purge(
        state.clone(),
        stop.clone(),
    )));
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
    let router = lode_server::router(state.clone());
    let server = BoundServer::bind(&cfg, router).await?;
    state.set_listening(server.local_addr());
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
    drop(lock);
    tracing::info!("stopped");
    if lost.load(std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("stopped: lost the database lock");
    }
    Ok(())
}

/// With Postgres, makes this process the one active server on the database: refuses to
/// start while another holds it, or with `wait` waits until it stops.
async fn active_server_lock(
    state: &lode_server::AppState,
    wait: bool,
) -> anyhow::Result<lode_meta::ServerLock> {
    let holder = format!("loded {} pid {}", host_name(), std::process::id());
    let mut announced = false;
    loop {
        match state.meta().try_server_lock(&holder).await? {
            lode_meta::LockAttempt::Held(lock) => {
                if announced {
                    tracing::info!("the other server stopped; this one is now active");
                }
                return Ok(lock);
            }
            lode_meta::LockAttempt::InUse(other) if !wait => anyhow::bail!(
                "the database is in use by another server ({other}). Stop it first, or start \
                 this one with --wait-for-lock to take over when it stops."
            ),
            lode_meta::LockAttempt::InUse(other) => {
                if !announced {
                    tracing::info!(active = %other, "another server is active; waiting to take over");
                    announced = true;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

/// This machine's name, for the database lock's holder text.
fn host_name() -> String {
    let from_env = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();
    from_env
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_owned())
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
        lode_mod_feed::Feed::module(),
        lode_mod_validate::Validate::module(),
    ]
}
