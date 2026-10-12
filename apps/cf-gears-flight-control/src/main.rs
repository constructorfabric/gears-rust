//! # CF/Gears Flight Control
//!
//! The minimal platform control-plane deployment unit for a *distributed*
//! (out-of-process) CF/Gears deployment: it links the directory + transport +
//! edge gears plus edge JWT validation (authn-resolver) and runs them under the
//! `ToolKit` `HostRuntime`, serving the `DirectoryService` that out-of-process
//! gears register with. The `AuthZ` plane and application gears run as their own
//! processes/pods (Profile 2/3) and discover flight-control at runtime.
//!
//! See this crate's `README.md` for the full composition, deployment profiles,
//! plugin presets, and usage; and `docs/arch/toolkit-oop/` (DESIGN § Flight
//! Control Composition, ADR-0001) for the authoritative architecture.

mod registered_gears;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use std::path::PathBuf;
use toolkit::bootstrap::{AppConfig, list_gear_names, run_server};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// CF/Gears Flight Control command-line interface (see the crate-level docs).
#[derive(Parser)]
#[command(name = "flight-control")]
#[command(about = "CF/Gears Flight Control - minimal platform control-plane deployment unit")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Print effective configuration (YAML) and exit
    #[arg(long)]
    print_config: bool,

    /// List all configured gear names and exit
    #[arg(long)]
    list_gears: bool,

    /// Log verbosity level (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the host: initialize all linked gears, serve the directory + edge,
    /// and block until shutdown.
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut config = AppConfig::load_or_default(cli.config.as_ref())?;
    config.apply_cli_overrides(cli.verbose);

    if cli.print_config {
        println!("Effective configuration:\n{}", config.to_yaml()?);
        return Ok(());
    }

    if cli.list_gears {
        let gears = list_gear_names(&config);
        println!("Configured gears ({}):", gears.len());
        for gear in gears {
            println!("  - {gear}");
        }
        return Ok(());
    }

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run_server(config).await,
    }
}
