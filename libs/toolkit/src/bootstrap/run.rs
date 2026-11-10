use super::config::{get_gear_runtime_config, render_gear_config_for_oop};
use super::host::{init_logging_unified, init_panic_tracing, normalize_path};
#[cfg(feature = "otel")]
use super::telemetry::tracing_shutdown;
use super::{AppConfig, RuntimeKind};
use crate::backends::LocalProcessBackend;
use crate::runtime::{
    DbOptions, OopGearSpawnConfig, OopSpawnOptions, RunOptions, ShutdownOptions, run, shutdown,
};
use anyhow::Result;
use figment::Figment;
use figment::providers::Serialized;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Spawn a signal handler task that cancels the provided token on SIGTERM/SIGINT.
///
/// This helper consolidates signal handling logic used by both `run_server` and `run_migrate`.
/// The `context` parameter customizes log messages for better diagnostics.
fn spawn_signal_handler(cancel: CancellationToken, context: &str) {
    let context_owned = context.to_owned();
    tokio::spawn(async move {
        match shutdown::wait_for_shutdown().await {
            Ok(()) => {
                tracing::info!(target: "", "------------------");
                tracing::info!("{}: shutdown signal received", context_owned);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "{}: signal handler failed, falling back to ctrl_c()",
                    context_owned
                );
                _ = tokio::signal::ctrl_c().await;
            }
        }
        cancel.cancel();
    });
}

/// # Errors
///
/// Returns an error if:
/// - There was a critical error during initialization of the gears
/// - Problems with the database or third-party services
/// - An issue during runtime or shutdown
///
/// The TLS crypto provider is installed automatically as the first step of
/// [`init_procedure`] (idempotent), so callers do not need to invoke
/// [`super::init_crypto_provider`] explicitly.
pub async fn run_server(config: AppConfig) -> Result<()> {
    init_procedure(&config).map_err(|e| {
        tracing::error!(error = %e, "Initialization failed");
        e
    })?;
    tracing::info!("Initializing gears...");

    // Generate process-level instance ID once at startup.
    // This is shared by all gears in this process.
    let instance_id = uuid::Uuid::new_v4();
    tracing::info!(instance_id = %instance_id, "Generated process instance ID");

    // Create root cancellation token for the entire process.
    // This token drives shutdown for the gear runtime and all lifecycle/stateful gears.
    let cancel = CancellationToken::new();

    // Hook OS signals to the root token at the host level.
    // This replaces the use of ShutdownOptions::Signals inside the runtime.
    spawn_signal_handler(cancel.clone(), "server");

    // Build config provider and resolve database options
    let db_options = resolve_db_options(&config)?;

    // Create OoP backend with cancellation token - it will auto-shutdown all processes on cancel
    let oop_backend = LocalProcessBackend::new(cancel.clone());

    // Build OoP spawn configuration
    let oop_options = build_oop_spawn_options(&config, oop_backend)?;

    // Remote platform-plane calls carry `X-ToolKit-Internal-Token` from
    // `oop_http.internal_auth` (cpt-cf-adr-two-plane-auth); without it they fail.
    // Co-hosted dependencies use local ClientHub objects and need no credential.
    let internal_token_provider = match config
        .oop_http
        .as_ref()
        .and_then(|http| http.internal_auth.as_ref())
    {
        Some(internal_auth) => {
            super::oop::build_platform_credentials(internal_auth, &cancel)
                .await?
                .1
        }
        None => None,
    };

    // Apply configured lifecycle deadlines (absent fields keep framework defaults).
    let init_deadline = config.server.init_deadline();
    let shutdown_deadline = config.server.shutdown_deadline();

    // Run the ToolKit runtime with the root cancellation token.
    // Shutdown is driven by the signal handler spawned above, not by ShutdownOptions::Signals.
    // OoP gears are spawned after parent publication, application startup and
    // REST provider registration, so the parent's complete API is available.
    let run_options = RunOptions::new(
        Arc::new(config),
        db_options,
        ShutdownOptions::Token(cancel.clone()),
        instance_id,
    )
    .with_oop(oop_options)
    .with_internal_token_provider(internal_token_provider)
    .with_init_deadline(init_deadline)
    .with_shutdown_deadline(shutdown_deadline);

    let result = run(run_options).await;

    // Graceful shutdown - flush remaining telemetry
    #[cfg(feature = "otel")]
    tracing_shutdown().await;

    result
}

