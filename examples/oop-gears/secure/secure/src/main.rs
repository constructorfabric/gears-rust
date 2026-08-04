//! OoP binary for the secure gear.
//!
//! Runs the secure gear as an out-of-process service with **two-plane
//! authentication**: it serves its Axum REST router over HTTP, self-registers
//! with the `DirectoryService`, and links an in-process AuthN stack so the
//! bearer token forwarded by the edge is re-validated inside this pod.
//!
//! Configuration is loaded from:
//! 1. `--config` CLI argument, or
//! 2. `MODULE_CONFIG_PATH` / `TOOLKIT_CONFIG_PATH` environment variable.
//!
//! The tenant-plane authenticator is wired up by *linking* the in-process
//! `authn-resolver` gear into this binary (see `registered_gears`): during
//! `init` that gear registers a `DynBearerAuthenticator` bridge in the
//! `ClientHub`, and the `OoP` runtime picks it up after `start` to install
//! `security_context_middleware` on the gear's routes. No bootstrap wiring is
//! needed here.

mod registered_gears;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

    /// OoP secure gear
    #[derive(Parser)]
    #[command(name = "secure-oop")]
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
        gear_name: "secure".to_string(),
        verbose: cli.verbose,
        config_path: cli.config,
        ..Default::default()
    };

    run_oop_with_options(opts).await
}
