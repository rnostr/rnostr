//! Rnostr cli
use clap::Parser;
#[macro_use]
extern crate clap;

use rnostr::*;
use std::io::IsTerminal;
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    prelude::*,
};

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
    /// Delete data by filter
    Delete(DeleteOpts),
}

fn logging_filter(value: &str) -> Targets {
    let filter = if value.trim().is_empty() {
        Targets::new()
    } else {
        value.parse::<Targets>().unwrap_or_else(|error| {
            eprintln!("Ignoring RUST_LOG={value:?}: {error}; using info");
            Targets::new()
        })
    };
    let default = filter.default_level().unwrap_or(LevelFilter::INFO);
    filter.with_default(default)
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    tracing_subscriber::registry()
        .with(logging_filter(
            &std::env::var("RUST_LOG").unwrap_or_default(),
        ))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(
                    std::io::stderr().is_terminal()
                        && std::env::var_os("NO_COLOR").unwrap_or_default().is_empty(),
                ),
        )
        .try_init()
        .map_err(anyhow::Error::msg)?;
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
        Commands::Delete(opts) => {
            let count = delete(&opts.path, &opts.filter, opts.dry_run)?;
            if opts.dry_run {
                println!("Would delete {} events", count);
            } else {
                println!("Deleted {} events", count);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    #[test]
    fn logging_filter_preserves_baseline_and_explicit_levels() {
        for (value, relay_debug, other_info, other_debug) in [
            ("", false, true, false),
            (" ", false, true, false),
            ("rnostr=debug", true, true, false),
            ("debug", true, true, true),
            ("off", false, false, false),
            ("off,rnostr=debug", true, false, false),
            ("rnostr=invalid-level", false, true, false),
            ("off,rnostr=invalid-level", false, true, false),
        ] {
            let filter = logging_filter(value);
            assert_eq!(
                filter.would_enable("rnostr::relay", &Level::DEBUG),
                relay_debug,
                "{value}"
            );
            assert_eq!(
                filter.would_enable("actix_web", &Level::INFO),
                other_info,
                "{value}"
            );
            assert_eq!(
                filter.would_enable("actix_web", &Level::DEBUG),
                other_debug,
                "{value}"
            );
        }
    }
}
