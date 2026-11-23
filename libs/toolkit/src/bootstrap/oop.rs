//! Out-of-process gear bootstrap library
//!
//! This gear provides reusable functionality for bootstrapping `OoP` (out-of-process)
//! `ToolKit` gears in local (non-k8s) environments.
//!
//! ## Features
//!
//! - Configuration loading using `toolkit-bootstrap`
//! - Logging initialization with tracing
//! - Lazy gRPC client to the `DirectoryService` (connects on first use, so a
//!   cold `DirectoryService` never blocks or crashes bootstrap —
//!   `cpt-cf-adr-eventual-readiness`)
//! - Gear instance registration
//! - Heartbeat management
//! - Gear lifecycle execution
//!
//! ## Shutdown Model
//!
//! Shutdown is driven by a single root `CancellationToken` per process:
//! - OS signals (SIGTERM, SIGINT, Ctrl+C) are hooked at bootstrap level
//! - The root token is passed to `RunOptions::Token` for gear runtime shutdown
//! - Background tasks (like heartbeat) use child tokens derived from the root
//!
//! On shutdown, the gear deregisters itself from the `DirectoryService` before exiting.
//!
//! ## Example
//!
//! ```rust,no_run
//! use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let opts = OopRunOptions {
//!         gear_name: "my_gear".to_string(),
//!         config_gear_name: None,
//!         instance_id: None,
//!         directory_endpoint: "http://127.0.0.1:50051".to_string(),
//!         config_path: None,
//!         verbose: 0,
//!         print_config: false,
//!         heartbeat_interval_secs: 5,
//!         version: None,
//!     };
//!
//!     run_oop_with_options(opts).await
//! }
//! ```

use anyhow::{Context, Result};
use figment::{Figment, providers::Serialized};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::config::{
    AppConfig, LoggingConfig, RenderedDbConfig, RenderedGearConfig, TOOLKIT_MODULE_CONFIG_ENV,
};
use crate::bootstrap::host::{init_logging_unified, init_panic_tracing};
use crate::runtime::{
    ClientCleanup, ClientCleanupError, ClientRegistration, DbOptions, OopServeOptions, RunOptions,
    ShutdownOptions, TOOLKIT_DIRECTORY_ENDPOINT_ENV, run, run_oop_serving, shutdown,
};
use cf_system_sdks::directory::{DirectoryClient, DirectoryGrpcClient};
use toolkit_contract::runtime::config::InternalTokenProvider;

/// Supplies process lifetime inputs to a composition factory.
#[derive(Debug)]
pub struct OopCompositionContext {
    /// Cancels process work when shutdown starts.
    pub cancel: CancellationToken,
    /// The outbound credential shared with the directory client and gear contexts.
    pub internal_token_provider: Option<InternalTokenProvider>,
}

/// Transfers the factory's clients and their cleanup operation to bootstrap.
pub struct OopComposition {
    /// The directory registration name for this serving role.
    pub registration_name: String,
    /// Registrations installed before gear initialization.
    pub clients: Vec<ClientRegistration>,
    /// Completes client shutdown after gears stop. Use a separate abort token so
    /// process cancellation leaves client workers available during gear shutdown.
    pub cleanup: ClientCleanup,
}

impl std::fmt::Debug for OopComposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OopComposition")
            .field("registration_name", &self.registration_name)
            .field("client_count", &self.clients.len())
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

/// Constructs clients from the effective configuration. Finish all fallible work
/// before starting a factory-owned worker. Once a worker starts, transfer its
/// cleanup owner through the successful return.
pub type OopCompositionFactory =
    fn(&AppConfig, &OopCompositionContext) -> Result<OopComposition, OopCompositionError>;

