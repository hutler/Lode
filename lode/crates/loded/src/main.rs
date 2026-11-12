//! `loded`, the Lode server.

#![forbid(unsafe_code)]

use clap::Parser;

/// Lode version control server.
#[derive(Parser, Debug)]
#[command(name = "loded", version, about)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    Ok(())
}