/// Run database migrations and exit.
///
/// This mode is designed for cloud deployment workflows where database
/// migrations need to run as a separate step before starting the application.
///
/// Phases executed:
/// - Pre-init (wire runtime internals)
/// - DB migration (run all pending migrations)
///
/// The process exits after migrations complete. Any errors are reported
/// and propagated as non-zero exit codes.
///
/// # Errors
///
/// Returns an error if:
/// - No database configuration is found
/// - Gear discovery fails
/// - Pre-init phase fails
/// - Migration phase fails
///
/// The TLS crypto provider is installed automatically as the first step of
/// [`init_procedure`] (idempotent), so callers do not need to invoke
/// [`super::init_crypto_provider`] explicitly.
pub async fn run_migrate(config: AppConfig) -> Result<()> {
    init_procedure(&config).map_err(|e| {
        tracing::error!(error = %e, "Initialization failed");
        e
    })?;
    tracing::info!("Starting migration mode...");

    // Generate process-level instance ID for this migration run
    let instance_id = uuid::Uuid::new_v4();
    tracing::info!(instance_id = %instance_id, "Generated migration instance ID");

    // Create cancellation token and wire it to OS signals
    let cancel = CancellationToken::new();

    // Hook OS signals to enable graceful cancellation of migrations
    spawn_signal_handler(cancel.clone(), "migration");

    // Build database options from configuration
    let db_options = resolve_db_options(&config)?;

    // Verify we have database configuration
    if matches!(db_options, DbOptions::None) {
        anyhow::bail!("Cannot run migrations: no database configuration found");
    }

    // Discover and build the gear registry
    let registry = crate::registry::GearRegistry::discover_and_build()?;
    tracing::info!(
        gear_count = registry.gears().len(),
        "Discovered gears for migration"
    );

    // Create the host runtime
    let host = crate::runtime::HostRuntime::new(
        registry,
        Arc::new(config),
        db_options,
        Arc::new(crate::client_hub::ClientHub::new()),
        cancel,
        instance_id,
        None, // No OoP spawning during migration
    );

    // Run only the migration phases (pre-init + DB migration)
    let result = host.run_migration_phases().await;

    // Graceful shutdown - flush remaining telemetry
    #[cfg(feature = "otel")]
    tracing_shutdown().await;

    result?;

    tracing::info!("All migrations completed successfully");
    Ok(())
}

/// Options of [`run_publish_gts`].
#[derive(Debug, Clone)]
pub struct PublishGtsOptions {
    /// Base URI of the remote registry, e.g. `https://registry:8443`.
    pub registry_endpoint: String,
    /// The overall deadline for every publication; at most
    /// [`MAX_PUBLICATION_BUDGET`](crate::gts_publication::MAX_PUBLICATION_BUDGET) (10 minutes).
    pub deadline: std::time::Duration,
}