/// A static failure category safe to report without exposing configuration values.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OopCompositionError {
    /// The named field has an invalid value.
    #[error("invalid composition configuration field: {field}")]
    InvalidConfig { field: &'static str },
    /// Client construction failed in the named component.
    #[error("client construction failed: {component}")]
    ClientConstruction { component: &'static str },
}

/// The outcome of composition, gear execution, or client shutdown.
#[derive(Debug, thiserror::Error)]
pub enum OopCompositionRunError {
    /// Composition failed before client ownership transferred to bootstrap.
    #[error(transparent)]
    Composition(#[from] OopCompositionError),
    /// Bootstrap or the gear runtime failed.
    #[error("OoP bootstrap failed: {source}")]
    Bootstrap {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Client cleanup failed after successful gear execution.
    #[error(transparent)]
    Cleanup(#[from] ClientCleanupError),
    /// Both bootstrap and subsequent client cleanup failed.
    #[error("OoP bootstrap failed: {source}. Client cleanup failed: {cleanup}")]
    BootstrapAndCleanup {
        source: Box<dyn std::error::Error + Send + Sync>,
        cleanup: ClientCleanupError,
    },
}

impl OopCompositionRunError {
    fn bootstrap(error: anyhow::Error) -> Self {
        Self::Bootstrap {
            source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
        }
    }
}

/// Configuration options for `OoP` gear bootstrap
#[derive(Debug, Clone)]
pub struct OopRunOptions {
    /// Directory registration name used by [`run_oop_with_options`].
    ///
    /// [`run_oop_with_composition`] uses [`OopComposition::registration_name`] instead.
    /// This value remains the configuration-key fallback when
    /// [`Self::config_gear_name`](field@Self::config_gear_name) is `None`.
    pub gear_name: String,

    /// Key of the `gears.<key>` configuration section.
    ///
    /// `None` selects [`Self::gear_name`] in both entrypoints.
    /// The composition's registration name does not change this key.
    pub config_gear_name: Option<String>,

    /// Instance ID (defaults to a random UUID if None)
    pub instance_id: Option<Uuid>,

    /// Directory service gRPC endpoint (e.g., "<http://127.0.0.1:50051>")
    pub directory_endpoint: String,

    /// Path to configuration file
    pub config_path: Option<PathBuf>,

    /// Log verbosity level (0=default, 1=debug, 2=trace)
    pub verbose: u8,

    /// Print configuration and exit before running the gear.
    ///
    /// [`run_oop_with_options`] prints the loaded local configuration.
    /// [`run_oop_with_composition`] prints configuration after merging the rendered gear section
    /// with local overrides, before client construction. Both apply CLI verbosity overrides.
    pub print_config: bool,

    /// Heartbeat interval in seconds (default: 5)
    pub heartbeat_interval_secs: u64,

    /// Gear version (used for `DirectoryService` registration and `OpenAPI` version).
    /// Defaults to `None`.
    pub version: Option<String>,
}

impl OopRunOptions {
    /// Configuration key from [`Self::config_gear_name`](field@Self::config_gear_name),
    /// or [`Self::gear_name`] when unset.
    #[must_use]
    pub fn config_gear_name(&self) -> &str {
        self.config_gear_name.as_deref().unwrap_or(&self.gear_name)
    }
}

impl Default for OopRunOptions {
    fn default() -> Self {
        // Check for config path in environment variable as fallback
        let config_path = std::env::var("TOOLKIT_CONFIG_PATH").ok().map(PathBuf::from);

        // Check for directory endpoint in environment variable (set by master host)
        // This is the preferred way to get the endpoint when spawned by master host
        let directory_endpoint = std::env::var(TOOLKIT_DIRECTORY_ENDPOINT_ENV)
            .unwrap_or_else(|_| "http://127.0.0.1:50051".to_owned());

        Self {
            gear_name: String::new(),
            config_gear_name: None,
            instance_id: None,
            directory_endpoint,
            config_path,
            verbose: 0,
            print_config: false,
            heartbeat_interval_secs: 5,
            version: None,
        }
    }
}

/// Builds the final configuration and `DbOptions` for an `OoP` gear.
///
/// Configuration merge strategy (for each section):
/// - **Database**: field-by-field merge using `DbManager` (master as base, local as override)
/// - **Logging**: key-by-key merge (each subsystem key is overridden by local)
/// - **Config**: local completely replaces master if present
///
/// The local config file (--config) can override any settings from master's `TOOLKIT_MODULE_CONFIG`.
///
/// For database, the merge happens at 3 levels:
/// 1. Global database.servers.* from master
/// 2. Gear's database section from master (gears.<name>.database)
/// 3. Gear's database section from local --config (overrides master)
#[tracing::instrument(
    level = "debug",
    skip(local_config, rendered_config),
    fields(
        has_rendered = rendered_config.is_some(),
        has_local_db = local_config.database.is_some()
    )
)]
fn build_oop_config_and_db(
    local_config: &AppConfig,
    gear_name: &str,
    rendered_config: Option<&RenderedGearConfig>,
) -> Result<(AppConfig, LoggingConfig, DbOptions)> {
    let home_dir = PathBuf::from(&local_config.server.home_dir);
    let final_config = effective_oop_config(local_config, gear_name, rendered_config);

    // Merge logging: master logging (base) + local logging (override by key)
    let final_logging = merge_logging_configs(
        rendered_config.as_ref().and_then(|r| r.logging.as_ref()),
        &local_config.logging,
    );

    // Build DbOptions using Figment merge + DbManager
    // This allows field-by-field merge: master db config (base) -> local db config (override)
    let db_options = build_merged_db_options(
        &home_dir,
        gear_name,
        rendered_config.as_ref().and_then(|r| r.database.as_ref()),
        local_config,
    )?;

    Ok((final_config, final_logging, db_options))
}

/// Merge the master's rendered gear config with local overrides, without I/O.
/// Shared by runtime and deployment publication to produce identical manifests.
fn effective_oop_config(
    local_config: &AppConfig,
    gear_name: &str,
    rendered_config: Option<&RenderedGearConfig>,
) -> AppConfig {
    if let Some(rendered) = rendered_config {
        // TOOLKIT_MODULE_CONFIG exists: use rendered config as BASE, local config as OVERRIDE
        let mut config = local_config.clone();

        // Get or create the gear entry
        let gear_entry = config
            .gears
            .entry(gear_name.to_owned())
            .or_insert_with(|| serde_json::json!({}));

        // Merge rendered.config as base, local gear config as override
        if let Some(obj) = gear_entry.as_object_mut() {
            // If local doesn't have "config" section, use rendered entirely
            // If local has "config" section, it takes precedence (local overrides master)
            if !obj.contains_key("config") || obj["config"].is_null() {
                obj.insert("config".to_owned(), rendered.config.clone());
            }
            // If local has "config", it already overrides - no action needed
            // `clients` follows the same rule.
            if !rendered.clients.is_null()
                && (!obj.contains_key("clients") || obj["clients"].is_null())
            {
                obj.insert("clients".to_owned(), rendered.clients.clone());
            }
        }

        debug!(
            gear =  %gear_name,
            has_rendered_db = %rendered.database.is_some(),
            has_rendered_logging = %rendered.logging.is_some(),
            "Using rendered config from master as base, local config as override"
        );

        config
    } else {
        // No TOOLKIT_MODULE_CONFIG: use local config entirely (standalone mode)
        debug!(
            gear =  %gear_name,
            "No rendered config from master, using local config entirely (standalone mode)"
        );
        local_config.clone()
    }
}

/// Load the `OoP` unit's effective config: local file and CLI overrides merged
/// with `TOOLKIT_MODULE_CONFIG`, as for runtime startup, without starting gears.
/// Used by [`run_publish_gts`](super::run_publish_gts).
///
/// # Errors
/// The file cannot be loaded or the rendered configuration is invalid.
pub fn oop_publication_config(opts: &OopRunOptions) -> Result<AppConfig> {
    oop_publication_config_from(opts, rendered_config_from_env()?.as_ref())
}

/// The master's rendered configuration from `TOOLKIT_MODULE_CONFIG`: `None`
/// when the variable is absent. The `OoP` runtime and the deployment job share
/// it, so both fail on the same invalid value.
///
/// # Errors
/// The variable is present but not a valid rendered configuration.
fn rendered_config_from_env() -> Result<Option<RenderedGearConfig>> {
    parse_rendered_config(std::env::var(TOOLKIT_MODULE_CONFIG_ENV).ok().as_deref())
}

fn parse_rendered_config(value: Option<&str>) -> Result<Option<RenderedGearConfig>> {
    value
        .map(RenderedGearConfig::from_json)
        .transpose()
        .with_context(|| format!("{TOOLKIT_MODULE_CONFIG_ENV} is set but invalid"))
}

fn oop_publication_config_from(
    opts: &OopRunOptions,
    rendered: Option<&RenderedGearConfig>,
) -> Result<AppConfig> {
    let mut config = AppConfig::load_or_default(opts.config_path.as_ref())?;
    config.apply_cli_overrides(opts.verbose);
    Ok(effective_oop_config(
        &config,
        opts.config_gear_name(),
        rendered,
    ))
}

/// Merges logging configurations: master as base, local as override (by key).
///
/// Each key in the logging `HashMap` (e.g., "default", "calculator", "sqlx")
/// is overridden by local if present.
fn merge_logging_configs(master: Option<&LoggingConfig>, local: &LoggingConfig) -> LoggingConfig {
    master
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(local.clone())
        .collect()
}

/// Builds `DbOptions` by merging rendered config from master with local config.
///
/// Uses Figment to merge configurations and `DbManager` to handle the actual
/// database connection setup with field-by-field merge logic.
fn build_merged_db_options(
    home_dir: &Path,
    gear_name: &str,
    rendered_db: Option<&RenderedDbConfig>,
    local_config: &AppConfig,
) -> Result<DbOptions> {
    // Check if we have any database configuration
    let has_rendered_db = rendered_db.is_some_and(|db| db.gear.is_some() || db.global.is_some());
    let has_local_db = local_config.database.is_some()
        || local_config
            .gears
            .values()
            .any(|m| m.get("database").is_some());

    if !has_rendered_db && !has_local_db {
        debug!(
            gear =  %gear_name,
            "No database config available"
        );
        return Ok(DbOptions::None);
    }

    // Build a merged configuration for DbManager:
    // 1. Start with rendered config from master (global servers + gear db)
    // 2. Overlay local config (local can override any field)

    let mut merged_config = serde_json::Map::new();

    // Step 1: Add rendered database config from master as base
    if let Some(rendered) = rendered_db {
        // Add global servers from master
        if let Some(ref global) = rendered.global {
            let global_json = serde_json::to_value(global)
                .context("Failed to serialize rendered global db config")?;
            merged_config.insert("database".to_owned(), global_json);
        }

        // Add gear's database config from master
        if let Some(ref gear_db) = rendered.gear {
            let gear_db_json = serde_json::to_value(gear_db)
                .context("Failed to serialize rendered gear db config")?;

            let mut gears = serde_json::Map::new();
            let mut gear_entry = serde_json::Map::new();
            gear_entry.insert("database".to_owned(), gear_db_json);
            gears.insert(gear_name.to_owned(), serde_json::Value::Object(gear_entry));
            merged_config.insert("gears".to_owned(), serde_json::Value::Object(gears));
        }
    }

    // Step 2: Overlay local config (local overrides master)
    // Local global database config
    if let Some(ref local_db) = local_config.database {
        let local_db_json =
            serde_json::to_value(local_db).context("Failed to serialize local global db config")?;

        // Merge with existing or replace
        if let Some(existing) = merged_config.get_mut("database") {
            merge_json_objects(existing, &local_db_json);
        } else {
            merged_config.insert("database".to_owned(), local_db_json);
        }
    }

    // Local gear database config: the unit's own gear, and every gear it co-hosts
    // that names a database in the local config (the master renders only its own).
    for (local_gear_name, local_gear) in &local_config.gears {
        let Some(local_gear_db) = local_gear.get("database") else {
            continue;
        };
        let gears = merged_config
            .entry("gears".to_owned())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

        if let Some(gears_obj) = gears.as_object_mut() {
            let gear_entry = gears_obj
                .entry(local_gear_name.clone())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

            if let Some(gear_obj) = gear_entry.as_object_mut() {
                if let Some(existing_db) = gear_obj.get_mut("database") {
                    merge_json_objects(existing_db, local_gear_db);
                } else {
                    gear_obj.insert("database".to_owned(), local_gear_db.clone());
                }
            }
        }
    }

    debug!(
        gear =  %gear_name,
        has_rendered = %rendered_db.is_some(),
        has_local_global = %local_config.database.is_some(),
        "Building DbManager with merged config"
    );

    // Create DbManager from merged Figment
    let figment = Figment::new().merge(Serialized::defaults(serde_json::Value::Object(
        merged_config,
    )));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, home_dir.to_path_buf())
            .context("Failed to create DbManager from merged config")?,
    );

    Ok(DbOptions::Manager(db_manager))
}

