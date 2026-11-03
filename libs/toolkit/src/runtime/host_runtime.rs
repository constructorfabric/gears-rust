//! Host Runtime - orchestrates the full `ToolKit` lifecycle
//!
//! This gear contains the `HostRuntime` type that owns and coordinates
//! the execution of all lifecycle phases.
//!
//! High-level phase order:
//! - the GTS manifest of every `gts_publication(...)` gear, built from the configuration
//!   and checked for ownership conflicts, and the startup group of every
//!   consumed client checked, before any gear runs or anything is written
//! - **system group** (`capabilities = [system]`: the types registry,
//!   service discovery, cluster, the REST and gRPC hosts), complete: runtime
//!   wiring and `pre_init`, DB migrations, `init`, the clients a system gear
//!   owns or provides, `post_init`, the initial REST routes (probes and
//!   platform-plane calls) and gRPC services, `start`, directory
//!   registration; then the configured `OoP` children spawn, without waiting
//!   for their readiness
//! - **application group**, prepared as a whole: `pre_init`, DB migrations,
//!   the registry clients the publications need, then the concurrent GTS
//!   publication of every manifest until all are admitted. No application
//!   `init` runs before that barrier; the system group keeps serving while
//!   it is pending or failed.
//! - application `init`, remaining proxy-wiring (`#[toolkit::consumes]`
//!   clients), the legacy finalization (temporary: the types registry's
//!   in-memory ready mode), application `post_init`, the final REST routes
//!   and the late gRPC services on the running listeners, `start`, directory
//!   registration.
//! - migration-only mode: runtime wiring, `pre_init` and DB migrations of
//!   every gear (the system group first), nothing else: no `init`,
//!   `post_init` or publication.
//! - wait for shutdown, then stop: the application group, then the system
//!   group, each in reverse order
//!
//! Every gear's `pre_init`, `init` and `post_init` run once, within its own
//! group, in topological order.
//!
//! Both lifecycle paths — in-process (`run_phases_internal`) and `OoP`
//! (`run_oop_serving`) — run these in the same relative order. Proxy-wiring in
//! particular must stay before `start`: a gear resolving a consumed contract
//! during its own `start` has to find the client in the `ClientHub` regardless
//! of profile. The `OoP` path has no REST host, directory-register or spawn
//! phases (`oop_serve` owns those concerns).
//!
//! # `deps` vs `#[toolkit::consumes]` — two different relationships
//!
//! These are often confused; they do different jobs (cf. systemd `After=` vs
//! `Requires=`/`Wants=`):
//!
//! - **`deps = [...]`** controls **`init` ordering within one process**, and
//!   requires the named gears to be *linked into this binary* (an unknown dep is a
//!   build-time error). It is therefore an **in-process** concern only: in `OoP`
//!   mode a `dep` can only name a gear in the same process. Use it solely when one
//!   gear's `init` must run after another's in the same binary.
//! - **`#[toolkit::consumes(contract, from)]`** declares a **runtime dependency**
//!   that may be satisfied **locally or remotely**. It adds **no ordering** edge;
//!   it is wired in the proxy-wiring phase (local impl wins, else a
//!   directory-resolving proxy) and gates `/readyz` until the provider resolves.
//!   This is the parity-safe way to depend on another gear — it behaves
//!   identically in- and out-of-process.
//!
//! Rule of thumb: reach for `#[toolkit::consumes]`; use `deps` only for genuine
//! same-process `init` ordering, never as an inter-gear-call or cross-profile
//! guarantee.

use axum::Router;
use std::collections::HashSet;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::backends::OopSpawnConfig;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::context::{GearContextBuilder, GtsAdmittedManifests};
use crate::gts_publication::{GtsManifest, GtsPublicationDescriptor, PreparedGtsPublication};
use crate::registry::{
    ApiGatewayCap, GearEntry, GearRegistry, GrpcHubCap, GtsPublicationCap, RegistryError,
    RestApiCap, RunnableCap, SystemCap,
};
use crate::runtime::{
    GearManager, GrpcInstallerStore, OopSpawnOptions, PREPARATION_GATE, SystemContext,
};

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

/// How long the runtime waits for the gRPC hub to install the late services
/// of the application group on its running listener.
const LATE_GRPC_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How the startup of the application group ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppStartup {
    /// Every application gear initialized and started.
    Started,
    /// A publication was refused: no application `init` ran. The system
    /// group keeps serving until shutdown.
    Failed,
    /// Shutdown came first.
    Cancelled,
}

/// The REST router composed so far against the in-process host.
#[derive(Default)]
struct RestComposition {
    /// The shared healthcheck registry, created when the host is prepared.
    hc_registry: Option<Arc<crate::healthcheck::RestHealthcheckRegistry>>,
    /// Routes of the gears registered so far, before the host's middleware.
    router: Option<Router>,
    /// Gears whose routes are registered: each one exactly once.
    composed: HashSet<&'static str>,
}

/// Owns the lifecycle orchestration for `ToolKit`: encapsulates all runtime
/// state and drives gears through the full lifecycle (see gear docs).
pub struct HostRuntime {
    registry: GearRegistry,
    ctx_builder: GearContextBuilder,
    instance_id: Uuid,
    gear_manager: Arc<GearManager>,
    grpc_installers: Arc<GrpcInstallerStore>,
    client_hub: Arc<ClientHub>,
    /// Per-gear config, retained for the proxy-wiring phase to read a consumer's
    /// static-endpoint override (dev/test escape hatch, ADR-0004).
    gears_cfg: Arc<dyn ConfigProvider>,
    /// Process-level dependency-resolution + draining signal, published in
    /// `client_hub` for the `/readyz` probe and updated by the proxy-wiring
    /// readiness loop + draining watcher. In-process, an
    /// [`ReadinessHealthcheck`](super::readiness::ReadinessHealthcheck) leaf
    /// bridges it into the gateway's healthcheck registry.
    dep_checker: Arc<super::readiness::DependencyChecker>,
    /// Admitted manifests, read by the contexts of application gears.
    gts_admitted: GtsAdmittedManifests,
    /// In-process REST provider advertisement. In `OoP` serving, presence +
    /// deregister is owned by `oop_serve`, so it stays unused.
    advertiser: Arc<RestAdvertiser>,
    /// Gears whose `start` the runtime called: the ones the stop phase stops,
    /// on shutdown and after a failed startup alike.
    start_called: parking_lot::Mutex<HashSet<&'static str>>,
    /// Consumer registrations already wired, by address: the registry
    /// clients are wired early, the rest after application `init`, each once.
    wired: parking_lot::Mutex<HashSet<usize>>,
    /// The in-process REST router composed so far.
    rest: parking_lot::Mutex<RestComposition>,
    cancel: CancellationToken,
    /// Ends the runtime's own background tasks (readiness probes, the drain
    /// watcher) when startup fails or the runtime is dropped, without
    /// cancelling `cancel`, which the caller may share. A child of `cancel`.
    owned: CancellationToken,
    /// Cancels `owned` when the runtime is dropped — the lifecycle future
    /// abandoned.
    _owned_on_drop: tokio_util::sync::DropGuard,
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

        // Process-level dependency/draining signal, published so the gateway's
        // /readyz handler can fetch it (concrete-type key). Created before any
        // phase runs.
        let dep_checker = Arc::new(super::readiness::DependencyChecker::new());
        client_hub.register::<super::readiness::DependencyChecker>(dep_checker.clone());

        let owned = cancel.child_token();
        let owned_on_drop = owned.clone().drop_guard();
        dep_checker.register_dep(PREPARATION_GATE);

