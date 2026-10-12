//! Standalone out-of-process entrypoint for the `api-contracts` `PaymentApi`
//! **provider** gear (the `OoP` bootstrap path shared by Profile 2 and Profile 3).
//!
//! Built only with `--features oop_module` (see `Cargo.toml`). Runs the gear as
//! its own `OoP` unit: connects to flight-control's `DirectoryService` (via
//! `TOOLKIT_DIRECTORY_ENDPOINT`), registers its REST endpoint (from
//! `oop_http.advertise_uri`), authenticates tenant-plane requests locally
//! (embedded authn stack), and serves `/api-contracts/v1/...`. Consumer units
//! (e.g. `api-contracts-consumer`) discover this endpoint via the
//! `DirectoryService` and call the `PaymentApi` contract over REST — the `OoP`
//! gear-to-gear path.

mod registered_gears;

use clap::Parser;
use mimalloc::MiMalloc;
use std::path::PathBuf;
use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// `api-contracts` `OoP` provider gear.
#[derive(Parser)]
#[command(name = "api-contracts-oop")]
#[command(about = "api-contracts - PaymentApi REST provider OoP gear")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Path to configuration file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Log verbosity level (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let opts = OopRunOptions {
        gear_name: "api-contracts".to_owned(),
        config_path: cli.config,
        verbose: cli.verbose,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    };

    run_oop_with_options(opts).await
}