/// Recursively merges source JSON object into target.
/// Source values override target values for matching keys.
fn merge_json_objects(target: &mut serde_json::Value, source: &serde_json::Value) {
    if let (Some(target_obj), Some(source_obj)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source_obj {
            if let Some(target_value) = target_obj.get_mut(key) {
                // Recursively merge objects, otherwise replace
                if target_value.is_object() && value.is_object() {
                    merge_json_objects(target_value, value);
                } else {
                    *target_value = value.clone();
                }
            } else {
                target_obj.insert(key.clone(), value.clone());
            }
        }
    } else {
        // If target is not an object, replace entirely
        *target = source.clone();
    }
}

/// Runs an out-of-process gear under the process signal token.
///
/// HTTP-serving gears manage directory presence through the serving lifecycle.
/// Without HTTP configuration, a heartbeat maintains externally registered instances.
/// Both modes use a lazy directory connection so a temporarily unavailable directory
/// does not block startup.
///
/// # Example
///
/// ```rust,no_run
/// use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let opts = OopRunOptions {
///         gear_name: "file-parser".to_string(),
///         config_gear_name: None,
///         instance_id: None,
///         directory_endpoint: "http://127.0.0.1:50051".to_string(),
///         config_path: None,
///         verbose: 1,
///         print_config: false,
///         heartbeat_interval_secs: 5,
///         version: None,
///     };
///
///     run_oop_with_options(opts).await
/// }
/// ```
///
/// # Errors
///
/// Returns an error if configuration, setup, or the gear lifecycle fails.
#[tracing::instrument(
    level = "info",
    name = "oop_bootstrap",
    skip(opts),
    fields(
        gear =  %opts.gear_name,
        directory = %opts.directory_endpoint
    )
)]
pub async fn run_oop_with_options(opts: OopRunOptions) -> Result<()> {
    let cancel = process_cancellation()?;
    let _on_exit = cancel.clone().drop_guard();
    let Some(prepared) = prepare(&opts, &cancel, Invocation::Legacy)? else {
        return Ok(());
    };
    let http = prepared.config.oop_http.clone();
    let connected = prepared.connect(&opts.directory_endpoint).await?;
    let result = if let Some(cfg) = http {
        let (run_options, serve) = connected
            .prepare_serving(&cfg, &opts.gear_name, &opts)
            .await?;
        run_oop_serving(run_options, serve).await
    } else {
        info!("Starting gear lifecycle (legacy gRPC-only)");
        start_legacy_heartbeat(
            connected.directory,
            &opts,
            connected.run_options.instance_id,
            &cancel,
        );
        run(connected.run_options).await
    };
    report_runtime_result(&result);
    #[cfg(feature = "otel")]
    super::telemetry::tracing_shutdown().await;
    result
}

