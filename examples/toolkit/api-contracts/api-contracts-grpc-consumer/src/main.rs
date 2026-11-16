//! Standalone out-of-process entrypoint for the `api-contracts-grpc-consumer`
//! gear (the `OoP` bootstrap path shared by Profile 2 and Profile 3).
//!
//! Built only with `--features oop_module`. Connects to flight-control's
//! `DirectoryService` (`TOOLKIT_DIRECTORY_ENDPOINT`), registers its REST
//! endpoint, authenticates tenant-plane requests locally (embedded authn
//! stack), and serves `/api-contracts-grpc-consumer/v1/...`. Every handler
//! calls the `PaymentApi` contract of the `api-contracts` provider unit over
//! gRPC, with the service found through the directory.

mod registered_gears;

use clap::Parser;
use mimalloc::MiMalloc;
use std::path::PathBuf;
use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// `api-contracts-grpc-consumer` `OoP` gear.
#[derive(Parser)]
#[command(name = "api-contracts-grpc-consumer-oop")]
#[command(about = "api-contracts-grpc-consumer - calls PaymentApi of another OoP unit over gRPC")]
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
        gear_name: "api-contracts-grpc-consumer".to_owned(),
        config_path: cli.config,
        verbose: cli.verbose,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    };

    run_oop_with_options(opts).await
}
