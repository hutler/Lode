//! `loded`, the Lode server.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
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
    /// Run the server.
    Serve {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
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
        Command::Serve { config } => {
            let cfg = ServerConfig::load(&config)?;
            let modules = lode_server::module::select_enabled(&cfg.modules, available_modules());
            let state =
                lode_server::open_state(cfg.clone(), Arc::new(SystemClock), modules).await?;
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
            let dispatcher = lode_server::events::Dispatcher::spawn(state.clone(), async move {
                let _ = stop_rx.await;
            });
            let router = lode_server::router(state);
            let server = BoundServer::bind(&cfg, router).await?;
            // Tests and wrapper scripts read this line to find the port.
            println!(
                "loded listening on {}://{}",
                server.scheme(),
                server.local_addr()
            );
            tracing::info!(addr = %server.local_addr(), scheme = server.scheme(), "listening");
            server
                .run(lode_server::shutdown_signal())
                .await
                .context("server error")?;
            let _ = stop_tx.send(());
            let _ = dispatcher.await;
            tracing::info!("stopped");
            Ok(())
        }
    }
}

/// Every module compiled into this build. `[modules]` decides which are enabled. The real
/// modules arrive with their tasks (M3, M4, M6).
fn available_modules() -> lode_server::module::Modules {
    Vec::new()
}