/// Publish the GTS manifests of every linked gear to a remote registry and exit:
/// a deployment step.
///
/// No gear runs: no `init`, database, bootstrap or plugin binding. Manifests
/// come from the effective configuration (file, environment overrides,
/// defaults), as at application startup. Requests carry the platform
/// credential of `oop_http.internal_auth`. All publications run concurrently;
/// see [`crate::gts_publication::deployment`].
///
/// # Errors
/// No platform credential is configured, setup fails (manifest, ownership,
/// wiring), or any publisher is refused or misses the deadline: the report is
/// in the error, naming each publisher and its GTS ids.
pub async fn run_publish_gts(config: AppConfig, options: PublishGtsOptions) -> Result<()> {
    init_procedure(&config).map_err(|e| {
        tracing::error!(error = %e, "Initialization failed");
        e
    })?;
    // SIGTERM/SIGINT end the job, reported as cancelled. Dropping this future
    // cancels too: the guard also ends the credential watcher.
    let cancel = CancellationToken::new();
    spawn_signal_handler(cancel.clone(), "publish-gts");
    let _cancel_on_drop = cancel.clone().drop_guard();
    let result = publish_gts(&config, &options, &cancel).await;

    // The outcome is logged before the telemetry flush, on every path.
    match &result {
        Ok(()) => tracing::info!("GTS publication complete"),
        Err(e) => tracing::error!(error = format!("{e:#}"), "GTS publication failed"),
    }
    #[cfg(feature = "otel")]
    tracing_shutdown().await;
    result
}

/// [`run_publish_gts`] after initialization.
async fn publish_gts(
    config: &AppConfig,
    options: &PublishGtsOptions,
    cancel: &CancellationToken,
) -> Result<()> {
    // Reject an over-long deadline before touching the registry or the credential.
    crate::gts_publication::check_publication_budget("--timeout", options.deadline)?;
    // Match startup validation without running gear lifecycle hooks.
    let registry = crate::registry::GearRegistry::discover_and_build()?;
    validate_deployment_configuration(&registry, config)?;
    let internal_auth = platform_credential(config)?;
    let (_, provider) = super::oop::build_platform_credentials(internal_auth, cancel).await?;
    let provider = provider.ok_or_else(|| {
        anyhow::anyhow!("oop_http.internal_auth gives no outbound platform credential")
    })?;
    let tuning = toolkit_contract::wiring::ClientTuning::default()
        .with_internal_token_provider(Some(provider));
    let descriptors = crate::gts_publication::linked_descriptors();
    tracing::info!(
        publishers = descriptors.len(),
        registry = %options.registry_endpoint,
        "publishing GTS manifests"
    );
    let report = crate::gts_publication::deployment::publish(
        &descriptors,
        config,
        &options.registry_endpoint,
        &tuning,
        options.deadline,
        cancel,
    )
    .await?;
    publication_result(&report)
}

fn validate_deployment_configuration(
    registry: &crate::registry::GearRegistry,
    config: &dyn crate::config::ConfigProvider,
) -> Result<()> {
    for entry in registry.gears() {
        entry.validate_gts_publication_config(config)?;
    }
    Ok(())
}

/// The platform credential `publish-gts` sends: `oop_http.internal_auth`.
fn platform_credential(config: &AppConfig) -> Result<&toolkit_security::InternalAuthConfig> {
    config
        .oop_http
        .as_ref()
        .and_then(|http| http.internal_auth.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "publish-gts needs a platform credential: configure oop_http.internal_auth"
            )
        })
}

/// The job's result: an error carrying the report unless every publisher succeeded.
fn publication_result(
    report: &crate::gts_publication::deployment::GtsDeploymentReport,
) -> Result<()> {
    if report.succeeded() {
        tracing::info!("GTS publication report:\n{report}");
        Ok(())
    } else {
        anyhow::bail!("GTS publication failed:\n{report}")
    }
}

fn resolve_db_options(config: &AppConfig) -> Result<DbOptions> {
    if config.database.is_none() {
        tracing::warn!("No global database section found; running without databases");
        return Ok(DbOptions::None);
    }

    tracing::info!("Using DbManager with Figment-based configuration");
    let figment = Figment::new().merge(Serialized::defaults(config));
    let db_manager = Arc::new(toolkit_db::DbManager::from_figment(
        figment,
        config.server.home_dir.clone(),
    )?);
    Ok(DbOptions::Manager(db_manager))
}

