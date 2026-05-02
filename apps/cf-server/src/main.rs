mod registered_modules;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use modkit::bootstrap::{
    AppConfig, dump_effective_modules_config_json, dump_effective_modules_config_yaml,
    list_module_names, run_migrate, run_server,
};

use std::path::PathBuf;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// `CyberFabric` Server - modular platform for AI services
#[derive(Parser)]
#[command(name = "cf-server")]
#[command(about = "CyberFabric Server - modular platform for XaaS services")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Port override for HTTP server (overrides config)
    #[arg(short, long)]
    port: Option<u16>,

    /// Print effective configuration (YAML) and exit
    #[arg(long)]
    print_config: bool,

    /// List all configured module names and exit
    #[arg(long)]
    list_modules: bool,

    /// Dump effective per-module configuration (YAML) and exit
    #[arg(long)]
    dump_modules_config_yaml: bool,

    /// Dump effective per-module configuration (JSON) and exit
    #[arg(long)]
    dump_modules_config_json: bool,

    /// Log verbosity level (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the server
    Run,
    /// Do nothing
    Check,
    /// Run database migrations and exit (for cloud deployments)
    Migrate,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install the crypto provider before anything else.
    // Must run before any TLS config, HTTP client, DB connection, or JWT operation.
    if let Err(e) = modkit::bootstrap::init_crypto_provider() {
        eprintln!("fatal: failed to install TLS crypto provider: {e}");
        return Err(e.into());
    }

    let cli = Cli::parse();

    // Layered config:
    // 1) defaults -> 2) YAML (if provided) -> 3) env (APP__*) -> 4) CLI overrides
    // Also normalizes + creates server.home_dir.
    let mut config = AppConfig::load_or_default(cli.config.as_ref())?;
    config.apply_cli_overrides(cli.verbose);

    // Print config and exit if requested
    if cli.print_config {
        println!("Effective configuration:\n{}", config.to_yaml()?);
        return Ok(());
    }

    // List all configured modules and exit if requested
    if cli.list_modules {
        let modules = list_module_names(&config);
        println!("Configured modules ({}):", modules.len());
        for module in modules {
            println!("  - {module}");
        }
        return Ok(());
    }

    // Dump modules config in YAML format and exit if requested
    if cli.dump_modules_config_yaml {
        let yaml = dump_effective_modules_config_yaml(&config)?;
        println!("{yaml}");
        return Ok(());
    }

    // Dump modules config in JSON format and exit if requested
    if cli.dump_modules_config_json {
        let json = dump_effective_modules_config_json(&config)?;
        println!("{json}");
        return Ok(());
    }

    // Dispatch subcommands (default: run)
    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run_server(config).await,
        Commands::Migrate => run_migrate(config).await,
        Commands::Check => Ok(()),
    }
}
