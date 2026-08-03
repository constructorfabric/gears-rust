//! OoP binary for the hello gear.
//!
//! Runs the hello gear as an out-of-process service: serves its Axum REST router
//! over HTTP and self-registers (REST endpoint + OpenAPI spec) with the
//! `DirectoryService` so the api-gateway edge can reverse-proxy to it.
//!
//! Configuration is loaded from:
//! 1. `--config` CLI argument, or
//! 2. `MODULE_CONFIG_PATH` environment variable (fallback).
//!
//! The directory endpoint is taken from `TOOLKIT_DIRECTORY_ENDPOINT` (or the
//! `OopRunOptions` default).

mod registered_gears;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;
    use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

    /// OoP hello gear
    #[derive(Parser)]
    #[command(name = "hello-oop")]
    struct Cli {
        /// Path to configuration file
        #[arg(short, long)]
        config: Option<std::path::PathBuf>,

        /// Log verbosity level (-v debug, -vv trace)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
    }

    let cli = Cli::parse();

    let opts = OopRunOptions {
        gear_name: "hello".to_string(),
        verbose: cli.verbose,
        config_path: cli.config,
        ..Default::default()
    };

    run_oop_with_options(opts).await
}
