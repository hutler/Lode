//! `lode`, the Lode command line client.

#![forbid(unsafe_code)]

use clap::Parser;

/// Lode version control client.
#[derive(Parser, Debug)]
#[command(name = "lode", version, about)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    Ok(())
}
