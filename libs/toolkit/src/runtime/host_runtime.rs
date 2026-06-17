//! Host Runtime - orchestrates the full `ToolKit` lifecycle
//!
//! This gear contains the `HostRuntime` type that owns and coordinates
//! the execution of all lifecycle phases.
//!
//! High-level phase order:
//! - `pre_init` (system gears only)
//! - DB migrations (gears with DB capability)
//! - `init` (all gears)
//! - `post_init` (system gears only; runs after *all* `init` complete)
//! - REST wiring (gears with REST capability; requires a single REST host)
//! - gRPC registration (gears with gRPC capability; requires a single gRPC hub)
//! - start/stop (stateful gears)
//! - `OoP` spawn / wait / stop (host-only orchestration)

use axum::Router;
use std::collections::HashSet;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::backends::OopSpawnConfig;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::context::GearContextBuilder;
use crate::registry::{
    ApiGatewayCap, GearEntry, GearRegistry, GrpcHubCap, RegistryError, RestApiCap, RunnableCap,
    SystemCap,
};
use crate::runtime::{GearManager, GrpcInstallerStore, OopSpawnOptions, SystemContext};

#[cfg(feature = "db")]
use crate::registry::DatabaseCap;

/// How the runtime should provide DBs to gears.
#[derive(Clone)]
pub enum DbOptions {
    /// No database integration. `GearCtx::db()` will be `None`, `db_required()` will error.
    None,
    /// Use a `DbManager` to handle database connections with Figment-based configuration.
    #[cfg(feature = "db")]
    Manager(Arc<toolkit_db::DbManager>),
}

/// Runtime execution mode that determines which phases to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Run all phases and wait for shutdown signal (normal application mode).
    Full,
    /// Run only pre-init and DB migration phases, then exit (for cloud deployments).
    MigrateOnly,
}

/// Environment variable name for passing directory endpoint to `OoP` gears.
pub const TOOLKIT_DIRECTORY_ENDPOINT_ENV: &str = "TOOLKIT_DIRECTORY_ENDPOINT";

/// Environment variable name for passing rendered gear config to `OoP` gears.
pub const TOOLKIT_MODULE_CONFIG_ENV: &str = "TOOLKIT_MODULE_CONFIG";

/// Default shutdown deadline for graceful gear stop (35 seconds).
///
/// This is intentionally 5 seconds longer than `WithLifecycle::stop_timeout` (30s default)
/// to ensure deterministic behavior: the lifecycle's internal timeout fires first,
/// and the runtime deadline acts as a hard backstop.
pub const DEFAULT_SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(35);

/// `HostRuntime` owns the lifecycle orchestration for `ToolKit`.
///
/// It encapsulates all runtime state and drives gears through the full lifecycle (see gear docs).
pub struct HostRuntime {
    registry: GearRegistry,
    ctx_builder: GearContextBuilder,
    instance_id: Uuid,
    gear_manager: Arc<GearManager>,
    grpc_installers: Arc<GrpcInstallerStore>,
    client_hub: Arc<ClientHub>,
    /// Process-level readiness, published in `client_hub` for the `/readyz`
    /// probe and updated by the proxy-wiring readiness loop + draining watcher.
    readiness: Arc<crate::ReadinessState>,
    cancel: CancellationToken,
    #[allow(dead_code)]
    db_options: DbOptions,
    /// `OoP` gear spawn configuration and backend
    oop_options: Option<OopSpawnOptions>,
    /// Maximum time allowed for graceful shutdown before hard-stop signal is sent.
    shutdown_deadline: std::time::Duration,
}

impl HostRuntime {
    /// Create a new `HostRuntime` instance.
    ///
    /// This prepares all runtime components but does not start any lifecycle phases.
    pub fn new(
        registry: GearRegistry,
        gears_cfg: Arc<dyn ConfigProvider>,
        db_options: DbOptions,
        client_hub: Arc<ClientHub>,
        cancel: CancellationToken,
        instance_id: Uuid,
        oop_options: Option<OopSpawnOptions>,
    ) -> Self {
        // Create runtime-owned components for system gears
        let gear_manager = Arc::new(GearManager::new());
        let grpc_installers = Arc::new(GrpcInstallerStore::new());

        // Process-level readiness, published so the gateway's /readyz handler
        // can fetch it (concrete-type key). Created before any phase runs.
        let readiness = Arc::new(crate::ReadinessState::new());
        client_hub.register::<crate::ReadinessState>(readiness.clone());

        // Build the context builder that will resolve per-gear DbHandles
        let ctx_builder =
            GearContextBuilder::new(instance_id, gears_cfg, client_hub.clone(), cancel.clone());
        #[cfg(feature = "db")]
        let ctx_builder = match &db_options {
            DbOptions::Manager(mgr) => ctx_builder.with_db_manager(mgr.clone()),
            DbOptions::None => ctx_builder,
        };

        Self {
            registry,
            ctx_builder,
            instance_id,
            gear_manager,
            grpc_installers,
            client_hub,
            readiness,
            cancel,
            db_options,
            oop_options,
            shutdown_deadline: DEFAULT_SHUTDOWN_DEADLINE,
        }
    }