/// Runs an HTTP-serving gear with application-supplied clients and a registration name.
///
/// The factory runs once after configuration and directory credentials are ready.
/// Printing the effective configuration returns before client construction.
/// After successful composition, bootstrap awaits the host lifecycle, then client cleanup
/// under the server shutdown deadline, then telemetry shutdown.
///
/// # Cancellation safety
///
/// Dropping this future during serving preparation, execution, or unfinished client cleanup
/// requests abort through the cleanup owner's token. A drop or deadline does not join workers.
///
/// # Errors
///
/// Returns the factory's typed error, a bootstrap failure, a cleanup failure, or both failures.
pub async fn run_oop_with_composition(
    options: OopRunOptions,
    compose: OopCompositionFactory,
) -> Result<(), OopCompositionRunError> {
    let cancel = process_cancellation().map_err(OopCompositionRunError::bootstrap)?;
    let _on_exit = cancel.clone().drop_guard();
    let Some(prepared) = prepare(&options, &cancel, Invocation::Composed)
        .map_err(OopCompositionRunError::bootstrap)?
    else {
        return Ok(());
    };
    let config = prepared.config.clone();
    let http = config
        .oop_http
        .as_ref()
        .ok_or(OopCompositionError::InvalidConfig { field: "oop_http" })?;
    let mut connected = prepared
        .connect(&options.directory_endpoint)
        .await
        .map_err(OopCompositionRunError::bootstrap)?;
    let context = OopCompositionContext {
        cancel: cancel.clone(),
        internal_token_provider: connected.run_options.internal_token_provider.clone(),
    };
    let OopComposition {
        registration_name,
        clients,
        cleanup,
    } = compose(&config, &context)?;
    connected.run_options.clients.extend(clients);
    let host = async {
        let (run_options, serve) = connected
            .prepare_serving(http, &registration_name, &options)
            .await?;
        run_oop_serving(run_options, serve).await
    };
    let result = Box::pin(finalize_composition(host, cleanup, &cancel, &config.server)).await;
    report_runtime_result(&result);
    #[cfg(feature = "otel")]
    super::telemetry::tracing_shutdown().await;
    result
}