/// Build `OoP` spawn configuration from `AppConfig`.
///
/// This collects all gears with `type=oop` and prepares their spawn configuration.
/// The actual spawning happens in the `HostRuntime` after parent startup.
fn build_oop_spawn_options(
    config: &AppConfig,
    backend: LocalProcessBackend,
) -> Result<Option<OopSpawnOptions>> {
    let home_dir = PathBuf::from(&config.server.home_dir);
    let mut gears = Vec::new();

    for gear_name in config.gears.keys() {
        if let Some(spawn_config) = try_build_oop_gear_config(config, gear_name, &home_dir)? {
            gears.push(spawn_config);
        }
    }

    if gears.is_empty() {
        Ok(None)
    } else {
        tracing::info!(count = gears.len(), "Prepared OoP gears for spawning");
        Ok(Some(OopSpawnOptions {
            gears,
            backend: Box::new(backend),
        }))
    }
}

/// Try to build `OoP` gear spawn config if gear is of type `OoP`
fn try_build_oop_gear_config(
    config: &AppConfig,
    gear_name: &str,
    home_dir: &Path,
) -> Result<Option<OopGearSpawnConfig>> {
    let Some(runtime_cfg) = get_gear_runtime_config(config, gear_name)? else {
        return Ok(None);
    };

    if !matches!(runtime_cfg.mod_type, RuntimeKind::Oop) {
        return Ok(None);
    }

    let exec_cfg = runtime_cfg.execution.as_ref().ok_or_else(|| {
        anyhow::anyhow!("gear '{gear_name}' is type=oop but execution config is missing")
    })?;

    let binary = normalize_path(&exec_cfg.executable_path)?;
    let spawn_args = exec_cfg.args.clone();
    let env = exec_cfg.environment.clone();

    // Render the complete gear config (with resolved DB)
    let rendered_config = render_gear_config_for_oop(config, gear_name, home_dir)?;
    let rendered_json = rendered_config.to_json()?;

    tracing::debug!(
        gear =  %gear_name,
        "Prepared OoP gear config: db={}",
        rendered_config.database.is_some()
    );

    Ok(Some(OopGearSpawnConfig {
        gear_name: gear_name.to_owned(),
        binary,
        args: spawn_args,
        env,
        working_directory: exec_cfg.working_directory.clone(),
        rendered_config_json: rendered_json,
    }))
}

