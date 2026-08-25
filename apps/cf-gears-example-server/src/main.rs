mod registered_gears;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use serde_json::Value;
use toolkit::bootstrap::{
    AppConfig, dump_effective_gears_config_json, dump_effective_gears_config_yaml, list_gear_names,
    run_migrate, run_server,
};

use std::{collections::hash_map::Entry, path::PathBuf};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// `CF/Gears` Server - modular platform for AI services
#[derive(Parser)]
#[command(name = "cf-gears-example-server")]
#[command(about = "CF/Gears Server - modular platform for XaaS services")]
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

    /// List all configured gear names and exit
    #[arg(long)]
    list_gears: bool,

    /// Dump effective per-gear configuration (YAML) and exit
    #[arg(long)]
    dump_gears_config_yaml: bool,

    /// Dump effective per-gear configuration (JSON) and exit
    #[arg(long)]
    dump_gears_config_json: bool,

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

fn apply_port_override(config: &mut AppConfig, port: u16) -> Result<()> {
    let gear = match config.gears.entry("api-gateway".to_owned()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => entry.insert(serde_json::json!({ "config": {} })),
    };
    let gear_obj = gear
        .as_object_mut()
        .context("api-gateway config must be a map")?;
    let config_value = gear_obj
        .entry("config".to_owned())
        .or_insert_with(|| serde_json::json!({}));
    let config_obj = config_value
        .as_object_mut()
        .context("api-gateway.config must be a map")?;
    let host = config_obj
        .get("bind_addr")
        .and_then(Value::as_str)
        .and_then(|bind_addr| bind_addr.rsplit_once(':').map(|(host, _)| host.to_owned()))
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    if host.is_empty() {
        bail!("api-gateway.config.bind_addr has an empty host");
    }
    config_obj.insert(
        "bind_addr".to_owned(),
        Value::String(format!("{host}:{port}")),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // The rustls crypto provider is installed inside
    // `toolkit::bootstrap::init_procedure` (called from `run_server` /
    // `run_migrate` below) — no explicit setup needed here.

    let cli = Cli::parse();

    // Layered config:
    // 1) defaults -> 2) YAML (if provided) -> 3) env (APP__*) -> 4) CLI overrides
    // Also normalizes + creates server.home_dir.
    let mut config = AppConfig::load_or_default(cli.config.as_ref())?;
    config.apply_cli_overrides(cli.verbose);
    if let Some(port) = cli.port {
        apply_port_override(&mut config, port)?;
    }

    // Print config and exit if requested
    if cli.print_config {
        println!("Effective configuration:\n{}", config.to_yaml()?);
        return Ok(());
    }

    // List all configured gears and exit if requested
    if cli.list_gears {
        let gears = list_gear_names(&config);
        println!("Configured gears ({}):", gears.len());
        for gear in gears {
            println!("  - {gear}");
        }
        return Ok(());
    }

    // Dump gears config in YAML format and exit if requested
    if cli.dump_gears_config_yaml {
        let yaml = dump_effective_gears_config_yaml(&config)?;
        println!("{yaml}");
        return Ok(());
    }

    // Dump gears config in JSON format and exit if requested
    if cli.dump_gears_config_json {
        let json = dump_effective_gears_config_json(&config)?;
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
