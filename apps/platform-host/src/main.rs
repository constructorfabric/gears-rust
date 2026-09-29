//! # CF/Gears Platform Host
//!
//! The orchestrating process for a *distributed* (out-of-process) CF/Gears
//! deployment: it links a small co-located core plus the shared system gears and
//! runs them under the `ToolKit` `HostRuntime`, serving the `DirectoryService`
//! that out-of-process gears register with. Application gears run as their own
//! processes/pods (Profile 2/3) and discover this host at runtime.
//!
//! See this crate's `README.md` for the full composition, deployment profiles,
//! plugin presets, and usage; and `docs/arch/toolkit-oop/` (DESIGN § Platform
//! Host Composition, ADR-0001) for the authoritative architecture.

mod registered_gears;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use std::path::PathBuf;
use toolkit::bootstrap::{AppConfig, list_gear_names, run_migrate, run_server};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// CF/Gears Platform Host — the orchestrating process for a distributed
/// deployment: runs the co-located core + system gears and serves the
/// `DirectoryService` that out-of-process gears register with. See the crate
/// docs for the full composition and deployment-profile model.
#[derive(Parser)]
#[command(name = "platform-host")]
#[command(about = "CF/Gears Platform Host - co-located core + system gears")]
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
    /// and block until shutdown. This is the default when no subcommand is given.
    Run,
    /// Run database migrations for all linked gears and exit, without starting
    /// the server. Intended as an init step for cloud/K8s deployments.
    Migrate,
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
        Commands::Migrate => run_migrate(config).await,
    }
}