/// Initialize process-wide bootstrap state from a provided `&AppConfig`.
///
/// This helper performs the common startup sequence shared by server and migration modes.
/// It does **not** load configuration; the caller is responsible for building and passing
/// a valid `AppConfig`.
///
/// Steps performed:
///
/// - initializes tracing/logging (once, guarded by a process-wide `Once`) and metrics
///   when OpenTelemetry is enabled
/// - registers the panic hook used to route panics through tracing
/// - emits a small startup span and version metadata for diagnostics
///
/// The rustls crypto provider is installed inside this function (idempotent
/// `OnceLock`), so callers do not need to invoke
/// [`super::init_crypto_provider`] separately. Direct callers of
/// [`super::init_crypto_provider`] outside the bootstrap path (e.g. ad-hoc
/// probe binaries that do not call `run_server` / `run_migrate`) are still
/// supported.
///
/// # Errors
///
/// Returns an error if OpenTelemetry tracing initialization fails while
/// tracing is enabled, or if the crypto provider installation fails.
#[cfg_attr(not(feature = "otel"), allow(clippy::unnecessary_wraps))]
pub fn init_procedure(config: &AppConfig) -> Result<()> {
    // Install the rustls crypto provider FIRST — before anything that may
    // touch TLS (OTLP exporter over HTTPS, DB connections, etc.). The call
    // is process-wide and idempotent; calling it twice is safe.
    super::init_crypto_provider()?;

    // Build OpenTelemetry layer before logging
    #[cfg(feature = "otel")]
    let otel_layer = if config.opentelemetry.tracing.enabled {
        Some(crate::telemetry::init::init_tracing(&config.opentelemetry)?)
    } else {
        None
    };
    #[cfg(not(feature = "otel"))]
    let otel_layer = None;

    // Initialize logging + otel in one Registry
    init_logging_unified(
        &config.logging,
        &config.server.home_dir,
        otel_layer,
        config.opentelemetry.inject_trace_ids_into_logs(),
    );

    // Register custom panic hook to reroute panic backtrace into tracing.
    init_panic_tracing();

    // Initialize OpenTelemetry metrics (or confirm noop when disabled)
    #[cfg(feature = "otel")]
    if let Err(e) = crate::telemetry::init::init_metrics_provider(&config.opentelemetry) {
        tracing::error!(error = %e, "OpenTelemetry metrics not initialized");
    }

    // One-time connectivity probe
    #[cfg(feature = "otel")]
    if config.opentelemetry.tracing.enabled
        && let Err(e) = crate::telemetry::init::otel_connectivity_probe(&config.opentelemetry)
    {
        tracing::error!(error = %e, "OTLP connectivity probe failed");
    }

    // Smoke test span to confirm traces flow to Jaeger
    tracing::info_span!("startup_check", app = config.server.name).in_scope(|| {
        tracing::info!("startup span alive - traces should be visible in Jaeger");
    });

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        rust_version = env!("CARGO_PKG_RUST_VERSION"),
        "{} Server starting",
        config.server.name,
    );

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod publish_gts_tests {
    use super::*;
    use crate::gts_publication::deployment::{GtsDeploymentOutcome, GtsDeploymentReport};

    #[test]
    fn deployment_rejects_publication_options_on_a_nonpublishing_gear() {
        struct PlainGear;
        #[async_trait::async_trait]
        impl crate::Gear for PlainGear {
            async fn init(&self, _ctx: &crate::GearCtx) -> Result<()> {
                panic!("deployment validation must not initialize gears")
            }
        }

        let mut builder = crate::registry::RegistryBuilder::default();
        builder.register_core_with_meta("plain-gear", &[], Arc::new(PlainGear));
        let registry = builder.build_topo_sorted().unwrap();
        let mut config = AppConfig::default();
        validate_deployment_configuration(&registry, &config).unwrap();
        config.gears.insert(
            "plain-gear".to_owned(),
            serde_json::json!({"config": {"gts_publication": {"timeout": "1s"}}}),
        );
        let error = validate_deployment_configuration(&registry, &config).unwrap_err();
        assert!(error.to_string().contains("plain-gear"), "{error}");
        assert!(error.to_string().contains("gts_publication"), "{error}");
    }

    /// `publish-gts --timeout` above 10 minutes is refused first, before the
    /// registry, the credential or the network; no clamping.
    #[tokio::test]
    async fn publish_gts_rejects_a_deadline_above_ten_minutes_first() {
        let over =
            crate::gts_publication::MAX_PUBLICATION_BUDGET + std::time::Duration::from_secs(1);
        let err = publish_gts(
            &AppConfig::default(),
            &PublishGtsOptions {
                registry_endpoint: "https://registry.example:8443".to_owned(),
                deadline: over,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("--timeout")
                && message.contains("too large to be a publication budget"),
            "{message}"
        );
    }

    #[test]
    fn publish_gts_needs_the_platform_credential() {
        let err = platform_credential(&AppConfig::default()).unwrap_err();
        assert!(err.to_string().contains("oop_http.internal_auth"), "{err}");
    }

    #[test]
    fn a_failed_publisher_fails_the_job_with_the_report() {
        let failed = GtsDeploymentReport {
            publishers: vec![
                ("ok", GtsDeploymentOutcome::Published { documents: 1 }),
                (
                    "late",
                    GtsDeploymentOutcome::DeadlineExceeded {
                        unconfirmed: vec![
                            toolkit_gts::gts_id!("cf.test.publication.late.v1~").to_owned(),
                        ],
                    },
                ),
            ],
        };
        let err = publication_result(&failed).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("late: deadline exceeded")
                && message.contains(toolkit_gts::gts_id!("cf.test.publication.late.v1~")),
            "{message}"
        );

        let succeeded = GtsDeploymentReport {
            publishers: vec![("ok", GtsDeploymentOutcome::Published { documents: 1 })],
        };
        publication_result(&succeeded).expect("every publisher succeeded");
    }
}