async fn finalize_composition(
    host: impl std::future::Future<Output = Result<()>>,
    cleanup: ClientCleanup,
    cancel: &CancellationToken,
    server: &super::config::ServerConfig,
) -> Result<(), OopCompositionRunError> {
    let result = host.await;
    if result.is_err() {
        cancel.cancel();
    }
    let deadline = server
        .shutdown_deadline()
        .unwrap_or(crate::runtime::DEFAULT_SHUTDOWN_DEADLINE);
    let completion = tokio::time::timeout(deadline, cleanup.finish())
        .await
        .unwrap_or(Err(ClientCleanupError::DeadlineExceeded));
    match (result, completion) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(source), Ok(())) => Err(OopCompositionRunError::bootstrap(source)),
        (Ok(()), Err(cleanup)) => Err(cleanup.into()),
        (Err(source), Err(cleanup)) => Err(OopCompositionRunError::BootstrapAndCleanup {
            source: source.reallocate_into_boxed_dyn_error_without_backtrace(),
            cleanup,
        }),
    }
}

#[derive(Clone, Copy)]
enum Invocation {
    Legacy,
    Composed,
}

struct Prepared {
    config: Arc<AppConfig>,
    db: DbOptions,
    cancel: CancellationToken,
    instance_id: Uuid,
}

struct Connected {
    run_options: RunOptions,
    directory: Arc<dyn DirectoryClient>,
}

impl Prepared {
    async fn connect(self, endpoint: &str) -> Result<Connected> {
        let auth = self
            .config
            .oop_http
            .as_ref()
            .and_then(|cfg| cfg.internal_auth.as_ref());
        info!("Creating directory service client (lazy connect) for {endpoint}");
        let (directory, provider) = build_directory_client(endpoint, auth, &self.cancel).await?;
        info!("Directory service client ready (will connect on first use)");
        let directory: Arc<dyn DirectoryClient> = Arc::new(directory);
        let init_deadline = self.config.server.init_deadline();
        let shutdown_deadline = self.config.server.shutdown_deadline();
        let run_options = RunOptions::new(
            self.config,
            self.db,
            ShutdownOptions::Token(self.cancel),
            self.instance_id,
        )
        .with_clients(vec![ClientRegistration::new::<dyn DirectoryClient>(
            directory.clone(),
        )])
        .with_internal_token_provider(provider)
        .with_init_deadline(init_deadline)
        .with_shutdown_deadline(shutdown_deadline);
        Ok(Connected {
            run_options,
            directory,
        })
    }
}

impl Connected {
    async fn prepare_serving(
        self,
        cfg: &super::config::OopHttpConfig,
        name: &str,
        opts: &OopRunOptions,
    ) -> Result<(RunOptions, OopServeOptions)> {
        let serve = build_oop_serve_options(
            cfg,
            name,
            self.run_options.instance_id,
            opts.version.clone(),
            Duration::from_secs(opts.heartbeat_interval_secs),
            self.directory,
        )
        .await?;
        Ok((self.run_options, serve))
    }
}

fn process_cancellation() -> Result<CancellationToken> {
    super::init_crypto_provider()?;
    Ok(arm_shutdown_wait(async {
        wait_for_shutdown_or_fallback(
            shutdown::wait_for_shutdown(),
            tokio::signal::ctrl_c(),
            std::io::stderr(),
        )
        .await;
        info!("shutdown: signal received in OoP bootstrap");
    }))
}

