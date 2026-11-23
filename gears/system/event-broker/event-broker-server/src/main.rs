//! Event Broker executable with process-owned cluster coordination.

#[cfg_attr(
    test,
    expect(
        dead_code,
        unused_imports,
        reason = "the cluster_memory integration target consumes the private backend test hooks"
    )
)]
mod cluster_memory;
/// Offline validation and runtime client wiring for the Event Broker process.
mod composition;
#[cfg(feature = "e2e-flight-control")]
mod flight_control_auth;
mod registered_gears;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use toolkit::bootstrap::{OopRunOptions, oop::oop_publication_config, run_oop_with_composition};

#[derive(Parser)]
#[command(name = "cf-gears-event-broker-server", about = "Event Broker", version)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Log verbosity level (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Authenticate tenant requests with the local test flight-control process.
    #[cfg(feature = "e2e-flight-control")]
    #[arg(long)]
    e2e_flight_control: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the server
    Run,
    /// Validate the effective broker configuration offline
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut options = OopRunOptions {
        gear_name: "event-broker".to_owned(),
        verbose: cli.verbose,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..OopRunOptions::default()
    };
    if let Some(config) = cli.config {
        options.config_path = Some(config);
    }
    if matches!(cli.command, Some(Commands::Check)) {
        composition::check(&oop_publication_config(&options)?)?;
        return Ok(());
    }
    #[cfg(feature = "e2e-flight-control")]
    if cli.e2e_flight_control {
        return Ok(Box::pin(run_oop_with_composition(options, composition::compose_e2e)).await?);
    }
    Ok(Box::pin(run_oop_with_composition(
        options,
        composition::compose_production,
    ))
    .await?)
}