    /// Set a custom shutdown deadline for graceful gear stop.
    ///
    /// This is the maximum time the runtime will wait for each gear to stop gracefully
    /// before sending the hard-stop signal (cancelling the deadline token).
    ///
    /// # Relationship with `WithLifecycle::stop_timeout`
    ///
    /// When using `WithLifecycle`, its `stop_timeout` (default 30s) races against this
    /// `shutdown_deadline` (also default 30s). To ensure deterministic behavior:
    ///
    /// - `WithLifecycle::stop_timeout` should be **less than** `shutdown_deadline`
    /// - This allows the lifecycle's internal timeout to trigger first for graceful cleanup
    /// - The runtime's `deadline_token` then acts as a hard backstop
    ///
    /// Example: `stop_timeout = 25s`, `shutdown_deadline = 30s`
    #[must_use]
    pub fn with_shutdown_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.shutdown_deadline = deadline;
        self
    }

    /// `PRE_INIT` phase: wire runtime internals into system gears.
    ///
    /// This phase runs before init and only for gears with the "system" capability.
    ///
    /// # Errors
    /// Returns `RegistryError` if system wiring fails.
    pub fn run_pre_init_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: pre_init");

        let sys_ctx = SystemContext::new(
            self.instance_id,
            Arc::clone(&self.gear_manager),
            Arc::clone(&self.grpc_installers),
        );

        for entry in self.registry.gears() {
            // Check for cancellation before processing each gear
            if self.cancel.is_cancelled() {
                tracing::warn!("Pre-init phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }

            if let Some(sys_mod) = entry.caps.query::<SystemCap>() {
                tracing::debug!(gear = entry.name, "Running system pre_init");
                sys_mod
                    .pre_init(&sys_ctx)
                    .map_err(|e| RegistryError::PreInit {
                        gear: entry.name,
                        source: e,
                    })?;
            }
        }

        Ok(())
    }

    /// Helper: resolve context for a gear with error mapping.
    #[cfg(feature = "db")]
    async fn gear_context(
        &self,
        gear_name: &'static str,
    ) -> Result<crate::context::GearCtx, RegistryError> {
        self.ctx_builder
            .for_gear(gear_name)
            .await
            .map_err(|e| RegistryError::DbMigrate {
                gear: gear_name,
                source: e,
            })
    }

    /// Helper: extract DB handle and gear if both exist.
    #[cfg(feature = "db")]
    async fn db_migration_target(
        &self,
        gear_name: &'static str,
        ctx: &crate::context::GearCtx,
        db_gear: Option<Arc<dyn crate::contracts::DatabaseCapability>>,
    ) -> Result<
        Option<(
            toolkit_db::Db,
            Arc<dyn crate::contracts::DatabaseCapability>,
        )>,
        RegistryError,
    > {
        let Some(dbm) = db_gear else {
            return Ok(None);
        };

        // Important: DB migrations require access to the underlying `Db`, not just `DBProvider`.
        // `GearCtx` intentionally exposes only `DBProvider` for better DX and to reduce mistakes.
        // So the runtime resolves the `Db` directly from its `DbManager`.
        let db = match &self.db_options {
            DbOptions::None => None,
            #[cfg(feature = "db")]
            DbOptions::Manager(mgr) => {
                mgr.get(gear_name)
                    .await
                    .map_err(|e| RegistryError::DbMigrate {
                        gear: gear_name,
                        source: e.into(),
                    })?
            }
        };

        _ = ctx; // ctx is kept for parity/error context; DB is resolved from manager above.
        Ok(db.map(|db| (db, dbm)))
    }

    /// Helper: run migrations for a single gear using the new migration runner.
    ///
    /// This collects migrations from the gear and executes them via the
    /// runtime's privileged connection. Gears never see the raw connection.
    #[cfg(feature = "db")]
    async fn migrate_gear(
        gear_name: &'static str,
        db: &toolkit_db::Db,
        db_gear: Arc<dyn crate::contracts::DatabaseCapability>,
    ) -> Result<(), RegistryError> {
        // Collect migrations from the gear
        let migrations = db_gear.migrations();

        if migrations.is_empty() {
            tracing::debug!(gear = gear_name, "No migrations to run");
            return Ok(());
        }

        tracing::debug!(
            gear = gear_name,
            count = migrations.len(),
            "Running DB migrations"
        );

        // Execute migrations using the migration runner
        let result =
            toolkit_db::migration_runner::run_migrations_for_gear(db, gear_name, migrations)
                .await
                .map_err(|e| RegistryError::DbMigrate {
                    gear: gear_name,
                    source: anyhow::Error::new(e),
                })?;

        tracing::info!(
            gear = gear_name,
            applied = result.applied,
            skipped = result.skipped,
            "DB migrations completed"
        );

        Ok(())
    }

    /// DB MIGRATION phase: run migrations for all gears with DB capability.
    ///
    /// Runs before init, with system gears processed first.
    ///
    /// Gears provide migrations via `DatabaseCapability::migrations()`.
    /// The runtime executes them with a privileged connection that gears
    /// never receive directly. Each gear gets a separate migration history
    /// table, preventing cross-gear interference.
    #[cfg(feature = "db")]
    async fn run_db_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: db (before init)");

        for entry in self.registry.gears_by_system_priority() {
            // Check for cancellation before processing each gear
            if self.cancel.is_cancelled() {
                tracing::warn!("DB migration phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }

            let ctx = self.gear_context(entry.name).await?;
            let db_gear = entry.caps.query::<DatabaseCap>();

            match self
                .db_migration_target(entry.name, &ctx, db_gear.clone())
                .await?
            {
                Some((db, dbm)) => {
                    Self::migrate_gear(entry.name, &db, dbm).await?;
                }
                None if db_gear.is_some() => {
                    tracing::debug!(
                        gear = entry.name,
                        "Gear has DbGear trait but no DB handle (no config)"
                    );
                }
                None => {}
            }
        }

        Ok(())
    }

    /// INIT phase: initialize all gears in topological order.
    ///
    /// System gears initialize first, followed by user gears.
    async fn run_init_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: init");

        for entry in self.registry.gears_by_system_priority() {
            let ctx =
                self.ctx_builder
                    .for_gear(entry.name)
                    .await
                    .map_err(|e| RegistryError::Init {
                        gear: entry.name,
                        source: e,
                    })?;
            tracing::info!(gear = entry.name, "Initializing a gear...");
            entry
                .core
                .init(&ctx)
                .await
                .map_err(|e| RegistryError::Init {
                    gear: entry.name,
                    source: e,
                })?;
            tracing::info!(gear = entry.name, "Initialized a gear.");
        }

        Ok(())
    }

    /// `POST_INIT` phase: optional hook after ALL gears completed `init()`.
    ///
    /// Consumer proxy-wiring phase (eventual readiness).
    ///
    /// Runs after init (compile-time / local registrations) and before
    /// post-init. Replays each `ConsumerRegistration` emitted by
    /// `#[toolkit::consumes]`: if a compile-time impl is already in the
    /// `ClientHub` it wins (the wiring closure short-circuits); otherwise a
    /// directory-resolving REST client is registered under the contract trait.
    ///
    /// Non-blocking: endpoint discovery is lazy/per-call inside the resolving
    /// client, so this phase never waits on provider availability (ADR-0007).
    /// A no-op when no consumer is registered, preserving the phase-order
    /// invariants relied on by existing tests.
    #[cfg(feature = "contract-directory-rest-client")]
    async fn run_proxy_wiring_phase(&self) -> Result<(), RegistryError> {
        use crate::discovery::{ConsumerRegistration, DirectoryEndpointResolver};
        use toolkit_contract::runtime::resolving::EndpointResolver;

        let regs: Vec<&ConsumerRegistration> =
            inventory::iter::<ConsumerRegistration>.into_iter().collect();
        if regs.is_empty() {
            return Ok(());
        }
        tracing::info!(count = regs.len(), "Phase: proxy-wiring (consumer discovery)");

        let Ok(dir) = self.client_hub.get::<dyn crate::DirectoryClient>() else {
            tracing::warn!(
                consumers = regs.len(),
                "proxy-wiring: no DirectoryClient in ClientHub; skipping consumer discovery wiring"
            );
            return Ok(());
        };
        let resolver: Arc<dyn EndpointResolver> = Arc::new(DirectoryEndpointResolver(dir));

        for reg in &regs {
            (reg.wire)(&self.client_hub, Arc::clone(&resolver)).map_err(|source| {
                RegistryError::ProxyWiring {
                    gear: reg.owner_gear,
                    source,
                }
            })?;
            tracing::debug!(owner = reg.owner_gear, dep = reg.dep_gear, "wired consumer contract");
        }

        // Register consumed deps as readiness gates and spawn a background probe
        // loop that flips each to resolved once the directory can resolve it.
        // Readiness is startup-gating + sticky (ADR-0007): /readyz reports
        // Starting until every consumed dependency resolves, then Ready.
        let deps: Vec<String> = regs.iter().map(|r| r.dep_gear.to_owned()).collect();
        for dep in &deps {
            self.readiness.register_dep(dep.clone());
        }
        let readiness = Arc::clone(&self.readiness);
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            const BASE: std::time::Duration = std::time::Duration::from_millis(100);
            const MAX: std::time::Duration = std::time::Duration::from_secs(30);
            let mut pending = deps;
            let mut backoff = BASE;
            while !pending.is_empty() {
                let mut still_pending = Vec::new();
                for dep in pending {
                    match resolver.resolve_endpoint(&dep).await {
                        Ok(Some(_)) => {
                            readiness.mark_resolved(&dep);
                            tracing::info!(dep = %dep, "readiness: dependency resolved");
                        }
                        // No live instance yet — expected during startup churn.
                        Ok(None) => still_pending.push(dep),
                        // Genuine directory-backend failure: surface it (a stuck
                        // Starting / 503 pod otherwise has no diagnostic trail).
                        Err(e) => {
                            tracing::warn!(dep = %dep, error = %e, "readiness: directory lookup failed");
                            still_pending.push(dep);
                        }
                    }
                }
                pending = still_pending;
                if pending.is_empty() {
                    break;
                }
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(MAX);
            }
        });

        Ok(())
    }

    /// This provides a global barrier between initialization-time registration
    /// and subsequent phases that may rely on a fully-populated runtime registry.
    ///
    /// System gears run first, followed by user gears, preserving topo order.
    async fn run_post_init_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: post_init");

        let sys_ctx = SystemContext::new(
            self.instance_id,
            Arc::clone(&self.gear_manager),
            Arc::clone(&self.grpc_installers),
        );

        for entry in self.registry.gears_by_system_priority() {
            if let Some(sys_mod) = entry.caps.query::<SystemCap>() {
                sys_mod
                    .post_init(&sys_ctx)
                    .await
                    .map_err(|e| RegistryError::PostInit {
                        gear: entry.name,
                        source: e,
                    })?;
            }
        }

        Ok(())
    }

    /// REST phase: compose the router against the REST host.
    ///
    /// This is a synchronous phase that builds the final Router by:
    /// 1. Preparing the host gear
    /// 2. Registering all REST providers
    /// 3. Finalizing with `OpenAPI` endpoints
    async fn run_rest_phase(&self) -> Result<Router, RegistryError> {
        tracing::info!("Phase: rest (sync)");

        let mut router = Router::new();

        // Find host(s) and whether any rest gears exist
        let host_count = self
            .registry
            .gears()
            .iter()
            .filter(|e| e.caps.has::<ApiGatewayCap>())
            .count();

        match host_count {
            0 => {
                return if self
                    .registry
                    .gears()
                    .iter()
                    .any(|e| e.caps.has::<RestApiCap>())
                {
                    Err(RegistryError::RestRequiresHost)
                } else {
                    Ok(router)
                };
            }
            1 => { /* proceed */ }
            _ => return Err(RegistryError::MultipleRestHosts),
        }

        // Resolve the single host entry and its gear context
        let host_idx = self
            .registry
            .gears()
            .iter()
            .position(|e| e.caps.has::<ApiGatewayCap>())
            .ok_or(RegistryError::RestHostNotFoundAfterValidation)?;
        let host_entry = &self.registry.gears()[host_idx];
        let Some(host) = host_entry.caps.query::<ApiGatewayCap>() else {
            return Err(RegistryError::RestHostMissingFromEntry);
        };
        let host_ctx = self
            .ctx_builder
            .for_gear(host_entry.name)
            .await
            .map_err(|e| RegistryError::RestPrepare {
                gear: host_entry.name,
                source: e,
            })?;

        // use host as the registry
        let registry: &dyn crate::contracts::OpenApiRegistry = host.as_registry();

        // 1) Host prepare: base Router / global middlewares / basic OAS meta
        router =
            host.rest_prepare(&host_ctx, router)
                .map_err(|source| RegistryError::RestPrepare {
                    gear: host_entry.name,
                    source,
                })?;

        // 2) Register all REST providers (in the current discovery order)
        for e in self.registry.gears() {
            if let Some(rest) = e.caps.query::<RestApiCap>() {
                let ctx = self.ctx_builder.for_gear(e.name).await.map_err(|err| {
                    RegistryError::RestRegister {
                        gear: e.name,
                        source: err,
                    }
                })?;

                router = rest
                    .register_rest(&ctx, router, registry)
                    .map_err(|source| RegistryError::RestRegister {
                        gear: e.name,
                        source,
                    })?;
            }
        }

        // 3) Host finalize: attach /openapi.json and /docs, persist Router if needed (no server start)
        router = host.rest_finalize(&host_ctx, router).map_err(|source| {
            RegistryError::RestFinalize {
                gear: host_entry.name,
                source,
            }
        })?;

        Ok(router)
    }

    /// gRPC registration phase: collect services from all grpc gears.
    ///
    /// Services are stored in the installer store for the `grpc-hub` to consume during start.
    async fn run_grpc_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: grpc (registration)");

        // If no grpc_hub and no grpc_services, skip the phase
        if self.registry.grpc_hub.is_none() && self.registry.grpc_services.is_empty() {
            return Ok(());
        }

        // If there are grpc_services but no hub, that's an error
        if self.registry.grpc_hub.is_none() && !self.registry.grpc_services.is_empty() {
            return Err(RegistryError::GrpcRequiresHub);
        }

        // If there's a hub, collect all services grouped by gear and hand them off to the installer store
        if let Some(hub_name) = &self.registry.grpc_hub {
            let mut gears_data = Vec::new();
            let mut seen = HashSet::new();

            // Collect services from all grpc gears
            for (gear_name, service_gear) in &self.registry.grpc_services {
                let ctx = self.ctx_builder.for_gear(gear_name).await.map_err(|err| {
                    RegistryError::GrpcRegister {
                        gear: gear_name.clone(),
                        source: err,
                    }
                })?;

                let installers = service_gear
                    .get_grpc_services(&ctx)
                    .await
                    .map_err(|source| RegistryError::GrpcRegister {
                        gear: gear_name.clone(),
                        source,
                    })?;

                for reg in &installers {
                    if !seen.insert(reg.service_name) {
                        return Err(RegistryError::GrpcRegister {
                            gear: gear_name.clone(),
                            source: anyhow::anyhow!(
                                "Duplicate gRPC service name: {}",
                                reg.service_name
                            ),
                        });
                    }
                }

                gears_data.push(crate::runtime::GearInstallers {
                    gear_name: gear_name.clone(),
                    installers,
                });
            }

            self.grpc_installers
                .set(crate::runtime::GrpcInstallerData { gears: gears_data })
                .map_err(|source| RegistryError::GrpcRegister {
                    gear: hub_name.clone(),
                    source,
                })?;
        }

        Ok(())
    }

    /// START phase: start all stateful gears.
    ///
    /// System gears start first, followed by user gears.
    async fn run_start_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: start");

        for e in self.registry.gears_by_system_priority() {
            if let Some(s) = e.caps.query::<RunnableCap>() {
                tracing::debug!(
                    gear = e.name,
                    is_system = e.caps.has::<SystemCap>(),
                    "Starting stateful gear"
                );
                s.start(self.cancel.clone())
                    .await
                    .map_err(|source| RegistryError::Start {
                        gear: e.name,
                        source,
                    })?;
                tracing::info!(gear = e.name, "Started gear");
            }
        }

        Ok(())
    }

    /// Stop a single gear, logging errors but continuing execution.
    async fn stop_one_gear(entry: &GearEntry, cancel: CancellationToken) {
        if let Some(s) = entry.caps.query::<RunnableCap>() {
            match s.stop(cancel).await {
                Err(err) => {
                    tracing::warn!(gear =  entry.name, error = %err, "Failed to stop gear");
                }
                _ => {
                    tracing::info!(gear = entry.name, "Stopped gear");
                }
            }
        }
    }

    /// STOP phase: stop all stateful gears in reverse order.
    ///
    /// # Two-Phase Shutdown Contract
    ///
    /// This phase implements a proper two-phase shutdown for **each gear**:
    ///
    /// 1. **Graceful stop request**: Each gear's `stop(deadline_token)` is called with a
    ///    *fresh* cancellation token (not the already-cancelled root token). Gears should
    ///    interpret this as "please stop gracefully".
    ///
    /// 2. **Hard-stop deadline**: After `shutdown_deadline` expires **for that gear**,
    ///    its `deadline_token` is cancelled. Gears should interpret this as "abort immediately".
    ///
    /// Each gear gets its own independent deadline — if gear A takes 25s to stop,
    /// gear B still gets the full `shutdown_deadline` for its graceful shutdown.
    ///
    /// This allows gears to implement real graceful shutdown:
    /// - Request cooperative shutdown of child tasks
    /// - Wait for them to finish gracefully
    /// - If `deadline_token` fires, switch to hard-abort mode
    ///
    /// Errors are logged but do not fail the shutdown process.
    /// Note: `OoP` gears are stopped automatically by the backend when the
    /// cancellation token is triggered.
    async fn run_stop_phase(&self) -> Result<(), RegistryError> {
        tracing::info!("Phase: stop");

        // Drop our REST providers from the directory first, so consumers stop
        // resolving an endpoint that is about to disappear.
        self.deregister_rest_providers().await;

        let deadline = self.shutdown_deadline;

        // Stop all gears in reverse order, each with its own independent deadline
        for e in self.registry.gears().iter().rev() {
            let gear_name = e.name;

            // Create a fresh deadline token for THIS gear
            // Each gear gets the full shutdown_deadline independently
            let deadline_token = CancellationToken::new();
            let deadline_token_for_timeout = deadline_token.clone();

            // Spawn a task to cancel this gear's deadline token after shutdown_deadline
            let deadline_task = tokio::spawn(async move {
                tokio::time::sleep(deadline).await;
                tracing::warn!(
                    gear = gear_name,
                    deadline_secs = deadline.as_secs(),
                    "Gear shutdown deadline reached, sending hard-stop signal"
                );
                deadline_token_for_timeout.cancel();
            });

            // Stop this gear with its own deadline token
            // The gear can observe the token transition from uncancelled→cancelled
            Self::stop_one_gear(e, deadline_token).await;

            // Cancel the deadline task and await it to ensure full cleanup
            deadline_task.abort();
            #[allow(clippy::let_underscore_must_use)]
            let _ = deadline_task.await;
        }

        Ok(())
    }

    /// `OoP` SPAWN phase: spawn out-of-process gears after start phase.
    ///
    /// This phase runs after `grpc-hub` is already listening, so we can pass
    /// the real directory endpoint to `OoP` gears.
    async fn run_oop_spawn_phase(&self) -> Result<(), RegistryError> {
        let oop_opts = match &self.oop_options {
            Some(opts) if !opts.gears.is_empty() => opts,
            _ => return Ok(()),
        };

        tracing::info!("Phase: oop_spawn");

        // Wait for grpc_hub to publish its endpoint (it runs async in start phase)
        let directory_endpoint = self.wait_for_grpc_hub_endpoint().await;

        for gear_cfg in &oop_opts.gears {
            // Build environment with directory endpoint and rendered config
            // Note: User controls --config via execution.args in master config
            let mut env = gear_cfg.env.clone();
            env.insert(
                TOOLKIT_MODULE_CONFIG_ENV.to_owned(),
                gear_cfg.rendered_config_json.clone(),
            );
            if let Some(ref endpoint) = directory_endpoint {
                env.insert(TOOLKIT_DIRECTORY_ENDPOINT_ENV.to_owned(), endpoint.clone());
            }

            // Use args from execution config as-is (user controls --config via args)
            let args = gear_cfg.args.clone();

            let spawn_config = OopSpawnConfig {
                gear_name: gear_cfg.gear_name.clone(),
                binary: gear_cfg.binary.clone(),
                args,
                env,
                working_directory: gear_cfg.working_directory.clone(),
            };

            oop_opts
                .backend
                .spawn(spawn_config)
                .await
                .map_err(|e| RegistryError::OopSpawn {
                    gear: gear_cfg.gear_name.clone(),
                    source: e,
                })?;

            tracing::info!(
                gear =  %gear_cfg.gear_name,
                directory_endpoint = ?directory_endpoint,
                "Spawned OoP gear via backend"
            );
        }

        Ok(())
    }

    /// Wait for `grpc-hub` to publish its bound endpoint.
    ///
    /// Polls the `GrpcHubGear::bound_endpoint()` with a short interval until available or timeout.
    /// Returns None if no `grpc-hub` is running or if it times out.
    async fn wait_for_grpc_hub_endpoint(&self) -> Option<String> {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

        // Find grpc_hub in registry
        let grpc_hub = self
            .registry
            .gears()
            .iter()
            .find_map(|e| e.caps.query::<GrpcHubCap>());

        let Some(hub) = grpc_hub else {
            return None; // No grpc_hub registered
        };

        let start = std::time::Instant::now();

        loop {
            if let Some(endpoint) = hub.bound_endpoint() {
                tracing::debug!(
                    endpoint = %endpoint,
                    elapsed_ms = start.elapsed().as_millis(),
                    "gRPC hub endpoint available"
                );
                return Some(endpoint);
            }

            if start.elapsed() > MAX_WAIT {
                tracing::warn!("Timed out waiting for gRPC hub to bind");
                return None;
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait for the REST host gateway to publish its bound endpoint.
    ///
    /// The gateway binds its listener asynchronously in the start phase, so the
    /// bound endpoint may not be set the instant the start phase returns; poll
    /// with a short interval until available or timeout.
    async fn wait_for_rest_endpoint(
        &self,
        host: &Arc<dyn crate::contracts::ApiGatewayCapability>,
    ) -> Option<String> {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

        let start = std::time::Instant::now();
        loop {
            if let Some(endpoint) = host.bound_endpoint() {
                return Some(endpoint);
            }
            if start.elapsed() > MAX_WAIT {
                tracing::warn!("Timed out waiting for REST host to bind");
                return None;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Directory-register phase (eventual readiness, provider side).
    ///
    /// After the REST server has bound, advertise every in-process REST provider
    /// gear in the service directory under its own gear name, pointing at the
    /// shared gateway endpoint. Consumers resolving a provider gear name then
    /// receive this endpoint and the gateway routes to the provider's handlers.
    ///
    /// No-op when there is no REST host or no REST provider gears, so non-REST
    /// deployments and existing tests are unaffected. Registers through the
    /// `DirectoryClient` in the `ClientHub`, which uniformly targets the
    /// in-process directory (`LocalDirectoryClient`) or the central directory
    /// (`DirectoryGrpcClient` for OoP) depending on what the host wired.
    async fn run_directory_register_phase(&self) -> Result<(), RegistryError> {
        let rest_gears = self.rest_provider_gears();
        if rest_gears.is_empty() {
            return Ok(());
        }

        let Some(host) = self
            .registry
            .gears()
            .iter()
            .find_map(|e| e.caps.query::<ApiGatewayCap>())
        else {
            return Ok(()); // no REST host serving the routes
        };

        let Some(endpoint) = self.wait_for_rest_endpoint(&host).await else {
            tracing::warn!(
                "directory-register: REST host endpoint unavailable; skipping REST provider registration"
            );
            return Ok(());
        };

        let Ok(dir) = self.client_hub.get::<dyn crate::DirectoryClient>() else {
            tracing::debug!(
                "directory-register: no DirectoryClient in ClientHub; skipping REST provider registration"
            );
            return Ok(());
        };

        let instance_id = self.instance_id.to_string();
        for gear in rest_gears {
            // The directory keys instances by (gear, instance_id) and replaces
            // wholesale. grpc-hub may have already registered this same
            // (gear, instance_id) with gRPC services during the start phase, so
            // carry those forward instead of clobbering them to empty — adding
            // the REST endpoint must augment, not replace, the entry.
            let (grpc_services, version) = match dir.list_instances(gear).await {
                Ok(insts) => insts
                    .into_iter()
                    .find(|i| i.instance_id == instance_id)
                    .map(|i| (i.grpc_services, i.version))
                    .unwrap_or_default(),
                Err(_) => (Vec::new(), None),
            };
            let info = crate::RegisterInstanceInfo {
                gear: gear.to_owned(),
                instance_id: instance_id.clone(),
                grpc_services,
                version,
                rest_endpoint: Some(crate::ServiceEndpoint::new(endpoint.clone())),
            };
            match dir.register_instance(info).await {
                Ok(()) => {
                    tracing::info!(gear, endpoint = %endpoint, "registered REST provider in directory");
                }
                Err(e) => {
                    tracing::warn!(gear, error = %e, "directory-register: failed to register REST provider");
                }
            }
        }
        Ok(())
    }

    /// Names of all gears that provide a REST API (have `RestApiCap`), excluding
    /// the REST host gateway itself (`ApiGatewayCap`) — the gateway is the
    /// transport, not a contract provider, so it must not be advertised in the
    /// directory under its own gear name.
    fn rest_provider_gears(&self) -> Vec<&'static str> {
        self.registry
            .gears()
            .iter()
            .filter(|e| e.caps.has::<RestApiCap>() && !e.caps.has::<ApiGatewayCap>())
            .map(|e| e.name)
            .collect()
    }

    /// Deregister this process's REST providers from the directory on shutdown,
    /// so consumers stop resolving an endpoint that is going away. Best-effort.
    async fn deregister_rest_providers(&self) {
        let rest_gears = self.rest_provider_gears();
        if rest_gears.is_empty() {
            return;
        }
        let Ok(dir) = self.client_hub.get::<dyn crate::DirectoryClient>() else {
            return;
        };
        let instance_id = self.instance_id.to_string();
        for gear in rest_gears {
            if let Err(e) = dir.deregister_instance(gear, &instance_id).await {
                tracing::warn!(gear, error = %e, "directory-deregister: failed to deregister REST provider");
            }
        }
    }

    /// Run the full gear lifecycle (all phases).
    ///
    /// This is the standard entry point for normal application execution.
    /// It runs all phases from pre-init through shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if any gear phase fails during execution.
    pub async fn run_gear_phases(self) -> anyhow::Result<()> {
        self.run_phases_internal(RunMode::Full).await
    }

    /// Run only the migration phases (pre-init + DB migration).
    ///
    /// This is designed for cloud deployment workflows where database migrations
    /// need to run as a separate step before starting the application.
    /// The process exits after migrations complete.
    ///
    /// # Errors
    ///
    /// Returns an error if pre-init or migration phases fail.
    pub async fn run_migration_phases(self) -> anyhow::Result<()> {
        self.run_phases_internal(RunMode::MigrateOnly).await
    }

    /// Internal implementation that runs gear phases based on the mode.
    ///
    /// This private method contains the actual phase execution logic and is called
    /// by both `run_gear_phases()` and `run_migration_phases()`.
    ///
    /// # Modes
    ///
    /// - `RunMode::Full`: Executes all phases and waits for shutdown signal
    /// - `RunMode::MigrateOnly`: Executes only pre-init and DB migration phases, then exits
    ///
    /// # Phases (Full Mode)
    ///
    /// 1. Pre-init (system gears only)
    /// 2. DB migration (all gears with database capability)
    /// 3. Init (all gears)
    /// 4. Post-init (system gears only)
    /// 5. REST (gears with REST capability)
    /// 6. gRPC (gears with gRPC capability)
    /// 7. Start (runnable gears)
    /// 8. `OoP` spawn (out-of-process gears)
    /// 9. Wait for cancellation
    /// 10. Stop (runnable gears in reverse order)
    async fn run_phases_internal(self, mode: RunMode) -> anyhow::Result<()> {
        // Log execution mode
        match mode {
            RunMode::Full => {
                tracing::info!("Running full lifecycle (all phases)");
            }
            RunMode::MigrateOnly => {
                tracing::info!("Running in migration mode (pre-init + db phases only)");
            }
        }

        // 1. Pre-init phase (before init, only for system gears)
        self.run_pre_init_phase()?;

        // 2. DB migration phase (system gears first)
        #[cfg(feature = "db")]
        {
            self.run_db_phase().await?;
        }
        #[cfg(not(feature = "db"))]
        {
            // No DB integration in this build.
        }

        // Exit early if running in migration-only mode
        if mode == RunMode::MigrateOnly {
            tracing::info!("Migration phases completed successfully");
            return Ok(());
        }

        // 3. Init phase (system gears first)
        self.run_init_phase().await?;

        // 3b. Proxy-wiring phase (consumer discovery; after init, before post-init)
        #[cfg(feature = "contract-directory-rest-client")]
        self.run_proxy_wiring_phase().await?;

        // 4. Post-init phase (barrier after ALL init; system gears only)
        self.run_post_init_phase().await?;

        // 5. REST phase (synchronous router composition)
        let _router = self.run_rest_phase().await?;

        // 6. gRPC registration phase
        self.run_grpc_phase().await?;

        // 7. Start phase
        self.run_start_phase().await?;

        // Draining watcher: flip readiness to Draining the moment shutdown
        // begins so /readyz reports 503 and the orchestrator drains the pod out
        // of the load balancer before the stop phase tears gears down.
        {
            let readiness = Arc::clone(&self.readiness);
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                readiness.set_draining();
            });
        }

        // 7b. Directory-register phase: advertise in-process REST providers in
        //     the directory once the gateway has bound its listener.
        self.run_directory_register_phase().await?;

        // 8. OoP spawn phase (after grpc_hub is running)
        self.run_oop_spawn_phase().await?;

        // 9. Wait for cancellation
        self.cancel.cancelled().await;

        // 10. Stop phase with hard timeout.
        //     Blocking syscalls (e.g. libc getaddrinfo in tokio spawn_blocking)
        //     can saturate all tokio worker threads, preventing tokio timers
        //     from firing. Use an OS thread so the watchdog works even when
        //     the tokio runtime is fully blocked.
        let stop_timeout = std::time::Duration::from_secs(15);
        let disarm = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let disarm_clone = std::sync::Arc::clone(&disarm);
        std::thread::spawn(move || {
            std::thread::sleep(stop_timeout);
            if !disarm_clone.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!(
                    timeout_secs = stop_timeout.as_secs(),
                    "shutdown: stop phase timed out, force exiting"
                );
                std::process::exit(1);
            }
        });

        self.run_stop_phase().await?;
        disarm.store(true, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::context::GearCtx;
    use crate::contracts::{Gear, RunnableCapability, SystemCapability};
    use crate::registry::RegistryBuilder;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[derive(Default)]
    #[allow(dead_code)]
    struct DummyCore;
    #[async_trait::async_trait]
    impl Gear for DummyCore {
        async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StopOrderTracker {
        my_order: usize,
        stop_order: Arc<AtomicUsize>,
    }

    impl StopOrderTracker {
        fn new(counter: &Arc<AtomicUsize>, stop_order: Arc<AtomicUsize>) -> Self {
            let my_order = counter.fetch_add(1, Ordering::SeqCst);
            Self {
                my_order,
                stop_order,
            }
        }
    }

    #[async_trait::async_trait]
    impl Gear for StopOrderTracker {
        async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl RunnableCapability for StopOrderTracker {
        async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
            let order = self.stop_order.fetch_add(1, Ordering::SeqCst);
            tracing::info!(my_order = self.my_order, stop_order = order, "Gear stopped");
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_stop_phase_reverse_order() {
        let counter = Arc::new(AtomicUsize::new(0));
        let stop_order = Arc::new(AtomicUsize::new(0));

        let gear_a = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));
        let gear_b = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));
        let gear_c = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("a", &[], gear_a.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("b", &["a"], gear_b.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("c", &["b"], gear_c.clone() as Arc<dyn Gear>);

        builder.register_stateful_with_meta("a", gear_a.clone() as Arc<dyn RunnableCapability>);
        builder.register_stateful_with_meta("b", gear_b.clone() as Arc<dyn RunnableCapability>);
        builder.register_stateful_with_meta("c", gear_c.clone() as Arc<dyn RunnableCapability>);

        let registry = builder.build_topo_sorted().unwrap();

        // Verify gear order is a -> b -> c
        let gear_names: Vec<_> = registry.gears().iter().map(|m| m.name).collect();
        assert_eq!(gear_names, vec!["a", "b", "c"]);

        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel.clone(),
            Uuid::new_v4(),
            None,
        );

        // Run stop phase
        runtime.run_stop_phase().await.unwrap();

        // Verify gears stopped in reverse order: c (stop_order=0), b (stop_order=1), a (stop_order=2)
        // Gear order is: a=0, b=1, c=2
        // Stop order should be: c=0, b=1, a=2
        assert_eq!(stop_order.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_stop_phase_continues_on_error() {
        struct FailingGear {
            should_fail: bool,
            stopped: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl Gear for FailingGear {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl RunnableCapability for FailingGear {
            async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
                Ok(())
            }
            async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
                self.stopped.fetch_add(1, Ordering::SeqCst);
                if self.should_fail {
                    anyhow::bail!("Intentional failure")
                }
                Ok(())
            }
        }

        let stopped = Arc::new(AtomicUsize::new(0));
        let gear_a = Arc::new(FailingGear {
            should_fail: false,
            stopped: stopped.clone(),
        });
        let gear_b = Arc::new(FailingGear {
            should_fail: true,
            stopped: stopped.clone(),
        });
        let gear_c = Arc::new(FailingGear {
            should_fail: false,
            stopped: stopped.clone(),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("a", &[], gear_a.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("b", &["a"], gear_b.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("c", &["b"], gear_c.clone() as Arc<dyn Gear>);

        builder.register_stateful_with_meta("a", gear_a.clone() as Arc<dyn RunnableCapability>);
        builder.register_stateful_with_meta("b", gear_b.clone() as Arc<dyn RunnableCapability>);
        builder.register_stateful_with_meta("c", gear_c.clone() as Arc<dyn RunnableCapability>);

        let registry = builder.build_topo_sorted().unwrap();

        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel.clone(),
            Uuid::new_v4(),
            None,
        );

        // Run stop phase - should not fail even though gear_b fails
        runtime.run_stop_phase().await.unwrap();

        // All gears should have attempted to stop
        assert_eq!(stopped.load(Ordering::SeqCst), 3);
    }

    struct EmptyConfigProvider;
    impl ConfigProvider for EmptyConfigProvider {
        fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
            None
        }
    }

    #[tokio::test]
    async fn test_post_init_runs_after_all_init_and_system_first() {
        #[derive(Clone)]
        struct TrackHooks {
            name: &'static str,
            events: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl Gear for TrackHooks {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                self.events.lock().await.push(format!("init:{}", self.name));
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl SystemCapability for TrackHooks {
            fn pre_init(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
                Ok(())
            }

            async fn post_init(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
                self.events
                    .lock()
                    .await
                    .push(format!("post_init:{}", self.name));
                Ok(())
            }
        }

        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let sys_a = Arc::new(TrackHooks {
            name: "sys_a",
            events: events.clone(),
        });
        let user_b = Arc::new(TrackHooks {
            name: "user_b",
            events: events.clone(),
        });
        let user_c = Arc::new(TrackHooks {
            name: "user_c",
            events: events.clone(),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("sys_a", &[], sys_a.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("user_b", &["sys_a"], user_b.clone() as Arc<dyn Gear>);
        builder.register_core_with_meta("user_c", &["user_b"], user_c.clone() as Arc<dyn Gear>);
        builder.register_system_with_meta("sys_a", sys_a.clone() as Arc<dyn SystemCapability>);

        let registry = builder.build_topo_sorted().unwrap();

        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel,
            Uuid::new_v4(),
            None,
        );

        // Run init phase for all gears, then post_init as a separate barrier phase.
        runtime.run_init_phase().await.unwrap();
        runtime.run_post_init_phase().await.unwrap();

        let events = events.lock().await.clone();
        let first_post_init = events
            .iter()
            .position(|e| e.starts_with("post_init:"))
            .expect("expected post_init events");
        assert!(
            events[..first_post_init]
                .iter()
                .all(|e| e.starts_with("init:")),
            "expected all init events before post_init, got: {events:?}"
        );

        // system-first order within each phase
        assert_eq!(
            events,
            vec![
                "init:sys_a",
                "init:user_b",
                "init:user_c",
                "post_init:sys_a",
            ]
        );
    }

    #[tokio::test]
    async fn test_stop_phase_provides_fresh_deadline_token() {
        use std::sync::atomic::AtomicBool;

        struct TokenCheckGear {
            stop_was_called: AtomicBool,
            token_was_cancelled_on_entry: AtomicBool,
        }

        #[async_trait::async_trait]
        impl Gear for TokenCheckGear {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl RunnableCapability for TokenCheckGear {
            async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
                Ok(())
            }
            async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()> {
                // Record that stop() was called
                self.stop_was_called.store(true, Ordering::SeqCst);
                // Record whether the token was already cancelled when stop() was called
                self.token_was_cancelled_on_entry
                    .store(deadline_token.is_cancelled(), Ordering::SeqCst);
                Ok(())
            }
        }

        let gear = Arc::new(TokenCheckGear {
            stop_was_called: AtomicBool::new(false),
            // Default to true to detect if stop() was never called
            token_was_cancelled_on_entry: AtomicBool::new(true),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("test", &[], gear.clone() as Arc<dyn Gear>);
        builder.register_stateful_with_meta("test", gear.clone() as Arc<dyn RunnableCapability>);

        let registry = builder.build_topo_sorted().unwrap();
        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel.clone(),
            Uuid::new_v4(),
            None,
        );

        // Run stop phase - the deadline token should NOT be cancelled
        runtime.run_stop_phase().await.unwrap();

        // First, verify stop() was actually called (guards against silent registration failures)
        assert!(
            gear.stop_was_called.load(Ordering::SeqCst),
            "stop() was never called - gear may not have been registered correctly"
        );

        // The token should NOT have been cancelled when stop() was called
        // This is the key fix: gears get a fresh token, not the already-cancelled root token
        assert!(
            !gear.token_was_cancelled_on_entry.load(Ordering::SeqCst),
            "deadline_token should NOT be cancelled when stop() is called - this enables graceful shutdown"
        );
    }

    #[tokio::test]
    async fn test_stop_phase_graceful_shutdown_completes_before_deadline() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        struct GracefulGear {
            graceful_completed: AtomicBool,
            deadline_fired: AtomicBool,
        }

        #[async_trait::async_trait]
        impl Gear for GracefulGear {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl RunnableCapability for GracefulGear {
            async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
                Ok(())
            }
            async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()> {
                // Simulate graceful shutdown that completes quickly (10ms)
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(10)) => {
                        self.graceful_completed.store(true, Ordering::SeqCst);
                    }
                    () = deadline_token.cancelled() => {
                        self.deadline_fired.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            }
        }

        let gear = Arc::new(GracefulGear {
            graceful_completed: AtomicBool::new(false),
            deadline_fired: AtomicBool::new(false),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("test", &[], gear.clone() as Arc<dyn Gear>);
        builder.register_stateful_with_meta("test", gear.clone() as Arc<dyn RunnableCapability>);

        let registry = builder.build_topo_sorted().unwrap();
        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        // Use a long deadline (5s) - gear should complete gracefully before this
        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel.clone(),
            Uuid::new_v4(),
            None,
        )
        .with_shutdown_deadline(Duration::from_secs(5));

        runtime.run_stop_phase().await.unwrap();

        // Graceful shutdown should have completed
        assert!(
            gear.graceful_completed.load(Ordering::SeqCst),
            "graceful shutdown should complete"
        );
        // Deadline should NOT have fired (gear finished before deadline)
        assert!(
            !gear.deadline_fired.load(Ordering::SeqCst),
            "deadline should not fire when graceful shutdown completes quickly"
        );
    }

    #[tokio::test]
    async fn test_stop_phase_deadline_fires_for_slow_gear() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        struct SlowGear {
            graceful_completed: AtomicBool,
            deadline_fired: AtomicBool,
        }

        #[async_trait::async_trait]
        impl Gear for SlowGear {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl RunnableCapability for SlowGear {
            async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
                Ok(())
            }
            async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()> {
                // Simulate slow graceful shutdown (would take 10s, but deadline is 100ms)
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(10)) => {
                        self.graceful_completed.store(true, Ordering::SeqCst);
                    }
                    () = deadline_token.cancelled() => {
                        self.deadline_fired.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            }
        }

        let gear = Arc::new(SlowGear {
            graceful_completed: AtomicBool::new(false),
            deadline_fired: AtomicBool::new(false),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("test", &[], gear.clone() as Arc<dyn Gear>);
        builder.register_stateful_with_meta("test", gear.clone() as Arc<dyn RunnableCapability>);

        let registry = builder.build_topo_sorted().unwrap();
        let client_hub = Arc::new(ClientHub::new());
        let cancel = CancellationToken::new();
        let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

        // Use a short deadline (100ms) - gear should be interrupted by deadline
        let runtime = HostRuntime::new(
            registry,
            config_provider,
            DbOptions::None,
            client_hub,
            cancel.clone(),
            Uuid::new_v4(),
            None,
        )
        .with_shutdown_deadline(Duration::from_millis(100));

        runtime.run_stop_phase().await.unwrap();

        // Graceful shutdown should NOT have completed (deadline fired first)
        assert!(
            !gear.graceful_completed.load(Ordering::SeqCst),
            "graceful shutdown should not complete when deadline fires first"
        );
        // Deadline should have fired
        assert!(
            gear.deadline_fired.load(Ordering::SeqCst),
            "deadline should fire for slow gears"
        );
    }
}