async fn wait_for_shutdown_or_fallback(
    primary: impl std::future::Future<Output = anyhow::Result<()>>,
    fallback: impl std::future::Future<Output = std::io::Result<()>>,
    mut diagnostic: impl std::io::Write,
) {
    use futures_util::FutureExt;

    let Err(error) = primary.await else {
        return;
    };
    let mut fallback = std::pin::pin!(fallback);
    // Arm Ctrl+C before stderr can block. The tracing subscriber may not be installed yet.
    let fallback_pending = fallback.as_mut().now_or_never().is_none();
    _ = writeln!(
        diagnostic,
        "OoP signal waiter failed, falling back to ctrl_c(): {error}"
    );
    drop(diagnostic);
    if fallback_pending {
        _ = fallback.await;
    }
}

fn arm_shutdown_wait(
    wait: impl std::future::Future<Output = ()> + Send + 'static,
) -> CancellationToken {
    use futures_util::FutureExt;

    let cancel = CancellationToken::new();
    let mut wait = Box::pin(wait);
    // Register listeners before clients can start. The same waiter owns them until shutdown.
    if wait.as_mut().now_or_never().is_some() {
        cancel.cancel();
    } else {
        let process = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = process.cancelled() => {},
                () = wait => process.cancel(),
            }
        });
    }
    cancel
}

fn prepare(
    opts: &OopRunOptions,
    cancel: &CancellationToken,
    invocation: Invocation,
) -> Result<Option<Prepared>> {
    let instance_id = opts.instance_id.unwrap_or_else(Uuid::new_v4);
    let mut config = AppConfig::load_or_default(opts.config_path.as_ref())?;
    config.apply_cli_overrides(opts.verbose);
    let rendered_config = rendered_config_from_env()?;

    if opts.print_config && matches!(invocation, Invocation::Composed) {
        print_config(&effective_oop_config(
            &config,
            opts.config_gear_name(),
            rendered_config.as_ref(),
        ));
        return Ok(None);
    }
    let (final_config, merged_logging, db_options) =
        build_oop_config_and_db(&config, opts.config_gear_name(), rendered_config.as_ref())?;
    // The master owns telemetry settings for every child process.
    #[cfg(feature = "otel")]
    let otel_cfg = rendered_config
        .as_ref()
        .and_then(|rc| rc.opentelemetry.as_ref());

    #[cfg(feature = "otel")]
    let otel_layer = otel_cfg
        .filter(|cfg| cfg.tracing.enabled)
        .map(crate::telemetry::init_tracing)
        .transpose()?;
    #[cfg(not(feature = "otel"))]
    let otel_layer = None;

    // Defer the metrics diagnostic until the logging subscriber is installed.
    #[cfg(feature = "otel")]
    let metrics_init_error = otel_cfg
        .filter(|cfg| cfg.metrics.enabled)
        .and_then(|cfg| crate::telemetry::init::init_metrics_provider(cfg).err());

    // An absent telemetry section enables trace IDs, matching the in-process default.
    #[cfg(feature = "otel")]
    let inject_trace_ids =
        otel_cfg.is_none_or(crate::telemetry::OpenTelemetryConfig::inject_trace_ids_into_logs);
    #[cfg(not(feature = "otel"))]
    let inject_trace_ids = false;

    init_logging_unified(
        &merged_logging,
        &config.server.home_dir,
        otel_layer,
        inject_trace_ids,
    );

    #[cfg(feature = "otel")]
    if let Some(e) = metrics_init_error {
        tracing::error!(error = %e, "OpenTelemetry metrics not initialized (OoP)");
    }

    init_panic_tracing();

    if let Some(ref rc) = rendered_config {
        info!(
            env_var = TOOLKIT_MODULE_CONFIG_ENV,
            has_database = rc.database.is_some(),
            has_config = !rc.config.is_null(),
            has_logging = rc.logging.is_some(),
            has_opentelemetry = rc.opentelemetry.is_some(),
            "Received rendered config from master host"
        );
    } else {
        debug!(
            env_var = TOOLKIT_MODULE_CONFIG_ENV,
            "No rendered config from master host, using local config only"
        );
    }

    info!(
        gear =  %opts.gear_name,
        instance_id = %instance_id,
        directory_endpoint = %opts.directory_endpoint,
        "OoP gear bootstrap starting"
    );

    if opts.print_config {
        print_config(&config);
        return Ok(None);
    }

    Ok(Some(Prepared {
        config: Arc::new(final_config),
        db: db_options,
        cancel: cancel.clone(),
        instance_id,
    }))
}

fn start_legacy_heartbeat(
    directory: Arc<dyn DirectoryClient>,
    opts: &OopRunOptions,
    instance_id: Uuid,
    cancel: &CancellationToken,
) {
    let heartbeat_gear = opts.gear_name.clone();
    let instance = instance_id.to_string();
    let heartbeat_interval = Duration::from_secs(opts.heartbeat_interval_secs.max(1));
    let cancel = cancel.child_token();
    tokio::spawn(async move {
        info!(interval_secs = ?heartbeat_interval, "Starting legacy heartbeat loop");
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Heartbeat loop stopping due to cancellation");
                    break;
                }
                () = sleep(heartbeat_interval) => {
                    if let Err(error) = directory.send_heartbeat(&heartbeat_gear, &instance).await {
                        warn!(%error, "Failed to send heartbeat, will retry");
                    }
                }
            }
        }
    });
}

