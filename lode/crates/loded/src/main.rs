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
            Ok(())
        }
        Command::Serve { config } => {
            let cfg = ServerConfig::load(&config)?;
            // No modules are registered yet (T017); /info reports none.
            let state =
                lode_server::open_state(cfg.clone(), Arc::new(SystemClock), Vec::new()).await?;
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
            tracing::info!("stopped");
            Ok(())
        }
    }
}
