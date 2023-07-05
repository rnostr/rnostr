//! Rnostr cli
use clap::Parser;
#[macro_use]
extern crate clap;

use rnostr::*;

/// Cli
#[derive(Debug, Parser)]
#[command(name = "rnostr", about = "Rnostr cli.", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Commands
#[derive(Debug, Subcommand)]
enum Commands {
    /// Import data from jsonl file
    #[command(arg_required_else_help = true)]
    Import(ImportOpts),
    /// Export data to jsonl file
    #[command(arg_required_else_help = true)]
    Export(ExportOpts),
    /// Benchmark filter
    #[command(arg_required_else_help = true)]
    Bench(BenchOpts),
    /// Start nostr relay server
    Relay(RelayOpts),
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        Commands::Import(opts) => {
            let total = import_opts(opts)?;
            println!("imported {} events", total);
        }
        Commands::Export(opts) => {
            export_opts(opts)?;
        }
        Commands::Bench(opts) => {
            bench_opts(opts)?;
        }
        Commands::Relay(opts) => {
            relay(&opts.config, opts.watch)?;
        }
    }
    Ok(())
}