fn report_runtime_result<E: std::fmt::Display>(result: &Result<(), E>) {
    if let Err(error) = result {
        error!(%error, "Gear runtime failed");
    } else {
        info!("Gear runtime completed successfully");
    }
}

/// Build [`OopServeOptions`] from configuration.
///
/// The tenant-plane `BearerAuthenticator` injection point is left unset here;
/// the app/gear binary supplies that adapter when it needs the tenant-plane
/// middleware installed. The platform-plane authenticator is constructed from
/// `oop_http.internal_auth` when the `k8s-auth` feature is enabled.
async fn build_oop_serve_options(
    cfg: &super::config::OopHttpConfig,
    gear_name: &str,
    instance_id: Uuid,
    version: Option<String>,
    heartbeat_interval: Duration,
    directory: Arc<dyn DirectoryClient>,
) -> Result<OopServeOptions> {
    let listen_addr: std::net::SocketAddr = cfg
        .listen_addr
        .parse()
        .with_context(|| format!("invalid oop_http.listen_addr: {}", cfg.listen_addr))?;

    let probe_bind_addr = cfg
        .probe_bind_addr
        .as_deref()
        .map(|s| {
            s.parse::<std::net::SocketAddr>()
                .with_context(|| format!("invalid oop_http.probe_bind_addr: {s}"))
        })
        .transpose()?;

    let advertise_uri = cfg
        .advertise_uri
        .clone()
        .unwrap_or_else(|| default_advertise_uri(listen_addr));

    // Fail fast on a malformed or unreachable advertise_uri rather than only
    // when the directory rejects the registration — the same late-failure the
    // label validation below avoids.
    validate_advertise_uri(&advertise_uri, cfg.allow_loopback_advertise)?;
    log_auth_disabled_when_routable(cfg.auth_disabled, &advertise_uri);

    // Fail fast on a mis-configured label (bad charset, over-long, too many)
    // at start-up, using the same shared rules the directory enforces. Without
    // this the process would come up, attempt to register, and only then be
    // permanently rejected by the directory — a confusing late failure for what
    // is a static configuration error. Checked before authenticator init so a
    // static config error is reported without any async backend setup.
    cf_system_sdks::directory::validate_labels(&cfg.labels).with_context(|| {
        "invalid oop_http.labels: label keys/values must be <=63 chars, <=64 entries, and use \
         only ASCII alphanumerics plus '-', '_', '.' (starting and ending alphanumeric)"
    })?;

    let internal_authenticator = build_internal_authenticator(cfg.internal_auth.as_ref()).await?;

    Ok(OopServeOptions {
        gear_name: gear_name.to_owned(),
        instance_id: instance_id.to_string(),
        version,
        advertise_uri,
        listen_addr,
        probe_bind_addr,
        drain_timeout: Duration::from_secs(cfg.drain_timeout_secs),
        heartbeat_interval,
        healthcheck_timeout: Duration::from_millis(cfg.healthcheck_timeout_ms),
        request_limits: cfg.request_limits,
        directory,
        bearer_authenticator: None,
        internal_authenticator,
        auth_disabled: cfg.auth_disabled,
        labels: cfg.labels.clone(),
    })
}

/// Construct the inbound platform-plane authenticator from configuration, via
/// the shared [`toolkit_security::build_inbound_authenticator`] (also used by
/// api-gateway and `grpc-hub`). `provider: kube` needs the `k8s-auth` feature;
/// without it, it is an error rather than a silent enforcement downgrade.
/// `Ok(None)` only when no `internal_auth` is configured (Profile 1).
async fn build_internal_authenticator(
    cfg: Option<&toolkit_security::InternalAuthConfig>,
) -> Result<Option<toolkit_security::DynInternalAuthenticator>> {
    #[cfg(feature = "k8s-auth")]
    let kube = Some(toolkit_k8s_auth::kube_authenticator_builder(
        Some(toolkit_security::DEFAULT_TOKEN_REVIEW_CACHE_TTL),
        None,
    ));
    #[cfg(not(feature = "k8s-auth"))]
    let kube = None;
    toolkit_security::build_inbound_authenticator(cfg, kube)
        .await
        .map_err(|e| anyhow::anyhow!("oop_http: {e}"))
}