        // Build the context builder that will resolve per-gear DbHandles
        let ctx_builder = GearContextBuilder::new(
            instance_id,
            gears_cfg.clone(),
            client_hub.clone(),
            cancel.clone(),
        );
        let gts_admitted = ctx_builder.gts_admitted_manifests();
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
            gears_cfg,
            dep_checker,
            gts_admitted,
            advertiser: Arc::new(RestAdvertiser::new(instance_id)),
            start_called: parking_lot::Mutex::new(HashSet::new()),
            wired: parking_lot::Mutex::new(HashSet::new()),
            rest: parking_lot::Mutex::new(RestComposition::default()),
            owned,
            _owned_on_drop: owned_on_drop,
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
    /// `shutdown_deadline` (default [`DEFAULT_SHUTDOWN_DEADLINE`], 35s). To ensure
    /// deterministic behavior:
    ///
    /// - `WithLifecycle::stop_timeout` should be **less than** `shutdown_deadline`
    /// - This allows the lifecycle's internal timeout to trigger first for graceful cleanup
    /// - The runtime's `deadline_token` then acts as a hard backstop
    ///
    /// Example: `stop_timeout = 30s`, `shutdown_deadline = 35s`
    #[must_use]
    pub fn with_shutdown_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.shutdown_deadline = deadline;
        self
    }

    /// Set the process-wide platform-plane credential source, applied to every
    /// [`GearCtx`](crate::context::GearCtx) this runtime builds so
    /// `#[toolkit::provides]`-generated clients attach `X-ToolKit-Internal-Token`
    /// on platform-plane methods. `None` (Profile 1 / in-process, or no
    /// credential configured) attaches nothing.
    #[must_use]
    pub fn with_internal_token_provider(
        mut self,
        provider: Option<toolkit_contract::runtime::config::InternalTokenProvider>,
    ) -> Self {
        self.ctx_builder = self.ctx_builder.with_internal_token_provider(provider);
        self
    }

    /// The system group (`system = true`) or the application group, in
    /// topological order.
    fn group(&self, system: bool) -> Vec<&GearEntry> {
        self.registry
            .gears()
            .iter()
            .filter(|entry| entry.is_system() == system)
            .collect()
    }

    /// Every gear, the system group first, each group in topological order.
    fn by_group(&self) -> Vec<&GearEntry> {
        let mut gears = self.group(true);
        gears.extend(self.group(false));
        gears
    }

    /// The runtime internals handed to the system gears.
    fn system_context(&self) -> SystemContext {
        SystemContext::new(
            self.instance_id,
            Arc::clone(&self.gear_manager),
            Arc::clone(&self.grpc_installers),
        )
    }

    /// `PRE_INIT` phase over every gear, the system group first
    /// (migration-only mode).
    ///
    /// # Errors
    /// Returns `RegistryError` if runtime wiring or a `pre_init` fails.
    pub async fn run_pre_init_phase(&self) -> Result<(), RegistryError> {
        self.run_pre_init_phase_for(&self.by_group()).await
    }

    /// `PRE_INIT` phase of `entries`, before their migrations and `init`:
    /// first the runtime internals into each system gear
    /// ([`SystemCapability::wire_runtime`](crate::contracts::SystemCapability::wire_runtime)),
    /// then every gear's `pre_init`, in the given order. The `pre_init`
    /// context has no database handle and no admitted GTS manifest.
    async fn run_pre_init_phase_for(&self, entries: &[&GearEntry]) -> Result<(), RegistryError> {
        tracing::info!(gears = entries.len(), "Phase: pre_init");

        let sys_ctx = self.system_context();
        for entry in entries {
            if self.cancel.is_cancelled() {
                tracing::warn!("Pre-init phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }
            if let Some(system) = entry.caps.query::<SystemCap>() {
                tracing::debug!(gear = entry.name, "Wiring runtime internals");
                system
                    .wire_runtime(&sys_ctx)
                    .map_err(|e| RegistryError::PreInit {
                        gear: entry.name,
                        source: e.context("runtime wiring"),
                    })?;
            }
        }

        for entry in entries {
            // Check for cancellation before processing each gear
            if self.cancel.is_cancelled() {
                tracing::warn!("Pre-init phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }
            let ctx = self.ctx_builder.for_gear_before_migrations(entry.name);
            entry
                .core
                .pre_init(&ctx)
                .await
                .map_err(|e| RegistryError::PreInit {
                    gear: entry.name,
                    source: e,
                })?;
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

    /// DB MIGRATION phase over every gear, the system group first
    /// (migration-only mode).
    #[cfg(feature = "db")]
    async fn run_db_phase(&self) -> Result<(), RegistryError> {
        self.run_db_phase_for(&self.by_group()).await
    }

    /// DB MIGRATION phase: run migrations for `entries` with DB capability,
    /// in the given order.
    ///
    /// Gears provide migrations via `DatabaseCapability::migrations()`.
    /// The runtime executes them with a privileged connection that gears
    /// never receive directly. Each gear gets a separate migration history
    /// table, preventing cross-gear interference.
    #[cfg(feature = "db")]
    async fn run_db_phase_for(&self, entries: &[&GearEntry]) -> Result<(), RegistryError> {
        tracing::info!(gears = entries.len(), "Phase: db (before init)");

        for entry in entries {
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

    /// INIT phase over every gear, the system group first.
    #[cfg(test)]
    async fn run_init_phase(&self) -> Result<(), RegistryError> {
        self.run_init_phase_for(&self.by_group()).await
    }

    /// INIT phase: initialize `entries` in the given order.
    async fn run_init_phase_for(&self, entries: &[&GearEntry]) -> Result<(), RegistryError> {
        tracing::info!(gears = entries.len(), "Phase: init");

        for entry in entries {
            if self.cancel.is_cancelled() {
                tracing::warn!("Init phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }
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
    fn consumer_registrations() -> Vec<&'static crate::discovery::ConsumerRegistration> {
        inventory::iter::<crate::discovery::ConsumerRegistration>
            .into_iter()
            .collect()
    }

    /// The body of the proxy-wiring phase, taking the registrations so tests can
    /// drive it with a locally-built registration instead of the link-time-global
    /// `inventory` set from [`Self::consumer_registrations`] (which would leak a
    /// dep onto other tests).
    ///
    /// A registration already wired (the registry clients are wired before
    /// the application group, the rest after its `init`) is skipped: each one
    /// is wired once. Registrations are told apart by address, so two
    /// contracts of one provider are two registrations.
    ///
    /// Sync, not `async`: the readiness probe is `tokio::spawn`ed, not awaited.
    fn wire_consumer_registrations(
        &self,
        regs: &[&crate::discovery::ConsumerRegistration],
    ) -> Result<Vec<crate::discovery::WireOutcome>, RegistryError> {
        let regs: Vec<&crate::discovery::ConsumerRegistration> = {
            let mut wired = self.wired.lock();
            regs.iter()
                .copied()
                .filter(|reg| wired.insert(std::ptr::from_ref(*reg) as usize))
                .collect()
        };
        self.wire_untracked(&regs)
    }

    /// The body of [`Self::wire_consumer_registrations`], wiring every one of
    /// `regs` once more.
    fn wire_untracked(
        &self,
        regs: &[&crate::discovery::ConsumerRegistration],
    ) -> Result<Vec<crate::discovery::WireOutcome>, RegistryError> {
        use crate::discovery::{DirectoryEndpointResolver, NullEndpointResolver};
        use toolkit_contract::runtime::resolving::EndpointResolver;

        if regs.is_empty() {
            return Ok(Vec::new());
        }
        tracing::info!(
            count = regs.len(),
            "Phase: proxy-wiring (consumer discovery)"
        );

        // Without a DirectoryClient we cannot resolve *remote* providers, but we
        // must NOT silently skip wiring: co-located consumers still short-circuit
        // to their local impl, and remote consumers must register as unresolved
        // readiness gates so `/readyz` stays 503 (a misconfigured consumer must
        // not report Ready). A null resolver makes every remote lookup `Ok(None)`
        // so the loop below classifies deps correctly without a directory.
        let (resolver, have_directory): (Arc<dyn EndpointResolver>, bool) =
            if let Ok(dir) = self.client_hub.get::<dyn crate::DirectoryClient>() {
                (Arc::new(DirectoryEndpointResolver::new(dir)), true)
            } else {
                tracing::error!(
                    consumers = regs.len(),
                    "proxy-wiring: no DirectoryClient in ClientHub; remote consumer \
                     dependencies cannot be resolved and will gate /readyz (503). \
                     Co-located (local) dependencies are unaffected."
                );
                (Arc::new(NullEndpointResolver), false)
            };

        // Wire each consumer contract. The outcome distinguishes a co-located
        // local impl (hub short-circuit) from a directory-resolving REST client.
        // Every dep is registered as a readiness gate; local ones are marked
        // resolved immediately, and only remote ones gate readiness + get the
        // background directory-resolve loop (ADR-0007: startup-gating + sticky).
        // `owner_gear` is set by `#[toolkit::consumes]` to the gear's declared
        // `#[toolkit::gear(name = ...)]`, falling back to the kebab-case of the
        // struct ident when that attribute is absent. It is used as the config key
        // `gears.<owner_gear>.config.consumer_wiring.<dep>`. If it does not match
        // any registered gear name, wiring still works (the loop below does not
        // filter on owner) but that static-override config key silently resolves to
        // nothing, so warn.
        let known_gears: std::collections::HashSet<&str> =
            self.registry.gears().iter().map(GearEntry::name).collect();
        // In-process gears with no REST API: a `Remote` outcome for
        // one of these is a self-pointing dead proxy (see the hard error below).
        // Directory advertisement is a separate choice: a partial REST provider
        // can opt out so other contracts resolve to a provider in another process.
        let unreachable_in_process: std::collections::HashSet<&str> = self
            .registry
            .gears()
            .iter()
            .filter(|gear| !gear.caps.has::<RestApiCap>())
            .map(GearEntry::name)
            .collect();
        for reg in regs {
            if !known_gears.contains(reg.owner_gear) {
                tracing::warn!(
                    owner = reg.owner_gear,
                    dep = reg.dep_gear,
                    "proxy-wiring: consumer's owner gear name does not match any registered gear; \
                     the `gears.{}.config.consumer_wiring.{}` static override will never resolve. \
                     Ensure the consumer's `#[toolkit::gear(name = ...)]` matches its registered name.",
                    reg.owner_gear,
                    reg.dep_gear,
                );
            }
        }

        let mut outcomes = Vec::with_capacity(regs.len());
        let mut remote_deps: Vec<String> = Vec::new();
        for reg in regs {
            // Read the consumer's `consumer_wiring.<dep>` entry (if any) and
            // split it into an optional static-endpoint override and the
            // per-deployment `ClientTuning` (timeout/retry/pool/concurrency).
            let (endpoint_override, mut tuning) =
                match read_consumer_wiring(self.gears_cfg.as_ref(), reg.owner_gear, reg.dep_gear)
                    .map_err(|source| RegistryError::ProxyWiring {
                        gear: reg.owner_gear,
                        source,
                    })? {
                    Some(wiring) => wiring.into_parts(),
                    None => (None, toolkit_contract::wiring::ClientTuning::default()),
                };

            // Thread the process's platform-plane credential onto the tuning so
            // the wired (directory-resolving) client's platform-plane methods
            // attach `X-ToolKit-Internal-Token` (`cpt-cf-adr-two-plane-auth`).
            // This is the genuine remote inter-gear path (Profile 2/3); a
            // co-located local impl short-circuits before the credential is used.
            tuning = tuning
                .with_internal_token_provider(self.ctx_builder.internal_token_provider().cloned());

            // ADR-0004 static-endpoint override (dev/test escape hatch): a
            // configured `endpoint` wires the dep directly via a
            // `StaticEndpointResolver`, bypassing discovery. It was validated as an
            // absolute URI at parse time (invalid ones aborted above) and is
            // resolvable by construction, so no probe loop is needed. Warned — must
            // not be used in production.
            let is_static = endpoint_override.is_some();
            let reg_resolver: Arc<dyn EndpointResolver> = if let Some(endpoint) = &endpoint_override
            {
                tracing::warn!(
                    owner = reg.owner_gear,
                    dep = reg.dep_gear,
                    endpoint = %endpoint,
                    "proxy-wiring: STATIC endpoint override in use (ADR-0004 dev/test \
                     escape hatch) - bypasses service discovery; MUST NOT be used in \
                     production"
                );
                Arc::new(crate::discovery::StaticEndpointResolver::new(
                    endpoint.clone(),
                ))
            } else {
                Arc::clone(&resolver)
            };

            let outcome = (reg.wire)(&self.client_hub, reg_resolver, tuning).map_err(|source| {
                RegistryError::ProxyWiring {
                    gear: reg.owner_gear,
                    source,
                }
            })?;
            self.dep_checker.register_dep(reg.dep_gear.to_owned());
            match outcome {
                // Local impl won the hub short-circuit — resolved.
                crate::discovery::WireOutcome::Local => {
                    self.dep_checker.mark_resolved(reg.dep_gear);
                }
                // Static override → fixed endpoint, always resolvable — no probe.
                crate::discovery::WireOutcome::Remote if is_static => {
                    self.dep_checker.mark_resolved(reg.dep_gear);
                }
                // A registered consumer depends on an in-process dep with no local
                // impl and no REST API: wiring fell to a self-pointing proxy that
                // would gate `/readyz` forever. Gated on the owner being registered
                // too, so a stray/unused consumer (owner not in this process) falls
                // through to the normal remote path below instead of aborting.
                crate::discovery::WireOutcome::Remote
                    if known_gears.contains(reg.owner_gear)
                        && unreachable_in_process.contains(reg.dep_gear) =>
                {
                    return Err(RegistryError::ProxyWiring {
                        gear: reg.owner_gear,
                        source: anyhow::anyhow!(
                            "consumer '{}' depends on in-process gear '{}', which registered no \
                             local impl and serves no REST API; the provider must register its \
                             client in init() (#[toolkit::provides] wire_* or ClientHub::register) \
                             or serve the contract over REST.",
                            reg.owner_gear,
                            reg.dep_gear,
                        ),
                    });
                }
                // Directory-resolved remote → gate readiness + background probe.
                crate::discovery::WireOutcome::Remote => remote_deps.push(reg.dep_gear.to_owned()),
            }
            tracing::debug!(
                owner = reg.owner_gear,
                dep = reg.dep_gear,
                outcome = ?outcome,
                static_override = is_static,
                "wired consumer contract"
            );
            outcomes.push(outcome);
        }

        // Only remote deps need directory resolution; local wins are already
        // resolved above. Without a directory the null resolver can never resolve
        // them, so skip the probe entirely and leave those deps gating /readyz.
        if !have_directory || remote_deps.is_empty() {
            return Ok(outcomes);
        }

        let readiness = Arc::clone(&self.dep_checker);
        let cancel = self.owned.clone();
        tokio::spawn(async move {
            const BASE: std::time::Duration = std::time::Duration::from_millis(100);
            const MAX: std::time::Duration = std::time::Duration::from_secs(30);
            let mut pending = remote_deps;
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

        Ok(outcomes)
    }

    /// Build the GTS manifest of every publishing gear from the effective
    /// configuration and check that no GTS id has two publishers, before any
    /// gear runs. A manifest is never rebuilt afterwards: the admitted one is
    /// what an application `init` reads.
    fn build_gts_manifests(
        &self,
    ) -> Result<Vec<(&'static GtsPublicationDescriptor, Arc<GtsManifest>)>, RegistryError> {
        let mut manifests = Vec::new();
        for entry in self.registry.gears() {
            let Some(publication) = entry.caps.query::<GtsPublicationCap>() else {
                continue;
            };
            if entry.is_system() {
                return Err(RegistryError::Publication {
                    gear: entry.name,
                    source: anyhow::anyhow!(
                        "a system gear initializes before the GTS barrier, so it cannot \
                         publish GTS documents with `gts_publication(...)`"
                    ),
                });
            }
            let descriptor = publication.descriptor();
            let manifest = descriptor
                .build_gts_manifest(self.gears_cfg.as_ref())
                .map_err(|source| RegistryError::Publication {
                    gear: entry.name,
                    source,
                })?;
            manifests.push((descriptor, Arc::new(manifest)));
        }
        if let Some((first, _)) = manifests.first() {
            crate::gts_publication::check_ownership(manifests.iter().map(|(_, m)| m.as_ref()))
                .map_err(|source| RegistryError::Publication {
                    gear: first.gear,
                    source,
                })?;
        }
        Ok(manifests)
    }

    /// Wire the registry clients the publications need, in the application
    /// group after its migrations and before the GTS barrier: the publisher
    /// client of every non-empty manifest, then every consumer
    /// registration of a provider such a manifest publishes to (a consumed
    /// tenant API of the same registry, say). A local provider wins; otherwise
    /// a resolving client with the platform credential. The other
    /// registrations are wired after application `init`, so a local
    /// application provider is never shadowed by a proxy. An empty manifest
    /// needs no registry: its publisher client is not wired, and nothing
    /// waits for its provider.
    ///
    /// A provider gear in this process that advertises itself but does not
    /// provide the contract locally would make the publication call itself
    /// forever; that is a configuration error (an embedded registry without
    /// its database, say).
    fn wire_registry_clients(
        &self,
        manifests: &[(&'static GtsPublicationDescriptor, Arc<GtsManifest>)],
    ) -> Result<(), RegistryError> {
        self.wire_registry_clients_over(manifests, &Self::consumer_registrations())
    }

    /// [`Self::wire_registry_clients`] over the given consumer registrations.
    fn wire_registry_clients_over(
        &self,
        manifests: &[(&'static GtsPublicationDescriptor, Arc<GtsManifest>)],
        all: &[&'static crate::discovery::ConsumerRegistration],
    ) -> Result<(), RegistryError> {
        let publications: Vec<crate::discovery::ConsumerRegistration> = manifests
            .iter()
            .filter(|(_, manifest)| !manifest.is_empty())
            .map(|(descriptor, _)| crate::discovery::ConsumerRegistration {
                owner_gear: descriptor.gear,
                dep_gear: descriptor.provider,
                wire: descriptor.wire,
            })
            .collect();
        if publications.is_empty() {
            return Ok(());
        }
        let publishers: Vec<&crate::discovery::ConsumerRegistration> =
            publications.iter().collect();
        let outcomes = self.wire_untracked(&publishers)?;
        for (publisher, outcome) in publishers.iter().zip(outcomes) {
            if outcome != crate::discovery::WireOutcome::Remote {
                continue;
            }
            let advertised_here = self.registry.gears().iter().any(|entry| {
                entry.name == publisher.dep_gear
                    && entry
                        .caps
                        .query::<RestApiCap>()
                        .is_some_and(|rest| rest.advertised_in_directory())
            });
            if advertised_here {
                return Err(RegistryError::Publication {
                    gear: publisher.owner_gear,
                    source: anyhow::anyhow!(
                        "'{}' runs in this process and advertises itself, but does not provide \
                         the contract the publication needs here: publishing would call this \
                         process forever. Give it what it needs to provide the contract (an \
                         embedded types registry needs its database), or run it elsewhere",
                        publisher.dep_gear
                    ),
                });
            }
        }
        let providers: HashSet<&str> = publications.iter().map(|p| p.dep_gear).collect();
        let consumers: Vec<&crate::discovery::ConsumerRegistration> = all
            .iter()
            .copied()
            .filter(|reg| providers.contains(reg.dep_gear))
            .collect();
        self.wire_consumer_registrations(&consumers)?;
        Ok(())
    }

    /// Whether `gear` runs in this process, and in the system group.
    fn local_group(&self, gear: &str) -> Option<bool> {
        self.registry
            .gears()
            .iter()
            .find(|e| e.name == gear)
            .map(GearEntry::is_system)
    }

    /// Refuse, before any gear runs, a client the runtime would wire before
    /// its provider in this process initialized, where the proxy would shadow
    /// the local client for good: a system gear consuming an application
    /// gear, or a non-empty publication to an application gear.
    fn check_wiring_groups(
        &self,
        manifests: &[(&'static GtsPublicationDescriptor, Arc<GtsManifest>)],
        regs: &[&'static crate::discovery::ConsumerRegistration],
    ) -> Result<(), RegistryError> {
        let application = |gear: &str| self.local_group(gear) == Some(false);
        for reg in regs {
            if self.local_group(reg.owner_gear) == Some(true) && application(reg.dep_gear) {
                return Err(RegistryError::ProxyWiring {
                    gear: reg.owner_gear,
                    source: anyhow::anyhow!(
                        "system gear '{}' consumes '{}', an application gear of this \
                         process: its client is needed before that gear initializes",
                        reg.owner_gear,
                        reg.dep_gear
                    ),
                });
            }
        }
        for (descriptor, manifest) in manifests {
            if !manifest.is_empty() && application(descriptor.provider) {
                return Err(RegistryError::Publication {
                    gear: descriptor.gear,
                    source: anyhow::anyhow!(
                        "'{}' publishes to '{}', an application gear of this process: the \
                         publication precedes every application `init`",
                        descriptor.gear,
                        descriptor.provider
                    ),
                });
            }
        }
        Ok(())
    }

    /// Wire, after the system group's `init`, every consumer registration a
    /// system gear of this process owns or provides: a local system provider
    /// is in the hub by then, so it wins, and a system consumer has its
    /// client before its `post_init` and `start`. The others wait for the
    /// application group's `init`, so a local application provider is never
    /// shadowed by a proxy.
    fn wire_system_clients(
        &self,
        all: &[&'static crate::discovery::ConsumerRegistration],
    ) -> Result<(), RegistryError> {
        let system = |gear: &str| self.local_group(gear) == Some(true);
        let regs: Vec<&crate::discovery::ConsumerRegistration> = all
            .iter()
            .copied()
            .filter(|reg| system(reg.owner_gear) || system(reg.dep_gear))
            .collect();
        self.wire_consumer_registrations(&regs).map(drop)
    }

    /// `POST_INIT` phase over every gear, the system group first.
    #[cfg(test)]
    async fn run_post_init_phase(&self) -> Result<(), RegistryError> {
        self.run_post_init_phase_for(&self.by_group()).await
    }

    /// `POST_INIT` phase: every gear's `post_init` of `entries`, after their
    /// group's `init` and proxy wiring, in the given order.
    async fn run_post_init_phase_for(&self, entries: &[&GearEntry]) -> Result<(), RegistryError> {
        tracing::info!(gears = entries.len(), "Phase: post_init");

        for entry in entries {
            if self.cancel.is_cancelled() {
                tracing::warn!("Post-init phase cancelled by signal");
                return Err(RegistryError::Cancelled);
            }
            let ctx = self.ctx_builder.for_gear(entry.name).await.map_err(|e| {
                RegistryError::PostInit {
                    gear: entry.name,
                    source: e,
                }
            })?;
            entry
                .core
                .post_init(&ctx)
                .await
                .map_err(|e| RegistryError::PostInit {
                    gear: entry.name,
                    source: e,
                })?;
        }

        Ok(())
    }

    /// Legacy finalization, once: after every application `init` and the
    /// remaining proxy wiring, before the application `post_init`. Temporary:
    /// it serves only the types registry's in-memory ready mode, which commits
    /// what the gears registered during `init` at once.
    async fn run_legacy_finalization(&self) -> Result<(), RegistryError> {
        let sys_ctx = self.system_context();
        for entry in self.registry.gears() {
            if let Some(sys_mod) = entry.caps.query::<SystemCap>() {
                sys_mod
                    .legacy_finalize_after_application_init(&sys_ctx)
                    .await
                    .map_err(|e| RegistryError::PostInit {
                        gear: entry.name,
                        source: e.context("legacy finalization after application init"),
                    })?;
            }
        }
        Ok(())
    }

    /// Init -> proxy-wiring -> post-init over every gear, for tests that
    /// drive the shared segment directly.
    #[cfg(test)]
    async fn run_init_wiring_post_init(&self) -> Result<(), RegistryError> {
        self.run_init_phase().await?;
        self.wire_consumer_registrations(&Self::consumer_registrations())?;
        self.run_post_init_phase().await
    }

    /// The GTS barrier: publish every manifest of the application group
    /// concurrently and wait until each one is admitted. Then record the
    /// admitted manifests for the application contexts.
    ///
    /// A backend that does not bind is a local configuration error. A
    /// refusal fails the preparation ([`AppStartup::Failed`]): no application
    /// `init` runs, and the refusal is not retried.
    async fn run_gts_publication_barrier(
        &self,
        manifests: &[(&'static GtsPublicationDescriptor, Arc<GtsManifest>)],
    ) -> Result<AppStartup, RegistryError> {
        let mut prepared = Vec::with_capacity(manifests.len());
        for (descriptor, manifest) in manifests {
            prepared.push(
                PreparedGtsPublication::bind(descriptor, Arc::clone(manifest), &self.client_hub)
                    .map_err(|e| RegistryError::Publication {
                        gear: descriptor.gear,
                        source: e.into(),
                    })?,
            );
        }
        if prepared.iter().any(|p| !p.gts_manifest().is_empty()) {
            tracing::info!(
                publishers = prepared.len(),
                "Phase: GTS publication (application group waits for admission)"
            );
        }
        let results = crate::gts_publication::publish_all(
            &prepared,
            &self.cancel,
            Arc::new(|gear, detail: &str| {
                tracing::debug!(gear, detail, "GTS publication pending");
            }),
            true,
        )
        .await;

        let mut refusals = Vec::new();
        for (_, result) in results {
            match result {
                Ok(()) | Err(crate::gts_publication::GtsPublicationError::Cancelled { .. }) => {}
                Err(e) => refusals.push(format!("{e:#}")),
            }
        }
        if !refusals.is_empty() {
            tracing::error!(
                error = %refusals.join("; "),
                "application group preparation failed; the system group keeps running until shutdown"
            );
            return Ok(AppStartup::Failed);
        }
        if self.cancel.is_cancelled() {
            return Ok(AppStartup::Cancelled);
        }
        for publication in &prepared {
            self.gts_admitted.insert(publication.shared_gts_manifest());
        }
        Ok(AppStartup::Started)
    }

    /// The host gear and its REST host capability, if any.
    fn rest_host(&self) -> Option<(&GearEntry, Arc<dyn crate::contracts::ApiGatewayCapability>)> {
        self.registry
            .gears()
            .iter()
            .find_map(|e| e.caps.query::<ApiGatewayCap>().map(|host| (e, host)))
    }

    /// Prepare the REST host once and return the shared healthcheck registry.
    fn prepare_rest_host(
        &self,
        host_entry: &GearEntry,
        host: &Arc<dyn crate::contracts::ApiGatewayCapability>,
        host_ctx: &crate::context::GearCtx,
    ) -> Result<Arc<crate::healthcheck::RestHealthcheckRegistry>, RegistryError> {
        let mut rest = self.rest.lock();
        if let Some(hc_registry) = &rest.hc_registry {
            return Ok(Arc::clone(hc_registry));
        }
        // Healthcheck registry, passed explicitly to the REST host and providers below
        // (not via ClientHub). Seeded with the host's shutdown token so in-flight checks
        // are aborted on shutdown.
        let hc_registry = Arc::new(
            crate::healthcheck::RestHealthcheckRegistry::with_cancellation(
                host_ctx.cancellation_token().clone(),
            ),
        );

        // Bridge the process-level eventual-readiness state into the served
        // probe: while any consumed dependency is unresolved, the application
        // group is not started (or the process is draining) this synthetic
        // check reports Unhealthy, so the gateway's `/readyz` returns 503.
        // (`/healthz` is a static liveness handler and is unaffected.)
        hc_registry.register(
            "readiness",
            Arc::new(super::readiness::ReadinessHealthcheck::new(
                self.dep_checker.clone(),
            )),
        );

        // Host prepare: base Router / global middlewares / basic OAS meta
        let router = host
            .rest_prepare(host_ctx, Router::new(), hc_registry.clone())
            .map_err(|source| RegistryError::RestPrepare {
                gear: host_entry.name,
                source,
            })?;
        rest.hc_registry = Some(Arc::clone(&hc_registry));
        rest.router = Some(router);
        Ok(hc_registry)
    }

    /// Register the REST routes of `entries` not registered yet onto the
    /// composed router, in the given order.
    async fn compose_rest(
        &self,
        entries: &[&GearEntry],
        registry: &dyn crate::contracts::OpenApiRegistry,
        hc_registry: &Arc<crate::healthcheck::RestHealthcheckRegistry>,
    ) -> Result<Router, RegistryError> {
        let mut router = self.rest.lock().router.take().unwrap_or_default();
        for e in entries {
            let Some(rest) = e.caps.query::<RestApiCap>() else {
                continue;
            };
            if !self.rest.lock().composed.insert(e.name) {
                continue;
            }
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

            // Register the gear's readiness healthcheck after successful route registration.
            if let Some(hc) = rest.healthcheck(&ctx) {
                hc_registry.register(e.name, hc);
            }
        }
        self.rest.lock().router = Some(router.clone());
        Ok(router)
    }

    /// Early REST phase: when the REST host is a system gear, serve the
    /// routes of the system gears before the application group is prepared
    /// (probes and platform-plane calls; everything else `503`).
    async fn run_rest_bootstrap_phase(&self, system: &[&GearEntry]) -> Result<(), RegistryError> {
        let Some((host_entry, host)) = self.rest_host() else {
            return Ok(());
        };
        if !host_entry.is_system() {
            return Ok(());
        }
        tracing::info!("Phase: rest (bootstrap routes)");
        let host_ctx = self
            .ctx_builder
            .for_gear(host_entry.name)
            .await
            .map_err(|e| RegistryError::RestPrepare {
                gear: host_entry.name,
                source: e,
            })?;
        let hc_registry = self.prepare_rest_host(host_entry, &host, &host_ctx)?;
        let in_order: Vec<&GearEntry> = self
            .registry
            .gears()
            .iter()
            .filter(|e| system.iter().any(|i| i.name == e.name))
            .collect();
        let router = self
            .compose_rest(&in_order, host.as_registry(), &hc_registry)
            .await?;
        host.rest_bootstrap(&host_ctx, router)
            .map_err(|source| RegistryError::RestPrepare {
                gear: host_entry.name,
                source,
            })
    }

    /// REST phase: compose the full router against the REST host.
    ///
    /// Prepares the host unless the bootstrap phase did, registers the routes
    /// of every gear not registered yet (the system gears' are kept),
    /// then finalizes with `OpenAPI` endpoints. A host serving already swaps
    /// the new routes in on its listener.
    async fn run_rest_phase(&self) -> Result<Router, RegistryError> {
        tracing::info!("Phase: rest (sync)");

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
                    Ok(Router::new())
                };
            }
            1 => { /* proceed */ }
            _ => return Err(RegistryError::MultipleRestHosts),
        }

        let (host_entry, host) = self
            .rest_host()
            .ok_or(RegistryError::RestHostNotFoundAfterValidation)?;
        let host_ctx = self
            .ctx_builder
            .for_gear(host_entry.name)
            .await
            .map_err(|e| RegistryError::RestPrepare {
                gear: host_entry.name,
                source: e,
            })?;
        let hc_registry = self.prepare_rest_host(host_entry, &host, &host_ctx)?;

        // Register all REST providers (in the current discovery order)
        let all: Vec<&GearEntry> = self.registry.gears().iter().collect();
        let router = self
            .compose_rest(&all, host.as_registry(), &hc_registry)
            .await?;

        // Host finalize: attach /openapi.json and /docs, persist Router if needed (no server start)
        host.rest_finalize(&host_ctx, router, hc_registry)
            .map_err(|source| RegistryError::RestFinalize {
                gear: host_entry.name,
                source,
            })
    }

    /// Collect the gRPC installers of the service gears of the system group
    /// (`system = true`) or the application group, reject duplicate service
    /// names, and pin their owners.
    async fn collect_grpc_installers(
        &self,
        system: bool,
        seen: &mut HashSet<&'static str>,
    ) -> Result<Vec<crate::runtime::GearInstallers>, RegistryError> {
        let mut gears_data = Vec::new();
        // Compiled-in service_name -> owning-gear map, installed as
        // authoritative ownership below (see
        // `GearManager::merge_authoritative_grpc_service_owners`).
        let mut owners: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (gear_name, service_gear) in &self.registry.grpc_services {
            let in_group = self
                .registry
                .gears()
                .iter()
                .any(|e| e.name == gear_name.as_str() && e.is_system() == system);
            if !in_group {
                continue;
            }
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
                owners.insert(reg.service_name.to_owned(), gear_name.clone());
            }

            gears_data.push(crate::runtime::GearInstallers {
                gear_name: gear_name.clone(),
                installers,
            });
        }
        if !owners.is_empty() {
            self.gear_manager
                .merge_authoritative_grpc_service_owners(owners);
        }
        Ok(gears_data)
    }

    /// Whether gRPC is in play at all; an error when services exist without a hub.
    fn grpc_in_use(&self) -> Result<bool, RegistryError> {
        match (
            &self.registry.grpc_hub,
            self.registry.grpc_services.is_empty(),
        ) {
            (None, true) => Ok(false),
            (None, false) => Err(RegistryError::GrpcRequiresHub),
            (Some(_), _) => Ok(true),
        }
    }

    /// Initial gRPC batch: the services of the system gears, for the
    /// hub to serve when it starts. Says whether application services follow.
    async fn run_grpc_initial_phase(
        &self,
        seen: &mut HashSet<&'static str>,
    ) -> Result<(), RegistryError> {
        if !self.grpc_in_use()? {
            return Ok(());
        }
        tracing::info!("Phase: grpc (initial services)");
        let gears = self.collect_grpc_installers(true, seen).await?;
        let more_batches = self.registry.grpc_services.iter().any(|(gear, _)| {
            self.registry
                .gears()
                .iter()
                .any(|e| e.name == gear.as_str() && !e.is_system())
        });
        let hub_name = self.registry.grpc_hub.clone().unwrap_or_default();
        self.grpc_installers
            .set(crate::runtime::GrpcInstallerData {
                gears,
                more_batches,
            })
            .map_err(|source| RegistryError::GrpcRegister {
                gear: hub_name,
                source,
            })
    }

    /// Late gRPC batch: the application group's services, installed by the
    /// running hub on its listener. Returns once the hub acknowledged them.
    async fn run_grpc_late_phase(
        &self,
        seen: &mut HashSet<&'static str>,
    ) -> Result<(), RegistryError> {
        if !self.grpc_in_use()? {
            return Ok(());
        }
        let hub_name = self.registry.grpc_hub.clone().unwrap_or_default();
        let gears = self.collect_grpc_installers(false, seen).await?;
        if gears.is_empty() {
            self.grpc_installers.close();
            return Ok(());
        }
        tracing::info!(gears = gears.len(), "Phase: grpc (late services)");
        let hub_runs = self.start_called.lock().contains(hub_name.as_str());
        let installed = self.grpc_installers.publish_late(gears).map_err(|source| {
            RegistryError::GrpcRegister {
                gear: hub_name.clone(),
                source,
            }
        })?;
        self.grpc_installers.close();
        if !hub_runs {
            // The hub runs in the application group: it takes the late batch
            // when it starts.
            return Ok(());
        }
        let ack = tokio::select! {
            () = self.cancel.cancelled() => return Err(RegistryError::Cancelled),
            ack = tokio::time::timeout(LATE_GRPC_INSTALL_TIMEOUT, installed) => ack,
        };
        match ack {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(source))) => Err(RegistryError::GrpcRegister {
                gear: hub_name,
                source,
            }),
            Ok(Err(_)) => Err(RegistryError::GrpcRegister {
                gear: hub_name,
                source: anyhow::anyhow!("the gRPC hub dropped the late services uninstalled"),
            }),
            Err(_) => Err(RegistryError::GrpcRegister {
                gear: hub_name,
                source: anyhow::anyhow!(
                    "the gRPC hub did not install the late services within {}s",
                    LATE_GRPC_INSTALL_TIMEOUT.as_secs()
                ),
            }),
        }
    }

    /// START phase over every gear, the system group first.
    #[cfg(test)]
    async fn run_start_phase(&self) -> Result<(), RegistryError> {
        self.run_start_phase_for(&self.by_group()).await
    }

    /// START phase: start the stateful gears of `entries` in order.
    async fn run_start_phase_for(&self, entries: &[&GearEntry]) -> Result<(), RegistryError> {
        tracing::info!(gears = entries.len(), "Phase: start");

        for e in entries {
            if let Some(s) = e.caps.query::<RunnableCap>() {
                tracing::debug!(
                    gear = e.name,
                    is_system = e.is_system(),
                    "Starting stateful gear"
                );
                self.start_called.lock().insert(e.name);
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

    /// STOP phase: stop all started gears: the application group, then the
    /// system group, each in reverse order.
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

        // The application group first: it may still use the system group.
        let in_reverse = self
            .registry
            .gears()
            .iter()
            .rev()
            .filter(|e| !e.is_system())
            .chain(self.registry.gears().iter().rev().filter(|e| e.is_system()));
        for e in in_reverse {
            let gear_name = e.name;
            // Only a gear whose `start` was called has anything to stop.
            if !self.start_called.lock().contains(gear_name) {
                continue;
            }

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

    /// Run the stop phase with a watchdog that force-exits the process if the
    /// stop phase hangs on a blocking syscall. The watchdog is disarmed whether
    /// the stop phase succeeds or fails, so a failing stop phase does not leave
    /// the watchdog active and eventually force-exit the process.
    async fn run_stop_phase_guarded(&self) -> Result<(), RegistryError> {
        let gear_count = u32::try_from(self.registry.gears().len().max(1)).unwrap_or(1);
        let stop_timeout = self
            .shutdown_deadline
            .checked_mul(gear_count)
            .and_then(|d| d.checked_add(std::time::Duration::from_secs(5)))
            .unwrap_or(self.shutdown_deadline);

        // Use a channel to arm/disarm the watchdog. If the lifecycle future is
        // dropped before the stop phase finishes (e.g. an outer timeout), the
        // sender is dropped and the watchdog exits without killing the process.
        let (disarm_tx, disarm_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            match disarm_rx.recv_timeout(stop_timeout) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // Stop phase completed, or the lifecycle future was cancelled.
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        timeout_secs = stop_timeout.as_secs(),
                        "shutdown: stop phase timed out, force exiting"
                    );
                    std::process::exit(1);
                }
            }
        });

        let stop_result = self.run_stop_phase().await;
        // Disarm the watchdog before propagating the stop-phase result. This runs
        // for both success and failure so a failing stop phase does not leave the
        // watchdog armed and eventually force-exit the process.
        let _ = disarm_tx.send(()).ok();

        stop_result
    }

    /// End a pending preparation, the runtime's own tasks and the gRPC
    /// batches, stop and join the spawned children, then
    /// stop every started gear. Runs after a failed startup as at shutdown;
    /// it never cancels the caller's token.
    async fn shut_down(&self) -> Result<(), RegistryError> {
        self.owned.cancel();
        self.grpc_installers.close();
        if let Some(oop) = &self.oop_options {
            oop.backend.shutdown_all().await;
        }
        self.run_stop_phase_guarded().await
    }

    /// `OoP` SPAWN phase: spawn every configured out-of-process gear once,
    /// as soon as the directory endpoint is bound. Their readiness is not
    /// awaited: a child may publish what this process's application group
    /// waits for.
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

            if hub.serves_no_endpoint() {
                tracing::debug!("gRPC hub binds no endpoint");
                return None;
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
    /// After the REST server has bound, advertise the in-process REST provider
    /// gears of `entries` in the service directory under their own gear name,
    /// pointing at the shared gateway endpoint. Consumers resolving a provider
    /// gear name then receive this endpoint and the gateway routes to the
    /// provider's handlers. The system gears are advertised before the
    /// application group is prepared, the application gears once it started.
    ///
    /// No-op when there is no REST host or no REST provider gears, so non-REST
    /// deployments and existing tests are unaffected. Registers through the
    /// `DirectoryClient` in the `ClientHub`, which uniformly targets the
    /// in-process directory (`LocalDirectoryClient`) or the central directory
    /// (`DirectoryGrpcClient` for `OoP`) depending on what the host wired.
    async fn run_directory_register_phase(
        &self,
        entries: &[&GearEntry],
    ) -> Result<(), RegistryError> {
        let rest_gears: Vec<&'static str> = self
            .rest_provider_gears()
            .into_iter()
            .filter(|gear| entries.iter().any(|e| e.name == *gear))
            .collect();
        if rest_gears.is_empty() {
            return Ok(());
        }

        let Some((_, host)) = self.rest_host() else {
            return Ok(()); // no REST host serving the routes
        };

        if !self.advertiser.is_enabled() {
            if !self.advertiser.first_attempt() {
                // An earlier stage found no endpoint or directory: the host
                // already started, neither appears later.
                return Ok(());
            }
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

            self.advertiser.enable(dir, endpoint);
        }
        for gear in rest_gears {
            self.advertiser.advertise(gear).await;
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
            .filter(|e| {
                e.caps
                    .query::<RestApiCap>()
                    .is_some_and(|rest| rest.advertised_in_directory())
                    && !e.caps.has::<ApiGatewayCap>()
            })
            .map(|e| e.name)
            .collect()
    }

    /// Deregister this process's REST providers from the directory on shutdown,
    /// so consumers stop resolving an endpoint that is going away. Best-effort.
    async fn deregister_rest_providers(&self) {
        self.advertiser.deregister_all().await;
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
    /// - `RunMode::MigrateOnly`: Executes only pre-init and DB migration phases
    ///   of every gear, then exits: no `init`, worker, listener, child or
    ///   publication
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

        // Migration-only: pre-init and migrations of every gear, then exit.
        if mode == RunMode::MigrateOnly {
            self.run_pre_init_phase().await?;
            // DB migration phase (the system group first)
            #[cfg(feature = "db")]
            self.run_db_phase().await?;
            tracing::info!("Migration phases completed successfully");
            return Ok(());
        }

        // A startup that fails leaves no started gear running:
        // gears started so far are stopped before the error returns.
        let outcome = match self.run_startup_phases().await {
            Ok(outcome) => outcome,
            Err(e) => {
                if let Err(stop) = self.shut_down().await {
                    tracing::warn!(error = %stop, "stop after a failed startup reported an error");
                }
                return Err(e);
            }
        };
        if outcome == AppStartup::Failed {
            tracing::error!(
                "the application group did not start; serving the system group only until shutdown"
            );
        }

        // Wait for cancellation
        self.cancel.cancelled().await;

        // Stop phase with hard timeout. Blocking stop implementations are
        // guarded by a watchdog thread so a hang cannot block shutdown,
        // whether in the in-process or OoP path.
        self.shut_down().await?;
        Ok(())
    }

    /// Draining watcher: the moment shutdown begins, flip readiness to
    /// Draining (`/readyz` reports 503 and the orchestrator drains the pod out
    /// of the load balancer) while in-flight requests finish. Both profiles
    /// start it first; it ends with the runtime's owned tasks.
    fn spawn_drain_watcher(&self) {
        let readiness = Arc::clone(&self.dep_checker);
        let cancel = self.cancel.clone();
        let owned = self.owned.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    readiness.set_draining(true);
                }
                () = owned.cancelled() => {}
            }
        });
    }

    /// Everything from the manifests to the started application group.
    async fn run_startup_phases(&self) -> anyhow::Result<AppStartup> {
        let system = self.group(true);
        let app = self.group(false);
        self.spawn_drain_watcher();

        // Local configuration first: a manifest that does not build, or a
        // client that would be wired before its provider, fails before any
        // gear runs or anything is written.
        let manifests = self.build_gts_manifests()?;
        let regs = Self::consumer_registrations();
        self.check_wiring_groups(&manifests, &regs)?;

        // System group, complete: pre-init, migrations, init, its clients,
        // post-init.
        self.run_pre_init_phase_for(&system).await?;
        #[cfg(feature = "db")]
        self.run_db_phase_for(&system).await?;
        self.run_init_phase_for(&system).await?;
        self.wire_system_clients(&regs)?;
        self.run_post_init_phase_for(&system).await?;

        // Initial routes, then start: the listeners serve probes and
        // platform-plane calls while the application group is prepared.
        let mut grpc_seen = HashSet::new();
        self.run_rest_bootstrap_phase(&system).await?;
        self.run_grpc_initial_phase(&mut grpc_seen).await?;
        self.run_start_phase_for(&system).await?;

        self.run_directory_register_phase(&system).await?;

        // Children spawn now, once: one of them may host the registry or
        // publish a base type the application group waits for.
        self.run_oop_spawn_phase().await?;

        // Application group: pre-init, migrations, the registry clients, then
        // the GTS barrier.
        self.run_pre_init_phase_for(&app).await?;
        #[cfg(feature = "db")]
        self.run_db_phase_for(&app).await?;
        self.wire_registry_clients(&manifests)?;
        match self.run_gts_publication_barrier(&manifests).await? {
            AppStartup::Started => {}
            other => {
                self.grpc_installers.close();
                return Ok(other);
            }
        }

        self.run_init_phase_for(&app).await?;
        self.wire_consumer_registrations(&regs)?;
        self.run_legacy_finalization().await?;
        self.run_post_init_phase_for(&app).await?;

        // Final routes on the running listeners, then start.
        let _router = self.run_rest_phase().await?;
        self.run_grpc_late_phase(&mut grpc_seen).await?;
        self.run_start_phase_for(&app).await?;
        self.run_directory_register_phase(&app).await?;
        self.dep_checker.mark_resolved(PREPARATION_GATE);
        Ok(AppStartup::Started)
    }
}

/// Advertises in-process REST providers in the directory, each at most once.
///
/// The directory-register phase enables it with the gateway endpoint, for the
/// system group and then for the application group. Only the in-process host path enables it: in `OoP` serving, `oop_serve` owns
/// presence + deregister, so nothing is advertised or deregistered here.
struct RestAdvertiser {
    instance_id: String,
    state: parking_lot::Mutex<AdvertiserState>,
}

#[derive(Default)]
struct AdvertiserState {
    target: Option<(Arc<dyn crate::DirectoryClient>, String)>,
    advertised: Vec<&'static str>,
    attempted: bool,
}

impl RestAdvertiser {
    fn new(instance_id: Uuid) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            state: parking_lot::Mutex::new(AdvertiserState::default()),
        }
    }

    fn enable(&self, dir: Arc<dyn crate::DirectoryClient>, endpoint: String) {
        self.state.lock().target = Some((dir, endpoint));
    }

    fn is_enabled(&self) -> bool {
        self.state.lock().target.is_some()
    }

    /// `true` the first time only: whether to look for an endpoint to enable.
    fn first_attempt(&self) -> bool {
        !std::mem::replace(&mut self.state.lock().attempted, true)
    }

    /// Advertise `gear`, unless not enabled yet or already advertised.
    async fn advertise(&self, gear: &'static str) {
        let (dir, endpoint) = {
            let mut state = self.state.lock();
            let Some((dir, endpoint)) = state.target.clone() else {
                return;
            };
            if state.advertised.contains(&gear) {
                return;
            }
            state.advertised.push(gear);
            (dir, endpoint)
        };
        let instance_id = &self.instance_id;
        // The directory keys instances by (gear, instance_id). grpc-hub
        // registers this same (gear, instance_id) with its gRPC services, in
        // the start phase or later on a retry; the store keeps them when this
        // registration carries none (`GearInstance::with_metadata_of`).
        // Carrying them forward also carries the version.
        //
        // Labels are deliberately NOT read-and-rewritten here. Carrying them
        // through would make this a cross-process read-modify-write with no
        // compare-and-set: any label change committed between the read and
        // the write would be silently reverted. Instead we register with an
        // empty label set, which `GearInstance::with_metadata_of` treats as
        // "preserve the stored labels" — an atomic no-op on labels.
        let (grpc_services, version) = match dir.list_instances(gear).await {
            Ok(insts) => insts
                .into_iter()
                .find(|i| &i.instance_id == instance_id)
                .map(|i| (i.grpc_services, i.version))
                .unwrap_or_default(),
            Err(e) => {
                // A failed directory read must not silently drop the
                // carried-forward metadata: log it, then fall back to an
                // empty augmentation so REST registration still proceeds.
                tracing::warn!(
                    gear,
                    error = %e,
                    "directory-register: failed to read existing registration; \
                     re-registering with empty grpc_services/version"
                );
                (Vec::new(), None)
            }
        };
        // OpenAPI spec is published separately (grpc-hub start phase); the
        // REST-augmentation registration does not carry it. Labels are
        // omitted so the store preserves the stored set (see above).
        let mut info = crate::RegisterInstanceInfo::new(gear.to_owned(), instance_id.clone())
            .with_grpc_services(grpc_services)
            .with_rest_endpoint(crate::ServiceEndpoint::new(endpoint.clone()));
        if let Some(version) = version {
            info = info.with_version(version);
        }
        match dir.register_instance(info).await {
            Ok(()) => {
                tracing::info!(gear, endpoint = %endpoint, "registered REST provider in directory");
            }
            Err(e) => {
                tracing::warn!(gear, error = %e, "directory-register: failed to register REST provider");
            }
        }
    }

    /// Deregister every advertised gear and disable further advertisement.
    async fn deregister_all(&self) {
        let (target, gears) = {
            let mut state = self.state.lock();
            (state.target.take(), std::mem::take(&mut state.advertised))
        };
        let Some((dir, _)) = target else {
            return;
        };
        for gear in gears {
            if let Err(e) = dir.deregister_instance(gear, &self.instance_id).await {
                tracing::warn!(gear, error = %e, "directory-deregister: failed to deregister REST provider");
            }
        }
    }
}

/// Composed `OoP` router, serialized `OpenAPI`, and auth policy from the same specs.
#[cfg(feature = "bootstrap")]
type ComposedOopRouter = (Router, String, toolkit_http_middleware::RouteAuthPolicy);

/// The host-less `OoP` router composed so far: each gear's routes are
/// registered once, the system gears' before the application group.
#[cfg(feature = "bootstrap")]
#[derive(Default)]
struct OopComposition {
    registry: crate::api::OpenApiRegistryImpl,
    router: Router,
    composed: HashSet<&'static str>,
}

/// Out-of-process HTTP serving lifecycle (`cpt-cf-component-oop-bootstrap`).
#[cfg(feature = "bootstrap")]
impl HostRuntime {
    /// Register the routes of the `RestApiCap` gears among `entries` not
    /// registered yet into the **host-less** `composition`, and return the
    /// router so far with its generated `OpenAPI` document (serialized JSON)
    /// and auth policy.
    ///
    /// Unlike [`run_rest_phase`](Self::run_rest_phase), this does not require an
    /// `ApiGatewayCap` host: `OoP` gears serve their own routes directly.
    async fn compose_oop_router(
        &self,
        options: &crate::runtime::OopServeOptions,
        hc_registry: &Arc<crate::healthcheck::RestHealthcheckRegistry>,
        entries: &[&GearEntry],
        composition: &mut OopComposition,
    ) -> anyhow::Result<ComposedOopRouter> {
        use crate::api::OpenApiInfo;
        use anyhow::Context as _;

        for entry in self.registry.gears() {
            if !entries.iter().any(|e| e.name == entry.name) {
                continue;
            }
            let Some(rest) = entry.caps.query::<RestApiCap>() else {
                continue;
            };
            if !composition.composed.insert(entry.name) {
                continue;
            }
            let ctx = self
                .ctx_builder
                .for_gear(entry.name)
                .await
                .with_context(|| format!("OoP router: build context for '{}'", entry.name))?;
            let router = std::mem::take(&mut composition.router);
            composition.router = rest
                .register_rest(&ctx, router, &composition.registry)
                .with_context(|| format!("OoP router: register_rest for '{}'", entry.name))?;

            // Register the gear's readiness healthcheck (the same mechanism
            // the api-gateway host uses), so /readyz reflects it identically
            // whether the gear runs in-process or OoP.
            if let Some(hc) = rest.healthcheck(&ctx) {
                hc_registry.register(entry.name, hc);
            }
        }

        let info = OpenApiInfo {
            title: options.gear_name.clone(),
            version: options
                .version
                .clone()
                .unwrap_or_else(|| "0.0.0".to_owned()),
            description: None,
            servers: vec![],
        };
        let openapi = composition
            .registry
            .build_openapi(&info)
            .context("OoP router: build OpenAPI document")?;
        let json = serde_json::to_string(&openapi).context("OoP router: serialize OpenAPI")?;
        let route_auth =
            super::oop_serve::route_auth_policy(composition.registry.operation_specs.iter());

        Ok((composition.router.clone(), json, route_auth))
    }

    /// Run the full `OoP` gear lifecycle: phases (`pre_init` … `start`), then
    /// serve the composed router with framework probes, background
    /// self-registration, dependency resolution, and graceful drain, then the
    /// `stop` phase.
    ///
    /// The listener binds first. The system group's routes attach as
    /// soon as it started — platform-plane calls only, tenant-plane calls get
    /// `503` — and the process is present in the directory then if its
    /// advertised gear is a system gear. The application group's routes
    /// attach on the same listener once it started.
    ///
    /// # Errors
    /// Returns an error if any lifecycle phase or the HTTP server fails.
    pub async fn run_oop_serving(
        self,
        options: crate::runtime::OopServeOptions,
    ) -> anyhow::Result<()> {
        use crate::runtime::ReadinessState;

        tracing::info!("Running OoP serving lifecycle");

        // Make the directory client reachable to the proxy-wiring phase, which
        // resolves remote `#[toolkit::consumes]` providers through the
        // `ClientHub`. The `OoP` presence loop uses `options.directory`
        // directly; consumer wiring reads it here.
        if self.client_hub.get::<dyn crate::DirectoryClient>().is_err() {
            self.client_hub
                .register::<dyn crate::DirectoryClient>(Arc::clone(&options.directory));
        }

        // Shared gear healthcheck registry (same mechanism as the api-gateway
        // host path). Seeded with the root cancellation token so in-flight
        // checks are aborted on shutdown. Populated during router composition.
        let hc_registry = Arc::new(
            crate::healthcheck::RestHealthcheckRegistry::with_cancellation(self.cancel.clone()),
        );

        // Readiness gates on `#[toolkit::consumes]` dependencies only — the same
        // policy as the in-process host path — via the shared `DependencyChecker`
        // fed by the proxy-wiring phase below. `deps = [...]`-only declarations
        // remain for topo-sort ordering but do NOT gate `/readyz`. The
        // healthcheck registry supplies the per-gear readiness dimension; both
        // feed the `/readyz` aggregate.
        let readiness = ReadinessState::from_checker(
            Arc::clone(&self.dep_checker),
            Arc::clone(&hc_registry),
            options.healthcheck_timeout,
        );

        // Bind the HTTP server and serve probes BEFORE the (possibly slow)
        // lifecycle phases, so the kubelet's liveness probe (`/healthz`) passes
        // immediately instead of getting connection-refused during `start()`.
        // Gear routes reply `503 starting` until attached below.
        let mut server = super::oop_serve::OopHttpServer::start(
            Arc::clone(&readiness),
            options,
            self.cancel.clone(),
        )
        .await?;

        self.spawn_drain_watcher();
        let mut composition = OopComposition::default();
        let outcome = self
            .run_oop_startup(&mut server, &hc_registry, &mut composition)
            .await;

        let serve_result = match outcome {
            Ok(outcome) => {
                if outcome == AppStartup::Failed {
                    tracing::error!(
                        "the application group did not start; serving the system group only \
                         until shutdown"
                    );
                }
                // Serve until cancelled, drain, then deregister.
                server.join().await
            }
            Err(e) => {
                tracing::error!(error = %e, "OoP startup failed before serving gear routes");
                // Tear down the probe server that is already bound.
                self.cancel.cancel();
                if let Err(join_err) = server.join().await {
                    tracing::warn!(error = %join_err, "OoP probe server teardown after startup failure errored");
                }
                Err(e)
            }
        };

        // Served or failed to start: the runtime's tasks end and gears stop. Errors are
        // logged but do not fail the shutdown process (same contract as
        // run_stop_phase).
        if let Err(e) = self.shut_down().await {
            tracing::warn!(error = %e, "OoP stop phase reported an error");
        }

        serve_result
    }

    /// The `OoP` phases from the manifests to the attached application routes.
    async fn run_oop_startup(
        &self,
        server: &mut super::oop_serve::OopHttpServer,
        hc_registry: &Arc<crate::healthcheck::RestHealthcheckRegistry>,
        composition: &mut OopComposition,
    ) -> anyhow::Result<AppStartup> {
        let system = self.group(true);
        let app = self.group(false);
        let advertised = server.options().gear_name.clone();
        let advertised_is_system = system.iter().any(|e| e.name == advertised.as_str());

        let manifests = self.build_gts_manifests()?;
        let regs = Self::consumer_registrations();
        self.check_wiring_groups(&manifests, &regs)?;

        // System group, complete before the application group.
        self.run_pre_init_phase_for(&system).await?;
        #[cfg(feature = "db")]
        self.run_db_phase_for(&system).await?;
        self.run_init_phase_for(&system).await?;
        self.wire_system_clients(&regs)?;
        self.run_post_init_phase_for(&system).await?;
        let mut grpc_seen = HashSet::new();
        self.run_grpc_initial_phase(&mut grpc_seen).await?;
        self.run_start_phase_for(&system).await?;

        // System routes on the bound listener: platform-plane calls
        // only until the application group attached its routes.
        if system.iter().any(|e| e.caps.has::<RestApiCap>()) {
            let (router, openapi, route_auth) = self
                .compose_oop_router(server.options(), hc_registry, &system, composition)
                .await?;
            server.publish_bootstrap_routes(router, openapi, route_auth);
            if advertised_is_system {
                server.start_presence();
            }
            // Every co-hosted system provider is discoverable at once,
            // whatever the root gear: an external publisher must reach a
            // registry the application group here may be waiting for.
            for entry in &system {
                if entry.name != advertised.as_str()
                    && entry
                        .caps
                        .query::<RestApiCap>()
                        .is_some_and(|rest| rest.advertised_in_directory())
                {
                    server.start_provider_presence(entry.name.to_owned());
                }
            }
        }

        // Application group: pre-init, migrations, the registry clients, then
        // the GTS barrier.
        self.run_pre_init_phase_for(&app).await?;
        #[cfg(feature = "db")]
        self.run_db_phase_for(&app).await?;
        self.wire_registry_clients(&manifests)?;
        match self.run_gts_publication_barrier(&manifests).await? {
            AppStartup::Started => {}
            other => {
                self.grpc_installers.close();
                return Ok(other);
            }
        }
        self.run_init_phase_for(&app).await?;
        // Proxy-wiring before start: a gear resolving a consumed contract
        // during `start` finds the client in the hub under both profiles.
        self.wire_consumer_registrations(&regs)?;
        self.run_legacy_finalization().await?;
        self.run_post_init_phase_for(&app).await?;
        self.run_grpc_late_phase(&mut grpc_seen).await?;
        self.run_start_phase_for(&app).await?;

        // The gear lifecycle has populated the ClientHub. If an in-process
        // authn stack (e.g. a linked authn-resolver gear) registered a
        // DynBearerAuthenticator bridge, install the tenant plane now —
        // before the final routes layer security_context_middleware. (The
        // platform plane is built eagerly at bootstrap from
        // `oop_http.internal_auth`.)
        server.resolve_bearer_authenticator(&self.client_hub);
        let all: Vec<&GearEntry> = self.registry.gears().iter().collect();
        let (router, openapi, route_auth) = self
            .compose_oop_router(server.options(), hc_registry, &all, composition)
            .await?;
        // Publish the full routes (they go live), then start directory
        // presence of the advertised gear: the directory never advertises it
        // while its routes reply 503.
        server.attach(router, openapi, route_auth)?;
        if !advertised_is_system {
            server.start_presence();
        }
        self.dep_checker.mark_resolved(PREPARATION_GATE);
        Ok(AppStartup::Started)
    }
}

/// Read `gears.<owner>.config.consumer_wiring.<dep>` as a [`ConsumerWiring`].
///
/// `Ok(None)` when absent (the dep keeps discovery). A *present* entry that cannot
/// be deserialized is an `Err` that aborts proxy-wiring — the same fail-at-boot
/// contract the provider side uses for `client_wiring` ([`crate::wiring::read_wiring`]).
fn read_consumer_wiring(
    cfg: &dyn ConfigProvider,
    owner_gear: &str,
    dep_gear: &str,
) -> Result<Option<toolkit_contract::wiring::ConsumerWiring>, anyhow::Error> {
    let Some(value) = cfg
        .get_gear_config(owner_gear)
        .and_then(|c| c.get("config"))
        .and_then(|c| c.get("consumer_wiring"))
        .and_then(|c| c.get(dep_gear))
    else {
        return Ok(None);
    };
    // A bare string is the legacy static-endpoint shape; the raw serde error
    // ("invalid type: string") would not hint at the fix, so name the object form.
    if let Some(s) = value.as_str() {
        anyhow::bail!(
            "gear `{owner_gear}`: `consumer_wiring.{dep_gear}` is a bare string; the \
             static-endpoint override now lives on the object form — write it as \
             `{{ \"endpoint\": \"{s}\" }}`"
        );
    }
    serde_json::from_value::<toolkit_contract::wiring::ConsumerWiring>(value.clone())
        .map(Some)
        .map_err(|e| {
            anyhow::anyhow!("gear `{owner_gear}`: invalid consumer_wiring.{dep_gear}: {e}")
        })
}

#[cfg(test)]
#[cfg(feature = "bootstrap")]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "host_runtime_oop_tests.rs"]
mod host_runtime_oop_tests;

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
        runtime.run_start_phase().await.unwrap();
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
        runtime.run_start_phase().await.unwrap();
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

    /// Serves one config blob for owner gear `orders` (the owner every
    /// `read_consumer_wiring` test uses) and nothing for anything else.
    struct OrdersCfg(serde_json::Value);
    impl ConfigProvider for OrdersCfg {
        fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
            (gear == "orders").then_some(&self.0)
        }
    }

    /// Serves config blobs by owner-gear name (for tests wiring several gears).
    struct MapCfg(std::collections::HashMap<String, serde_json::Value>);
    impl ConfigProvider for MapCfg {
        fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
            self.0.get(gear)
        }
    }

    /// Extract just the static-endpoint override from a `consumer_wiring` entry.
    fn endpoint_override(cfg: &dyn ConfigProvider, owner: &str, dep: &str) -> Option<String> {
        super::read_consumer_wiring(cfg, owner, dep)
            .expect("consumer_wiring must not error for a valid/absent endpoint")
            .and_then(|w| w.into_parts().0)
    }

    #[test]
    fn static_endpoint_override_reads_nested_consumer_wiring_key() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "orders".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": { "billing": { "endpoint": "http://localhost:8081" } }
                }
            }),
        );
        let cfg = MapCfg(map);

        // Present override is read from `config.consumer_wiring.<dep>.endpoint`.
        assert_eq!(
            endpoint_override(&cfg, "orders", "billing").as_deref(),
            Some("http://localhost:8081")
        );
        // Absent dep / owner → None (falls through to directory resolution).
        assert_eq!(endpoint_override(&cfg, "orders", "inventory"), None);
        assert_eq!(endpoint_override(&cfg, "warehouse", "billing"), None);
        assert_eq!(
            endpoint_override(&EmptyConfigProvider, "orders", "billing"),
            None
        );
    }

    /// The object form yields both the `endpoint` override and the flattened
    /// `ClientTuning`.
    #[test]
    fn consumer_wiring_object_form_yields_endpoint_and_tuning() {
        let cfg = OrdersCfg(serde_json::json!({
            "config": {
                "consumer_wiring": {
                    "billing": {
                        "endpoint": "http://billing:8080",
                        "timeout": "5s",
                        "max_concurrent_requests": 256,
                        "pool_max_idle_per_host": 256
                    }
                }
            }
        }));

        let (endpoint, tuning) = super::read_consumer_wiring(&cfg, "orders", "billing")
            .expect("wiring must parse")
            .expect("wiring present")
            .into_parts();
        assert_eq!(endpoint.as_deref(), Some("http://billing:8080"));
        assert_eq!(tuning.timeout, Some(std::time::Duration::from_secs(5)));
        assert_eq!(tuning.max_concurrent_requests, Some(256));
        assert_eq!(tuning.pool_max_idle_per_host, Some(256));
    }

    /// Omitting `endpoint` keeps discovery while still applying the tuning.
    #[test]
    fn consumer_wiring_object_without_endpoint_keeps_discovery() {
        let cfg = OrdersCfg(serde_json::json!({
            "config": {
                "consumer_wiring": { "billing": { "max_concurrent_requests": 1 } }
            }
        }));

        let (endpoint, tuning) = super::read_consumer_wiring(&cfg, "orders", "billing")
            .expect("wiring must parse")
            .expect("wiring present")
            .into_parts();
        assert_eq!(endpoint, None, "omitted endpoint must keep discovery");
        assert_eq!(tuning.max_concurrent_requests, Some(1));
    }

    /// A present-but-unparseable entry (wrong shape or wrong-typed tuning) is a
    /// boot error, matching the provider side's `client_wiring` — not a silent
    /// downgrade to discovery.
    #[test]
    fn consumer_wiring_malformed_entry_is_an_error() {
        let malformed = [
            // Not a ConsumerWiring object at all.
            serde_json::json!([1, 2, 3]),
            // A wrong-typed tuning value: a string where a count is expected, and
            // a bare number where a humantime duration string is expected.
            serde_json::json!({ "max_concurrent_requests": "lots" }),
            serde_json::json!({ "timeout": 5 }),
            // The same, even alongside an otherwise-valid endpoint override: the
            // whole entry fails, so the override is not silently honoured untuned.
            serde_json::json!({ "endpoint": "http://billing:8080", "timeout": 5 }),
        ];
        for entry in malformed {
            let cfg = OrdersCfg(serde_json::json!({
                "config": { "consumer_wiring": { "billing": entry.clone() } }
            }));
            assert!(
                super::read_consumer_wiring(&cfg, "orders", "billing").is_err(),
                "a malformed entry (`{entry}`) must be a boot error, not silently dropped"
            );
        }
    }

    /// A legacy bare-string value is a hard error whose message names the object
    /// form (a migration hint, not an opaque serde "invalid type" error).
    #[test]
    fn consumer_wiring_legacy_bare_string_is_an_error_naming_the_object_form() {
        let cfg = OrdersCfg(serde_json::json!({
            "config": { "consumer_wiring": { "billing": "http://localhost:8081" } }
        }));
        let err = super::read_consumer_wiring(&cfg, "orders", "billing")
            .expect_err("a bare-string value is the legacy shape -> hard error")
            .to_string();
        assert!(
            err.contains("endpoint"),
            "the error must point the operator at the `{{ endpoint: ... }}` object form, got: {err}"
        );
    }

    /// A present-but-invalid `endpoint` is surfaced as an error, not downgraded to
    /// discovery — the operator asked for a specific endpoint that can't be honoured.
    #[test]
    fn consumer_wiring_invalid_endpoint_is_an_error() {
        for bad in ["", "billing", "/path/only"] {
            let cfg = OrdersCfg(serde_json::json!({
                "config": { "consumer_wiring": { "billing": { "endpoint": bad } } }
            }));
            assert!(
                super::read_consumer_wiring(&cfg, "orders", "billing").is_err(),
                "a present-but-invalid endpoint (`{bad}`) must be a hard error, not survived"
            );
        }
    }

    /// One record of what a test wired the phase with. The phase tests share this
    /// collector and run in parallel, so each finds its own entry by a unique
    /// tuning value.
    #[derive(Clone)]
    struct WiredCall {
        resolver: Arc<dyn crate::discovery::EndpointResolver>,
        max_concurrent_requests: Option<usize>,
        /// A dropped credential is otherwise invisible — the only runtime symptom
        /// is a missing `X-ToolKit-Internal-Token` on every Profile 2/3 call.
        has_credential: bool,
    }
    static WIRED: std::sync::Mutex<Vec<WiredCall>> = std::sync::Mutex::new(Vec::new());

    /// Stands in for the `#[toolkit::consumes]`-generated wire fn: records the
    /// resolver and tuning it was handed, then reports a remote binding.
    #[allow(
        clippy::unnecessary_wraps,
        clippy::needless_pass_by_value,
        reason = "must match the ConsumerRegistration::wire signature the macro emits"
    )]
    fn record_wire(
        _hub: &ClientHub,
        resolver: Arc<dyn crate::discovery::EndpointResolver>,
        tuning: toolkit_contract::wiring::ClientTuning,
    ) -> anyhow::Result<crate::discovery::WireOutcome> {
        WIRED.lock().unwrap().push(WiredCall {
            resolver,
            max_concurrent_requests: tuning.max_concurrent_requests,
            has_credential: tuning.internal_token_provider.is_some(),
        });
        Ok(crate::discovery::WireOutcome::Remote)
    }

    /// A consumer registration for the phase tests, wired directly so it never
    /// enters the link-time-global `inventory` set (which would leak onto other
    /// tests in this binary).
    fn test_registration() -> crate::discovery::ConsumerRegistration {
        crate::discovery::ConsumerRegistration {
            owner_gear: "orders",
            dep_gear: "billing",
            wire: record_wire,
        }
    }

    /// Static-endpoint arm: an `endpoint` wires a `StaticEndpointResolver` pinned
    /// to it, threads the entry's tuning through, and marks the dep resolved
    /// immediately (no probe loop).
    #[tokio::test]
    async fn proxy_wiring_static_override_uses_static_resolver_and_marks_ready() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "orders".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": {
                        "billing": {
                            "endpoint": "http://localhost:8081",
                            "max_concurrent_requests": 7
                        }
                    }
                }
            }),
        );

        let registry = RegistryBuilder::default().build_topo_sorted().unwrap();
        let runtime = HostRuntime::new(
            registry,
            Arc::new(MapCfg(map)),
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        let reg = test_registration();
        runtime.wire_consumer_registrations(&[&reg]).unwrap();

        // Find *the* entry whose resolver yields the configured endpoint and
        // assert that same entry's tuning — OR-ing two flags independently across
        // every WIRED entry would pass even if the endpoint and the tuning had
        // arrived on two different wire calls.
        let calls: Vec<WiredCall> = WIRED.lock().unwrap().clone();
        let mut matched = None;
        for call in &calls {
            if let Ok(Some(ep)) = call.resolver.resolve_endpoint("billing").await
                && ep == "http://localhost:8081"
            {
                matched = Some(call.clone());
                break;
            }
        }
        let matched = matched
            .expect("static override must wire a resolver pinned to the configured endpoint");
        assert_eq!(
            matched.max_concurrent_requests,
            Some(7),
            "the tuning on the same wire call as the static endpoint must be the configured one"
        );

        // A static endpoint is resolvable by construction: the dep must be
        // resolved immediately rather than left gating /readyz.
        assert_eq!(
            runtime.dep_checker.unresolved_deps(),
            [crate::runtime::PREPARATION_GATE],
            "only the not yet started application group gates readiness"
        );
    }

    /// An in-process provider that forgot to register its local impl must fail
    /// startup rather than silently wiring a self-pointing proxy: here both the
    /// consumer `orders` and provider `billing` are registered gears, but the
    /// consumer's wiring reports `Remote`.
    #[tokio::test]
    async fn proxy_wiring_in_process_provider_wired_remote_is_a_hard_error() {
        // Register the consumer `orders` and the provider `billing`, where billing
        // never provides its client (no local impl, no REST).
        let mut b = crate::registry::RegistryBuilder::default();
        b.register_core_with_meta("orders", &[], Arc::new(DummyCore));
        b.register_core_with_meta("billing", &[], Arc::new(DummyCore));
        let registry = b.build_topo_sorted().unwrap();

        let runtime = HostRuntime::new(
            registry,
            Arc::new(MapCfg(std::collections::HashMap::new())),
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        // `test_registration()` is owner=orders, dep=billing, and `record_wire`
        // reports `Remote` without registering a local impl — the bug scenario.
        let reg = test_registration();
        let err = runtime
            .wire_consumer_registrations(&[&reg])
            .expect_err("an in-process provider wired Remote must fail startup");
        let msg = err.to_string();
        assert!(
            matches!(err, RegistryError::ProxyWiring { gear, .. } if gear == "orders"),
            "expected a ProxyWiring error for the consumer gear, got: {msg}"
        );
        // Display must surface the source, so both the consumer and provider gear
        // names appear in the rendered message.
        assert!(
            msg.contains("orders") && msg.contains("billing"),
            "displayed error must name both consumer and provider, got: {msg}"
        );
    }

    /// Tuning-only entry (no `endpoint`, the production shape): keeps the shared
    /// discovery resolver and leaves the dep gating readiness — a remote dep with
    /// no directory stays unresolved.
    #[tokio::test]
    async fn proxy_wiring_tuning_only_entry_keeps_discovery_and_gates_readiness() {
        let mut map = std::collections::HashMap::new();
        // A unique tuning value so this test can find its own WIRED entry among
        // the link-time-global registrations.
        map.insert(
            "orders".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": { "billing": { "max_concurrent_requests": 4343 } }
                }
            }),
        );

        let registry = RegistryBuilder::default().build_topo_sorted().unwrap();
        let runtime = HostRuntime::new(
            registry,
            Arc::new(MapCfg(map)),
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        let reg = test_registration();
        runtime.wire_consumer_registrations(&[&reg]).unwrap();

        let calls: Vec<WiredCall> = WIRED.lock().unwrap().clone();
        let ours = calls
            .iter()
            .find(|c| c.max_concurrent_requests == Some(4343))
            .expect("our tuning-only wire call must have been recorded");
        assert!(
            matches!(ours.resolver.resolve_endpoint("billing").await, Ok(None)),
            "a tuning-only entry must keep discovery -- the resolver must not be pinned to a \
             static endpoint"
        );

        // No directory + a remote dep → it must stay unresolved and gate /readyz.
        assert!(!runtime.dep_checker.all_resolved());
        assert!(
            runtime
                .dep_checker
                .unresolved_deps()
                .contains(&"billing".to_owned()),
            "a discovery-wired dep with no directory must gate readiness"
        );
    }

    /// The process's platform-plane credential must ride on the tuning handed to
    /// the wire fn (Profile 2/3), else every inter-gear call ships without
    /// `X-ToolKit-Internal-Token`.
    #[tokio::test]
    async fn proxy_wiring_threads_platform_credential_onto_tuning() {
        use toolkit_contract::runtime::config::{CredentialState, InternalTokenProvider};

        let mut map = std::collections::HashMap::new();
        map.insert(
            "orders".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": { "billing": { "max_concurrent_requests": 4242 } }
                }
            }),
        );

        let registry = RegistryBuilder::default().build_topo_sorted().unwrap();
        let provider = InternalTokenProvider::new(|| CredentialState::NotConfigured);
        let runtime = HostRuntime::new(
            registry,
            Arc::new(MapCfg(map)),
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        )
        .with_internal_token_provider(Some(provider));

        let reg = test_registration();
        runtime.wire_consumer_registrations(&[&reg]).unwrap();

        let calls: Vec<WiredCall> = WIRED.lock().unwrap().clone();
        let ours = calls
            .iter()
            .find(|c| c.max_concurrent_requests == Some(4242))
            .expect("our wire call must have been recorded");
        assert!(
            ours.has_credential,
            "the process credential must be threaded onto the tuning handed to the wire fn"
        );
    }

    /// Phase-level: a non-absolute `endpoint` aborts proxy-wiring with
    /// `RegistryError::ProxyWiring` rather than silently downgrading to discovery
    /// (which would mark the dep resolved while every call fails).
    #[tokio::test]
    async fn proxy_wiring_invalid_endpoint_is_a_hard_error() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "orders".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": {
                        "billing": { "endpoint": "", "max_concurrent_requests": 4444 }
                    }
                }
            }),
        );

        let registry = RegistryBuilder::default().build_topo_sorted().unwrap();
        let runtime = HostRuntime::new(
            registry,
            Arc::new(MapCfg(map)),
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        let reg = test_registration();
        let err = runtime
            .wire_consumer_registrations(&[&reg])
            .expect_err("an invalid static endpoint must abort proxy-wiring");
        assert!(
            matches!(err, RegistryError::ProxyWiring { gear, .. } if gear == "orders"),
            "expected a ProxyWiring error for the owning gear, got {err:?}"
        );
    }

    /// The override is keyed by the kebab *gear name* (`owner_gear`), not the
    /// struct ident. Regression guard: the macro once emitted
    /// `stringify!(StructIdent)`, so the lookup could never fire.
    #[test]
    fn static_endpoint_override_is_keyed_by_kebab_gear_name() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "api-contracts-consumer".to_owned(),
            serde_json::json!({
                "config": {
                    "consumer_wiring": {
                        "api-contracts": { "endpoint": "http://localhost:9099" }
                    }
                }
            }),
        );
        let cfg = MapCfg(map);

        assert_eq!(
            endpoint_override(&cfg, "api-contracts-consumer", "api-contracts").as_deref(),
            Some("http://localhost:9099"),
        );
        // The pre-fix value — the Rust struct ident — must NOT resolve.
        assert_eq!(
            endpoint_override(&cfg, "ApiContractsConsumer", "api-contracts"),
            None,
        );
    }

    #[tokio::test]
    async fn test_post_init_runs_after_all_init_for_every_gear_system_group_first() {
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

            async fn post_init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                self.events
                    .lock()
                    .await
                    .push(format!("post_init:{}", self.name));
                Ok(())
            }
        }

        impl SystemCapability for TrackHooks {}

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

        // The system group first, each group in topological order.
        assert_eq!(
            events,
            vec![
                "init:sys_a",
                "init:user_b",
                "init:user_c",
                "post_init:sys_a",
                "post_init:user_b",
                "post_init:user_c",
            ]
        );
    }

    /// The in-process and `OoP` paths must agree on where proxy-wiring sits.
    ///
    /// They did not: the `OoP` path ran it *after* `start`, so a gear resolving
    /// a consumed contract during its own `start` found nothing in the
    /// `ClientHub` under Profile 2/3 while working under Profile 1. Both paths
    /// now go through `run_init_wiring_post_init`, which pins the order.
    ///
    /// This test covers the two observable endpoints of that segment. The
    /// wiring step between them is a no-op here (no `#[toolkit::consumes]`
    /// registration is linked into this test binary), so what actually stops
    /// the paths diverging again is the shared method itself — inlining it back
    /// into both call sites makes it dead code and fails the `-D warnings`
    /// build.
    #[tokio::test]
    async fn init_wiring_post_init_runs_as_one_ordered_segment() {
        #[derive(Clone)]
        struct TrackHooks {
            events: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl Gear for TrackHooks {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                self.events.lock().await.push("init".to_owned());
                Ok(())
            }

            async fn post_init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                self.events.lock().await.push("post_init".to_owned());
                Ok(())
            }
        }

        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let gear = Arc::new(TrackHooks {
            events: events.clone(),
        });

        let mut builder = RegistryBuilder::default();
        builder.register_core_with_meta("app", &[], gear.clone() as Arc<dyn Gear>);
        let registry = builder.build_topo_sorted().unwrap();

        let runtime = HostRuntime::new(
            registry,
            Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        runtime.run_init_wiring_post_init().await.unwrap();

        assert_eq!(events.lock().await.clone(), vec!["init", "post_init"]);
    }

    /// A REST gear opting out of directory advertisement is served but not
    /// advertised (nor deregistered) as a REST provider.
    #[test]
    fn a_rest_gear_can_opt_out_of_directory_advertisement() {
        struct Rest {
            advertised: bool,
        }
        #[async_trait::async_trait]
        impl Gear for Rest {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                Ok(())
            }
        }
        impl crate::contracts::RestApiCapability for Rest {
            fn register_rest(
                &self,
                _ctx: &GearCtx,
                router: axum::Router,
                _openapi: &dyn crate::api::OpenApiRegistry,
            ) -> anyhow::Result<axum::Router> {
                Ok(router)
            }
            fn advertised_in_directory(&self) -> bool {
                self.advertised
            }
        }

        let mut builder = RegistryBuilder::default();
        for (name, advertised) in [("advertised", true), ("hidden", false)] {
            let gear = Arc::new(Rest { advertised });
            builder.register_core_with_meta(name, &[], gear.clone() as Arc<dyn Gear>);
            builder.register_rest_with_meta(
                name,
                gear as Arc<dyn crate::contracts::RestApiCapability>,
            );
        }
        let runtime = HostRuntime::new(
            builder.build_topo_sorted().unwrap(),
            Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
            DbOptions::None,
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
            Uuid::new_v4(),
            None,
        );

        assert_eq!(runtime.rest_provider_gears(), vec!["advertised"]);
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
        runtime.run_start_phase().await.unwrap();
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

        runtime.run_start_phase().await.unwrap();
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

        runtime.run_start_phase().await.unwrap();
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "host_runtime_preparation_tests.rs"]
mod host_runtime_preparation_tests;