/// Build the `OoP` gear's `DirectoryService` client on a **lazy** channel, plus
/// the outbound
/// [`InternalTokenProvider`](toolkit_contract::runtime::config::InternalTokenProvider)
/// when a platform-plane credential is configured (`None` => no credential).
///
/// The lazy channel performs no eager connection, so this succeeds even when
/// `directory_endpoint` is unreachable (`cpt-cf-adr-eventual-readiness`).
///
/// The provider is built by the shared
/// [`build_internal_token_provider`](toolkit_contract::runtime::platform_credential::build_internal_token_provider),
/// and the directory client's interceptor is a thin adapter over it, so the two
/// can't diverge after a rotation and follow the same attach policy. An `OoP`
/// gear always registers and heartbeats with `DirectoryService`, so a kube
/// config without a `present` block is a startup error here.
///
/// # Errors
/// Only if the endpoint is malformed or a configured credential source fails to
/// initialise — never for an unreachable directory. The endpoint is validated
/// before any credential work, so a malformed endpoint is reported ahead of a
/// credential-source failure.
async fn build_directory_client(
    directory_endpoint: &str,
    internal_auth_cfg: Option<&toolkit_security::InternalAuthConfig>,
    cancel: &CancellationToken,
) -> Result<(
    DirectoryGrpcClient,
    Option<toolkit_contract::runtime::config::InternalTokenProvider>,
)> {
    // Validate the endpoint up front — before any credential-source work — so a
    // malformed endpoint is reported ahead of a credential failure. The lazy
    // client performs no connection, only URI validation; it is reused as-is on
    // the no-credential path.
    let client = DirectoryGrpcClient::connect_lazy(directory_endpoint)?;

    let Some(cfg) = internal_auth_cfg else {
        return Ok((client, None));
    };

    let provider =
        toolkit_contract::runtime::platform_credential::build_internal_token_provider(cfg, cancel)
            .await?;
    let interceptor = toolkit_contract::runtime::platform_credential::internal_auth_interceptor(
        Some(&provider),
        "directory",
    );
    // Rebuild with the interceptor attached (it must be set at construction);
    // the endpoint was already validated above.
    let client =
        DirectoryGrpcClient::connect_lazy_with_interceptor(directory_endpoint, interceptor)?;
    Ok((client, Some(provider)))
}

/// Derive a default advertise URI from the bind address. Unspecified hosts
/// (`0.0.0.0` / `[::]`) are rewritten to loopback, and every IPv6 host is
/// enclosed in brackets so the resulting URI is valid when registered as a
/// REST endpoint.
fn default_advertise_uri(listen_addr: std::net::SocketAddr) -> String {
    let host = match listen_addr {
        std::net::SocketAddr::V4(addr) if addr.ip().is_unspecified() => "127.0.0.1".to_owned(),
        std::net::SocketAddr::V4(addr) => addr.ip().to_string(),
        std::net::SocketAddr::V6(addr) if addr.ip().is_unspecified() => "[::1]".to_owned(),
        std::net::SocketAddr::V6(addr) => format!("[{}]", addr.ip()),
    };
    format!("http://{host}:{}", listen_addr.port())
}

/// Reject a malformed or unreachable `advertise_uri` at start-up (fail fast).
///
/// Checks shape (parseable `http`/`https` URL, non-empty host, no userinfo) and,
/// unless `allow_loopback`, rejects a loopback / unspecified host - a
/// registered-but-unreachable instance in multi-host Profile 2 / Profile 3
/// (`cpt-cf-adr-instance-addressable-discovery` section 5). The default derives
/// loopback from an unspecified bind, so this also covers an *unset* value. The
/// directory enforces its full endpoint ruleset server-side.
fn validate_advertise_uri(uri: &str, allow_loopback: bool) -> Result<()> {
    let parsed = url::Url::parse(uri)
        .with_context(|| format!("invalid oop_http.advertise_uri: not a valid URL: {uri}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!(
            "invalid oop_http.advertise_uri: scheme must be http or https (got '{}')",
            parsed.scheme()
        );
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        anyhow::bail!("invalid oop_http.advertise_uri: missing host: {uri}");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("invalid oop_http.advertise_uri: must not contain userinfo: {uri}");
    }
    if !allow_loopback && is_loopback_host(&parsed) {
        anyhow::bail!(
            "invalid oop_http.advertise_uri: '{uri}' is a loopback/unspecified address, which is \
             unreachable by other gears in multi-host Profile 2 / Profile 3 (a registered-but-\
             unreachable instance). Set oop_http.advertise_uri to a routable host, or set \
             oop_http.allow_loopback_advertise = true for single-host / local-dev."
        );
    }
    Ok(())
}

/// Whether `uri`'s host is loopback / unspecified, i.e. unreachable from other hosts.
fn is_loopback_host(uri: &url::Url) -> bool {
    match uri.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Domain(d)) => d.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Log an `error!` when the development-only `auth_disabled` is set on a worker advertised
/// at a routable address: other hosts reach it through the directory, and every tenant's
/// request then runs as the default tenant.
fn log_auth_disabled_when_routable(auth_disabled: bool, advertise_uri: &str) {
    if !auth_disabled {
        return;
    }
    let routable = url::Url::parse(advertise_uri).is_ok_and(|uri| !is_loopback_host(&uri));
    if routable {
        tracing::error!(
            advertise_uri,
            "oop_http.auth_disabled is set on a worker advertised at a routable address: every \
             tenant request reaching it runs as the default tenant. Development only; unset it \
             or advertise a loopback address."
        );
    }
}

#[allow(unknown_lints, de1301_no_print_macros)] // direct stdout config print before exit
fn print_config(config: &AppConfig) {
    match config.to_yaml() {
        Ok(yaml) => {
            println!("{yaml}");
        }
        Err(e) => {
            eprintln!("Failed to render config as YAML: {e}");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "oop_tests.rs"]
mod tests;
