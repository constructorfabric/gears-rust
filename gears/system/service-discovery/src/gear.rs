//! Gear definition for `ServiceDiscoveryGear`

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use toolkit::context::GearCtx;
use toolkit::contracts::{
    GrpcServiceCapability, OpenApiRegistry, RegisterGrpcServiceFn, RestApiCapability,
    RunnableCapability, SystemCapability,
};
use toolkit::registry::GearRegistry;
use toolkit::runtime::GearManager;
use toolkit::{DirectoryClient, Healthcheck, HealthcheckResult};

use cf_system_sdks::directory::DIRECTORY_SERVICE_NAME;
use cf_system_sdks::directory::labels::is_valid_label_segment;

use crate::cache_directory_backend::{StoragePluginHandle, build_selected_backend};
use crate::config::ServiceDiscoveryConfig;
use crate::directory_backend::{
    DirectoryBackend, DirectoryBackendError, DirectoryChange, DirectoryChangeStream,
};
use crate::directory_client::ReplicatedDirectoryClient;
use crate::domain::authz::RegistrationPolicy;
use crate::domain::service::GearsService;
use crate::local_directory_client::LocalDirectoryClient;
use crate::replica::DirectoryReplica;
use crate::server;

/// Service Discovery - system gear for service discovery
///
/// This gear:
/// - Provides `DirectoryClient` to the `ClientHub` for in-process gears
/// - Exposes `DirectoryService` gRPC service via `grpc-hub`
/// - Tracks gear instances and provides service resolution
/// - Exposes REST API to list all registered gears
// `one_per_installation`: only one application owns the logical service
// directory. Replicas of that application may share a configured backend, but
// a second application would create a separate directory and discovery
// authority. Composition tooling refuses that duplicate; this gear cannot
// detect it from its local read model.
#[toolkit::gear(
    name = "service-discovery",
    capabilities = [grpc, system, rest, stateful],
    client = cf_system_sdks::directory::DirectoryClient,
    one_per_installation = true
)]
pub struct ServiceDiscoveryGear {
    directory_api: OnceLock<Arc<dyn DirectoryClient>>,
    directory_backend: OnceLock<Arc<dyn DirectoryBackend>>,
    storage_plugin: tokio::sync::Mutex<Option<StoragePluginHandle>>,
    /// Read model plus backend versions, with a directory backend.
    replica: OnceLock<Arc<DirectoryReplica>>,
    /// Set once the backend snapshot has first loaded; gates `/readyz`.
    /// Never cleared: see [`SnapshotLoaded`].
    snapshot_loaded: Arc<AtomicBool>,
    gear_manager: OnceLock<Arc<GearManager>>,
    gears_service: OnceLock<Arc<GearsService>>,
    policy: OnceLock<RegistrationPolicy>,
    /// How often the lease sweeper reclaims records whose lease expired
    /// past grace. Set from the configured `lease.grace` in `init`.
    sweep_interval: OnceLock<Duration>,
    #[cfg(all(test, feature = "integration"))]
    backend_janitor_interval: OnceLock<Duration>,
}

impl Default for ServiceDiscoveryGear {
    fn default() -> Self {
        Self {
            directory_api: OnceLock::new(),
            directory_backend: OnceLock::new(),
            storage_plugin: tokio::sync::Mutex::new(None),
            replica: OnceLock::new(),
            snapshot_loaded: Arc::new(AtomicBool::new(false)),
            gear_manager: OnceLock::new(),
            gears_service: OnceLock::new(),
            policy: OnceLock::new(),
            sweep_interval: OnceLock::new(),
            #[cfg(all(test, feature = "integration"))]
            backend_janitor_interval: OnceLock::new(),
        }
    }
}

impl ServiceDiscoveryGear {
    /// Install a shared directory backend before `init`. Test-only until the
    /// shared-storage backend (#5875) is selected from configuration.
    ///
    /// # Errors
    /// Fails if a backend is already installed or `init` has already run.
    #[cfg(test)]
    pub(crate) fn install_directory_backend(
        &self,
        backend: Arc<dyn DirectoryBackend>,
    ) -> anyhow::Result<()> {
        if self.directory_api.get().is_some() {
            anyhow::bail!("DirectoryBackend must be installed before init");
        }
        self.directory_backend
            .set(backend)
            .map_err(|_| anyhow::anyhow!("DirectoryBackend already installed"))
    }

    #[cfg(all(test, feature = "integration"))]
    fn set_backend_janitor_interval_for_test(&self, interval: Duration) -> anyhow::Result<()> {
        self.backend_janitor_interval
            .set(interval)
            .map_err(|_| anyhow::anyhow!("backend janitor interval already set"))
    }
}

#[async_trait]
impl SystemCapability for ServiceDiscoveryGear {
    fn wire_runtime(&self, sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        self.gear_manager
            .set(Arc::clone(&sys.gear_manager))
            .map_err(|_| anyhow::anyhow!("GearManager already set (wire_runtime called twice?)"))?;
        Ok(())
    }
}

#[async_trait]
impl toolkit::Gear for ServiceDiscoveryGear {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        // Read the (optional) authorization config: which peers may act on any
        // gear's registration, plus the namespace / trust-domain allowlists.
        // Absent config yields empty sets, i.e. every gear may act only on its
        // own name. Whether a *token-less* request is rejected is not configured
        // here: it is decided per-request from the `PlatformAuthEnforced` marker
        // the enforcement layer stamps (see `DirectoryServiceImpl`), so it stays
        // in lockstep with the listener without a drift-prone knob.
        let cfg: ServiceDiscoveryConfig = ctx.config_or_default()?;
        // Fail fast on a lease policy clients cannot renew in time, rather than
        // let workers silently flap in production.
        cfg.lease
            .validate()
            .context("invalid service-discovery lease configuration")?;
        self.policy
            .set(RegistrationPolicy {
                trusted_registrars: cfg.trusted_registrars.into_iter().collect(),
                platform_namespaces: cfg.platform_namespaces.into_iter().collect(),
                trust_domains: cfg.trust_domains.into_iter().collect(),
            })
            .map_err(|_| anyhow::anyhow!("registration policy already set (init called twice?)"))?;

        // Build the compiled-gear catalog for the gears service.
        let registry =
            GearRegistry::discover_and_build().context("failed to build gear registry")?;

        // Use the injected GearManager to create the DirectoryClient
        let manager =
            self.gear_manager.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("GearManager not wired into ServiceDiscoveryGear")
            })?;

        // Apply the configured instance-lease policy onto the
        // host-owned manager, and record the grace as the sweeper cadence.
        manager.set_lease_policy(cfg.lease.ttl, cfg.lease.grace);
        self.sweep_interval
            .set(cfg.lease.grace)
            .map_err(|_| anyhow::anyhow!("sweep interval already set (init called twice?)"))?;

        // Pin gRPC-service-name ownership from config so a name can only be
        // advertised by its configured owner, regardless of registration order.
        // Validate at load: a key that fails `register_instance`'s
        // `is_valid_label_segment` screen (or an empty owner) could never match a
        // registration, so it would silently pin nothing — fail loudly instead.
        for (service_name, owner) in &cfg.grpc_service_owners {
            if !is_valid_label_segment(service_name) {
                anyhow::bail!(
                    "grpc_service_owners: '{service_name}' is not a valid gRPC service name; \
                     it could never match a registration"
                );
            }
            if owner.trim().is_empty() {
                anyhow::bail!(
                    "grpc_service_owners['{service_name}']: owner gear name must not be empty"
                );
            }
        }
        manager.set_grpc_service_owners(cfg.grpc_service_owners);

        if let Some((backend, handle)) =
            build_selected_backend(cfg.storage, manager.lease_grace()).await?
        {
            if self.directory_backend.get().is_some() {
                handle.stop().await;
                anyhow::bail!(
                    "a DirectoryBackend was injected while a shared storage backend is configured"
                );
            }
            self.directory_backend
                .set(backend)
                .map_err(|_| anyhow::anyhow!("DirectoryBackend already set"))?;
            *self.storage_plugin.lock().await = Some(handle);
        }

        let api_impl: Arc<dyn DirectoryClient> = match self.directory_backend.get() {
            Some(backend) => {
                let replica = Arc::new(DirectoryReplica::new(Arc::clone(&manager)));
                self.replica.set(Arc::clone(&replica)).map_err(|_| {
                    anyhow::anyhow!("DirectoryReplica already set (init called twice?)")
                })?;
                Arc::new(ReplicatedDirectoryClient::new(replica, Arc::clone(backend)))
            }
            None => Arc::new(LocalDirectoryClient::new(Arc::clone(&manager))),
        };

        // Register in ClientHub directly
        ctx.client_hub()
            .register::<dyn DirectoryClient>(api_impl.clone());

        self.directory_api
            .set(api_impl)
            .map_err(|_| anyhow::anyhow!("DirectoryClient already set (init called twice?)"))?;

        // Create the GearsService from the catalog built above.
        let gears_service = Arc::new(GearsService::new(&registry, manager));
        self.gears_service
            .set(gears_service)
            .map_err(|_| anyhow::anyhow!("GearsService already set (init called twice?)"))?;

        tracing::info!("ServiceDiscoveryGear initialized");

        Ok(())
    }
}

impl RestApiCapability for ServiceDiscoveryGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        let service = Arc::clone(
            self.gears_service
                .get()
                .ok_or_else(|| anyhow::anyhow!("GearsService not initialized"))?,
        );

        let router = crate::api::rest::routes::register_routes(router, openapi, service);

        tracing::info!("ServiceDiscoveryGear REST routes registered");
        Ok(router)
    }

    /// With a directory backend, keep `/readyz` at 503 until the feed task
    /// has loaded the snapshot, so a new replica is not routed traffic
    /// before it knows the directory.
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        self.directory_backend.get()?;
        Some(Arc::new(SnapshotLoaded(Arc::clone(&self.snapshot_loaded))))
    }
}

/// Readiness check that passes once the backend snapshot has loaded.
///
/// Gated only on the *first* load: when the backend later goes down, every
/// replica would fail readiness at once, so each keeps serving its last
/// snapshot while it resyncs.
struct SnapshotLoaded(Arc<AtomicBool>);

#[async_trait]
impl Healthcheck for SnapshotLoaded {
    fn name(&self) -> &'static str {
        "directory-snapshot"
    }

    async fn check(&self) -> HealthcheckResult {
        if self.0.load(Ordering::Acquire) {
            HealthcheckResult::healthy()
        } else {
            HealthcheckResult::unhealthy("directory snapshot not loaded yet")
                .with_code("directory_snapshot_loading")
        }
    }
}

/// Export gRPC services to `grpc-hub`
#[async_trait]
impl GrpcServiceCapability for ServiceDiscoveryGear {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> Result<Vec<RegisterGrpcServiceFn>> {
        let api = self
            .directory_api
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("DirectoryClient not initialized"))?;

        let policy = self
            .policy
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("registration policy not initialized"))?;

        let directory_svc = server::make_directory_service(api, policy);

        Ok(vec![RegisterGrpcServiceFn {
            service_name: DIRECTORY_SERVICE_NAME,
            register: Box::new(move |routes| {
                routes.add_service(directory_svc.clone());
            }),
        }])
    }
}

/// Default sweeper cadence if `init` has not recorded one (it always does in
/// practice); bounded below so a misconfigured zero grace cannot hot-loop.
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const MIN_SWEEP_INTERVAL: Duration = Duration::from_secs(1);
const BACKEND_JANITOR_INTERVAL: Duration = Duration::from_secs(30);

/// The lease sweeper. It runs with or without a directory backend: each
/// replica stamps its own leases, so each reclaims its own expired records.
///
/// Reads already hide records whose lease expired past grace, so this task only
/// reclaims their memory (and frees the gRPC service names and round-robin
/// counters of a gear whose last instance expired, and, with a backend, the
/// replica's lapsed tombstones). It runs until `cancel`.
async fn sweeper_loop(
    sweep: impl Fn(Instant) + Send + 'static,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut tick = tokio::time::interval(interval.max(MIN_SWEEP_INTERVAL));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => sweep(Instant::now()),
        }
    }
    tracing::debug!("service-discovery lease sweeper stopped");
}

async fn backend_janitor_loop_with_interval(
    backend: Arc<dyn DirectoryBackend>,
    grace: Duration,
    cancel: CancellationToken,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                if let Err(error) = backend.janitor(grace).await {
                    tracing::warn!(%error, "service-discovery shared-storage janitor failed");
                }
            }
        }
    }
}

/// First delay before retrying a failed snapshot load or resubscribing after
/// the backend change feed fails.
const RESYNC_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// Cap on the resubscribe delay while the backend stays unavailable.
const RESYNC_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Safety-net reload interval while the change feed remains healthy.
const BACKEND_RESYNC_INTERVAL: Duration = Duration::from_secs(30);

fn next_resync_delay(delay: Duration) -> Duration {
    (delay * 2).min(RESYNC_MAX_BACKOFF)
}

async fn load_backend_snapshot(
    replica: &DirectoryReplica,
    backend: &dyn DirectoryBackend,
) -> Result<(), DirectoryBackendError> {
    let started = Instant::now();
    let records = backend.load_all(replica.manager().lease_grace()).await?;
    replica.replace_snapshot(records, started);
    Ok(())
}

/// Load the backend snapshot, mark it loaded, then apply the change feed to
/// `replica` until `cancel` fires.
///
/// The subscription is established before the snapshot is loaded; the replica
/// orders both by record version. This runs in the background because `init`
/// and `start` must not wait on a remote service; readiness waits on `loaded`.
/// When the feed fails or ends, resubscribe and reload with backoff. The
/// backoff resets once a feed delivers a change, so a feed that fails at once
/// keeps backing off.
#[expect(
    clippy::cognitive_complexity,
    reason = "tokio::select! and tracing macros inflate the measured complexity"
)]
async fn backend_feed_loop(
    replica: Arc<DirectoryReplica>,
    backend: Arc<dyn DirectoryBackend>,
    loaded: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    let mut retry_delay = RESYNC_INITIAL_BACKOFF;
    let Some(mut changes) =
        sync_backend(&replica, backend.as_ref(), &cancel, &mut retry_delay).await
    else {
        return;
    };
    loaded.store(true, Ordering::Release);
    tracing::info!("service-discovery backend snapshot loaded");
    let mut resync = tokio::time::interval(BACKEND_RESYNC_INTERVAL);
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    resync.tick().await;
    loop {
        let change = tokio::select! {
            () = cancel.cancelled() => break,
            _ = resync.tick() => {
                match await_or_cancel(
                    &cancel,
                    load_backend_snapshot(&replica, backend.as_ref()),
                )
                .await
                {
                    Some(Ok(())) => {
                        tracing::debug!("service-discovery periodic backend snapshot completed");
                    }
                    Some(Err(error)) => {
                        tracing::error!(
                            error = format!("{error:#}"),
                            "service-discovery periodic backend snapshot failed"
                        );
                    }
                    None => break,
                }
                continue;
            }
            change = changes.next() => change,
        };
        if apply_feed_item(&replica, change) {
            retry_delay = RESYNC_INITIAL_BACKOFF;
            continue;
        }
        match resync_backend(&replica, backend.as_ref(), &cancel, &mut retry_delay).await {
            Some(next) => changes = next,
            None => break,
        }
    }
    tracing::debug!("service-discovery backend change feed stopped");
}

/// Apply one item from the change feed. Returns `false` when the feed failed
/// or ended and the caller must resync.
#[expect(
    clippy::cognitive_complexity,
    reason = "tracing macros inflate the measured complexity"
)]
fn apply_feed_item(
    replica: &DirectoryReplica,
    item: Option<Result<DirectoryChange, DirectoryBackendError>>,
) -> bool {
    match item {
        Some(Ok(change)) => {
            let result = match change {
                DirectoryChange::Upsert(record) | DirectoryChange::Renew(record) => {
                    replica.apply_change(record)
                }
                DirectoryChange::Remove {
                    gear,
                    instance_id,
                    generation,
                } => {
                    replica.apply_removal(&gear, instance_id, generation);
                    Ok(())
                }
            };
            if let Err(conflict) = result {
                tracing::warn!(%conflict, "skipping directory backend change that violates pinned gRPC service ownership");
            }
            true
        }
        Some(Err(err)) => {
            tracing::warn!(error = %err, "directory backend change feed failed; resyncing");
            false
        }
        None => {
            tracing::warn!("directory backend change feed ended; resyncing");
            false
        }
    }
}

/// Pause, then [`sync_backend`]; the pause keeps a feed that fails at once
/// from spinning. Returns `None` if `cancel` fires first.
async fn resync_backend(
    replica: &DirectoryReplica,
    backend: &dyn DirectoryBackend,
    cancel: &CancellationToken,
    retry_delay: &mut Duration,
) -> Option<DirectoryChangeStream> {
    wait_before_retry(cancel, retry_delay).await?;
    sync_backend(replica, backend, cancel, retry_delay).await
}

async fn await_or_cancel<T>(
    cancel: &CancellationToken,
    future: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        () = cancel.cancelled() => None,
        output = future => Some(output),
    }
}

async fn wait_before_retry(cancel: &CancellationToken, delay: &mut Duration) -> Option<()> {
    await_or_cancel(cancel, tokio::time::sleep(*delay)).await?;
    *delay = next_resync_delay(*delay);
    Some(())
}

/// Subscribe to the change feed and load the snapshot, retrying with backoff
/// until both succeed. Returns `None` if `cancel` fires first.
async fn sync_backend(
    replica: &DirectoryReplica,
    backend: &dyn DirectoryBackend,
    cancel: &CancellationToken,
    delay: &mut Duration,
) -> Option<DirectoryChangeStream> {
    loop {
        if let Some(changes) = try_sync_backend(replica, backend, cancel).await {
            return Some(changes);
        }
        wait_before_retry(cancel, delay).await?;
    }
}

async fn try_sync_backend(
    replica: &DirectoryReplica,
    backend: &dyn DirectoryBackend,
    cancel: &CancellationToken,
) -> Option<DirectoryChangeStream> {
    // Establish the subscription before scanning so concurrent changes are buffered.
    let changes = await_or_cancel(cancel, backend.changes())
        .await?
        .inspect_err(|err| {
            tracing::error!(
                error = format!("{err:#}"),
                "failed to establish directory backend change feed; retrying"
            );
        })
        .ok()?;
    if let Err(err) = await_or_cancel(cancel, load_backend_snapshot(replica, backend)).await? {
        tracing::error!(
            error = format!("{err:#}"),
            "failed to load directory backend snapshot; retrying"
        );
        return None;
    }
    Some(changes)
}

#[async_trait]
impl RunnableCapability for ServiceDiscoveryGear {
    /// Start the lease sweeper and, with a directory backend, the task that
    /// loads its snapshot and follows its change feed. Neither is awaited:
    /// readiness waits on the snapshot instead. The tasks self-stop when `cancel`
    /// (a child of the runtime root token) fires, so [`Self::stop`] is a no-op.
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let manager = self.gear_manager.get().cloned().ok_or_else(|| {
            anyhow::anyhow!("GearManager not wired; init() must run before start()")
        })?;
        let interval = self
            .sweep_interval
            .get()
            .copied()
            .unwrap_or(DEFAULT_SWEEP_INTERVAL);
        if let Some(backend) = self.directory_backend.get().cloned() {
            let replica = self.replica.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("DirectoryReplica not set; init() must run before start()")
            })?;
            let janitor_interval = {
                #[cfg(all(test, feature = "integration"))]
                {
                    self.backend_janitor_interval
                        .get()
                        .copied()
                        .unwrap_or(BACKEND_JANITOR_INTERVAL)
                }
                #[cfg(not(all(test, feature = "integration")))]
                {
                    BACKEND_JANITOR_INTERVAL
                }
            };
            tokio::spawn(backend_janitor_loop_with_interval(
                Arc::clone(&backend),
                manager.lease_grace(),
                cancel.clone(),
                janitor_interval,
            ));
            tokio::spawn(backend_feed_loop(
                Arc::clone(&replica),
                backend,
                Arc::clone(&self.snapshot_loaded),
                cancel.clone(),
            ));
            tracing::info!("service-discovery backend feed task started");
            tokio::spawn(sweeper_loop(
                move |now| replica.sweep(now),
                interval,
                cancel,
            ));
        } else {
            tokio::spawn(sweeper_loop(
                move |now| {
                    manager.sweep_expired(now);
                },
                interval,
                cancel,
            ));
        }
        tracing::info!(
            sweep_interval_secs = interval.as_secs(),
            "service-discovery lease sweeper started"
        );
        Ok(())
    }

    async fn stop(&self, _deadline_token: CancellationToken) -> anyhow::Result<()> {
        // The sweeper stops on the root cancellation token propagated to the
        // task's `cancel`; stop the selected plugin after its consumers have
        // stopped.
        if let Some(handle) = self.storage_plugin.lock().await.take() {
            handle.stop().await;
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::directory_backend::{
        DirectoryBackendError, DirectoryChangeStream, DirectoryRecord, MemoryDirectoryBackend,
    };
    use crate::replica::DirectoryReplica;
    use std::sync::atomic::AtomicUsize;
    use tokio_util::sync::CancellationToken;
    use toolkit::HealthcheckStatus;
    use toolkit::client_hub::ClientHub;
    use toolkit::config::ConfigProvider;
    use toolkit::runtime::{Endpoint, GearInstance, ReportedState};
    use uuid::Uuid;

    struct StaticConfigProvider(serde_json::Value);
    impl ConfigProvider for StaticConfigProvider {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            (gear_name == "service-discovery").then_some(&self.0)
        }
    }

    /// Run `init` against a config section, returning the gear so tests can
    /// inspect the policy it built.
    async fn init_with_config(config: serde_json::Value) -> Result<ServiceDiscoveryGear> {
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("config".to_owned(), config);
        let ctx = GearCtx::new(
            "service-discovery",
            Uuid::new_v4(),
            Arc::new(StaticConfigProvider(serde_json::Value::Object(wrapped))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );
        let gear = ServiceDiscoveryGear::default();
        let gear_manager = Arc::new(GearManager::new());
        gear.gear_manager
            .set(gear_manager)
            .map_err(|_| anyhow::anyhow!("gear_manager already set"))?;
        toolkit::Gear::init(&gear, &ctx).await?;
        Ok(gear)
    }

    fn string_set(values: &[&str]) -> std::collections::HashSet<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn init_succeeds_with_empty_config() {
        init_with_config(serde_json::json!({}))
            .await
            .expect("init with an empty config section (all defaults) must succeed");
    }

    #[tokio::test]
    async fn start_loads_snapshot_in_background_and_gates_readiness() {
        let memory = MemoryDirectoryBackend::default();
        memory
            .register(
                Arc::new(
                    GearInstance::new("preexisting-gear", Uuid::new_v4())
                        .with_rest_endpoint(Endpoint::http("127.0.0.1", 8080))
                        .with_reported_state(ReportedState::Serving),
                ),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        // The first load fails: startup must neither wait on nor fail because
        // of the backend.
        let backend = Arc::new(FlakyBackend::new(memory.clone(), 0, 0));

        let mut wrapped = serde_json::Map::new();
        wrapped.insert("config".to_owned(), serde_json::json!({}));
        let ctx = GearCtx::new(
            "service-discovery",
            Uuid::new_v4(),
            Arc::new(StaticConfigProvider(serde_json::Value::Object(wrapped))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );
        let gear = ServiceDiscoveryGear::default();
        let manager = Arc::new(GearManager::new());
        gear.gear_manager
            .set(Arc::clone(&manager))
            .map_err(|_| anyhow::anyhow!("gear_manager already set"))
            .unwrap();
        gear.install_directory_backend(backend.clone()).unwrap();
        toolkit::Gear::init(&gear, &ctx).await.unwrap();

        let readiness =
            RestApiCapability::healthcheck(&gear, &ctx).expect("a backend must gate readiness");
        assert_eq!(readiness.check().await.status, HealthcheckStatus::Unhealthy);
        assert!(manager.instances_of("preexisting-gear").is_empty());

        let cancel = CancellationToken::new();
        RunnableCapability::start(&gear, cancel.clone())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while readiness.check().await.status != HealthcheckStatus::Healthy {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("readiness must pass once a snapshot load succeeds");
        assert!(backend.loads.load(Ordering::SeqCst) >= 2);
        let instances = manager.instances_of("preexisting-gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(
            instances[0].state(),
            toolkit::runtime::InstanceState::Healthy
        );

        memory
            .register(
                Arc::new(GearInstance::new("late-gear", Uuid::new_v4())),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        wait_for("the feed task must follow the feed after loading", || {
            !manager.instances_of("late-gear").is_empty()
        })
        .await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn replicas_receive_registration_heartbeat_and_deregistration_from_feed() {
        let backend = Arc::new(MemoryDirectoryBackend::default());
        let manager_a = Arc::new(GearManager::new());
        let manager_b = Arc::new(GearManager::new());
        let loaded_a = Arc::new(AtomicBool::new(false));
        let loaded_b = Arc::new(AtomicBool::new(false));
        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();
        let replica_a = Arc::new(DirectoryReplica::new(Arc::clone(&manager_a)));
        let replica_b = Arc::new(DirectoryReplica::new(Arc::clone(&manager_b)));
        let task_a = tokio::spawn(backend_feed_loop(
            Arc::clone(&replica_a),
            backend.clone(),
            Arc::clone(&loaded_a),
            cancel_a.clone(),
        ));
        let task_b = tokio::spawn(backend_feed_loop(
            replica_b,
            backend.clone(),
            Arc::clone(&loaded_b),
            cancel_b.clone(),
        ));

        wait_for("both replicas must load the initial snapshot", || {
            loaded_a.load(std::sync::atomic::Ordering::Acquire)
                && loaded_b.load(std::sync::atomic::Ordering::Acquire)
        })
        .await;

        let client_a = ReplicatedDirectoryClient::new(replica_a, backend);
        let gear = "replicated-gear";
        let instance_id = Uuid::new_v4();
        client_a
            .register_instance(
                cf_system_sdks::directory::RegisterInstanceInfo::new(gear, instance_id.to_string())
                    .with_reported_state(cf_system_sdks::directory::ReportedState::Starting),
            )
            .await
            .unwrap();
        wait_for("registration must reach the other replica", || {
            manager_b
                .instances_of(gear)
                .iter()
                .any(|instance| instance.instance_id == instance_id)
        })
        .await;
        assert_eq!(
            manager_b.instances_of(gear)[0].state(),
            toolkit::runtime::InstanceState::Registered
        );

        client_a
            .heartbeat(
                gear,
                &instance_id.to_string(),
                Some(cf_system_sdks::directory::ReportedState::Serving),
            )
            .await
            .unwrap();
        wait_for("serving heartbeat must reach the other replica", || {
            manager_b.instances_of(gear).iter().any(|instance| {
                instance.instance_id == instance_id
                    && instance.state() == toolkit::runtime::InstanceState::Healthy
            })
        })
        .await;

        client_a
            .heartbeat(
                gear,
                &instance_id.to_string(),
                Some(cf_system_sdks::directory::ReportedState::Draining),
            )
            .await
            .unwrap();
        wait_for("draining heartbeat must reach the other replica", || {
            manager_b.instances_of(gear).iter().any(|instance| {
                instance.instance_id == instance_id
                    && instance.state() == toolkit::runtime::InstanceState::Draining
            })
        })
        .await;

        client_a
            .deregister_instance(gear, &instance_id.to_string())
            .await
            .unwrap();
        wait_for("deregistration must reach the other replica", || {
            manager_b.instances_of(gear).is_empty()
        })
        .await;

        cancel_a.cancel();
        cancel_b.cancel();
        task_a.await.unwrap();
        task_b.await.unwrap();
    }

    #[tokio::test]
    async fn restart_during_lease_grace_restores_quarantined_state() {
        let backend = MemoryDirectoryBackend::default();
        let id = Uuid::new_v4();
        backend
            .register(
                Arc::new(
                    GearInstance::new("grace-gear", id).with_reported_state(ReportedState::Serving),
                ),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        backend.set_lease_age("grace-gear", id, Duration::from_secs(10));

        let replica = DirectoryReplica::new(Arc::new(
            GearManager::new().with_lease_policy(Duration::from_secs(30), Duration::from_secs(30)),
        ));
        load_backend_snapshot(&replica, &backend).await.unwrap();

        assert_eq!(
            replica.manager().instances_of("grace-gear")[0].state(),
            toolkit::runtime::InstanceState::Quarantined
        );
    }

    #[tokio::test]
    async fn reregister_after_lease_grace_starts_a_new_generation() {
        let backend = MemoryDirectoryBackend::default();
        let id = Uuid::new_v4();
        let ttl = Duration::from_secs(30);
        let grace = Duration::from_secs(30);
        let first = backend
            .register(
                Arc::new(
                    GearInstance::new("grace-gear", id).with_reported_state(ReportedState::Serving),
                ),
                ttl,
                grace,
            )
            .await
            .unwrap();
        backend.set_lease_age("grace-gear", id, Duration::from_secs(61));

        let second = backend
            .register(
                Arc::new(
                    GearInstance::new("grace-gear", id)
                        .with_reported_state(ReportedState::Starting),
                ),
                ttl,
                grace,
            )
            .await
            .unwrap();

        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(
            second.instance.reported_state,
            Some(ReportedState::Starting)
        );
    }

    /// Poll `done` until it holds, failing with `msg` after five seconds.
    async fn wait_for(msg: &str, done: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect(msg);
    }

    /// Delegates to a [`MemoryDirectoryBackend`], but its `load_all` call
    /// number `fail_load` fails, its first `end_feeds` change feeds end
    /// immediately, and later feeds end after `feed_items` changes.
    struct FlakyBackend {
        inner: MemoryDirectoryBackend,
        fail_load: usize,
        end_feeds: usize,
        feed_items: usize,
        subscriptions: AtomicUsize,
        loads: AtomicUsize,
    }

    impl FlakyBackend {
        fn new(inner: MemoryDirectoryBackend, fail_load: usize, end_feeds: usize) -> Self {
            Self {
                inner,
                fail_load,
                end_feeds,
                feed_items: usize::MAX,
                subscriptions: AtomicUsize::new(0),
                loads: AtomicUsize::new(0),
            }
        }

        fn with_feed_items(mut self, feed_items: usize) -> Self {
            self.feed_items = feed_items;
            self
        }
    }

    #[async_trait]
    impl DirectoryBackend for FlakyBackend {
        async fn register(
            &self,
            instance: Arc<GearInstance>,
            ttl: Duration,
            grace: Duration,
        ) -> Result<DirectoryRecord, DirectoryBackendError> {
            self.inner.register(instance, ttl, grace).await
        }
        async fn heartbeat(
            &self,
            gear: &str,
            id: Uuid,
            reported: ReportedState,
            ttl: Duration,
            grace: Duration,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            self.inner.heartbeat(gear, id, reported, ttl, grace).await
        }
        async fn deregister(
            &self,
            gear: &str,
            id: Uuid,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            self.inner.deregister(gear, id).await
        }
        async fn load_all(
            &self,
            grace: Duration,
        ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
            if self.loads.fetch_add(1, Ordering::SeqCst) == self.fail_load {
                return Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                    "backend unavailable"
                )));
            }
            self.inner.load_all(grace).await
        }
        async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
            if self.subscriptions.fetch_add(1, Ordering::SeqCst) < self.end_feeds {
                // `unfold` panics if polled again after it ends.
                Ok(Box::pin(futures_util::stream::unfold((), |()| async {
                    None
                })))
            } else {
                Ok(Box::pin(self.inner.changes().await?.take(self.feed_items)))
            }
        }
    }

    #[tokio::test]
    async fn feed_loop_resyncs_after_feed_ends_and_load_fails() {
        let memory = MemoryDirectoryBackend::default();
        let backend = Arc::new(FlakyBackend::new(memory.clone(), 1, 1));
        let manager = Arc::new(GearManager::new());
        let loaded = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(backend_feed_loop(
            Arc::new(DirectoryReplica::new(Arc::clone(&manager))),
            backend.clone(),
            Arc::clone(&loaded),
            cancel.clone(),
        ));

        // Wait until the loop has resubscribed after the failed reload.
        wait_for("feed loop must retry the failed snapshot load", || {
            backend.loads.load(Ordering::SeqCst) >= 3
        })
        .await;
        assert!(loaded.load(Ordering::Acquire));

        memory
            .register(
                Arc::new(GearInstance::new("late-gear", Uuid::new_v4())),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        wait_for("feed loop must follow the new feed after resyncing", || {
            !manager.instances_of("late-gear").is_empty()
        })
        .await;

        cancel.cancel();
        task.await.expect("feed loop must stop cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_watch_closures_back_off_between_snapshot_reloads() {
        let memory = MemoryDirectoryBackend::default();
        let backend = Arc::new(FlakyBackend::new(memory, usize::MAX, 2));
        let manager = Arc::new(GearManager::new());
        let loaded = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(backend_feed_loop(
            Arc::new(DirectoryReplica::new(manager)),
            backend.clone(),
            loaded,
            cancel.clone(),
        ));

        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
        assert_eq!(backend.subscriptions.load(Ordering::SeqCst), 1);

        tokio::time::advance(RESYNC_INITIAL_BACKOFF.saturating_sub(Duration::from_millis(1))).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(backend.loads.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(backend.loads.load(Ordering::SeqCst), 2);
        assert_eq!(backend.subscriptions.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_millis(499)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(backend.loads.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(backend.loads.load(Ordering::SeqCst), 3);
        assert_eq!(backend.subscriptions.load(Ordering::SeqCst), 3);

        cancel.cancel();
        task.await.expect("feed loop must stop cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_resets_once_a_feed_delivers_a_change() {
        let memory = MemoryDirectoryBackend::default();
        let backend = Arc::new(FlakyBackend::new(memory.clone(), usize::MAX, 2).with_feed_items(1));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(backend_feed_loop(
            Arc::new(DirectoryReplica::new(Arc::new(GearManager::new()))),
            backend.clone(),
            Arc::new(AtomicBool::new(false)),
            cancel.clone(),
        ));
        let settle = || async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        };

        // Two feeds end at once: the delay grows to 250 ms, then 500 ms.
        settle().await;
        tokio::time::advance(Duration::from_millis(250)).await;
        settle().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        settle().await;
        assert_eq!(backend.loads.load(Ordering::SeqCst), 3);

        // The third feed delivers one change and then ends.
        memory
            .register(
                Arc::new(GearInstance::new("gear", Uuid::new_v4())),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        settle().await;
        tokio::time::advance(RESYNC_INITIAL_BACKOFF.saturating_sub(Duration::from_millis(1))).await;
        settle().await;
        assert_eq!(backend.loads.load(Ordering::SeqCst), 3);
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(
            backend.loads.load(Ordering::SeqCst),
            4,
            "a feed that delivered a change must resync after the initial backoff"
        );

        cancel.cancel();
        task.await.expect("feed loop must stop cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_feed_still_triggers_periodic_snapshot_reload() {
        let backend = Arc::new(SnapshotRecoveryBackend {
            loads: AtomicUsize::new(0),
            record: DirectoryRecord {
                instance: Arc::new(
                    GearInstance::new("recovered-gear", Uuid::new_v4())
                        .with_reported_state(ReportedState::Serving),
                ),
                generation: 1,
                revision: 1,
                lease_expires_at: std::time::SystemTime::now() + Duration::from_secs(60),
            },
        });
        let manager = Arc::new(GearManager::new());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(backend_feed_loop(
            Arc::new(DirectoryReplica::new(Arc::clone(&manager))),
            backend.clone(),
            Arc::new(AtomicBool::new(false)),
            cancel.clone(),
        ));
        let settle = || async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        };

        settle().await;
        assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
        assert!(manager.instances_of("recovered-gear").is_empty());
        tokio::time::advance(BACKEND_RESYNC_INTERVAL).await;
        settle().await;
        assert_eq!(backend.loads.load(Ordering::SeqCst), 2);
        assert_eq!(manager.instances_of("recovered-gear").len(), 1);

        cancel.cancel();
        task.await.expect("feed loop must stop cleanly on cancel");
    }

    #[test]
    fn repeated_watch_failures_carry_capped_backoff() {
        let mut delay = RESYNC_INITIAL_BACKOFF;
        let mut observed = vec![delay];
        for _ in 0..8 {
            delay = next_resync_delay(delay);
            observed.push(delay);
        }
        assert_eq!(
            observed,
            [
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    struct SnapshotBarrierBackend {
        inner: MemoryDirectoryBackend,
        snapshot_taken: Arc<tokio::sync::Notify>,
        release_snapshot: Arc<tokio::sync::Notify>,
    }

    struct SnapshotRecoveryBackend {
        loads: AtomicUsize,
        record: DirectoryRecord,
    }

    #[async_trait]
    impl DirectoryBackend for SnapshotRecoveryBackend {
        async fn register(
            &self,
            _: Arc<GearInstance>,
            _: Duration,
            _: Duration,
        ) -> Result<DirectoryRecord, DirectoryBackendError> {
            Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                "register not used by snapshot recovery test"
            )))
        }

        async fn heartbeat(
            &self,
            _: &str,
            _: Uuid,
            _: ReportedState,
            _: Duration,
            _: Duration,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                "heartbeat not used by snapshot recovery test"
            )))
        }

        async fn deregister(
            &self,
            _: &str,
            _: Uuid,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                "deregister not used by snapshot recovery test"
            )))
        }

        async fn load_all(
            &self,
            _: Duration,
        ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
            if self.loads.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(Vec::new())
            } else {
                Ok(vec![self.record.clone()])
            }
        }

        async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }

    #[async_trait]
    impl DirectoryBackend for SnapshotBarrierBackend {
        async fn register(
            &self,
            instance: Arc<GearInstance>,
            ttl: Duration,
            grace: Duration,
        ) -> Result<DirectoryRecord, DirectoryBackendError> {
            self.inner.register(instance, ttl, grace).await
        }

        async fn heartbeat(
            &self,
            gear: &str,
            id: Uuid,
            reported: ReportedState,
            ttl: Duration,
            grace: Duration,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            self.inner.heartbeat(gear, id, reported, ttl, grace).await
        }

        async fn deregister(
            &self,
            gear: &str,
            id: Uuid,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            self.inner.deregister(gear, id).await
        }

        async fn load_all(
            &self,
            grace: Duration,
        ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
            let snapshot = self.inner.load_all(grace).await?;
            self.snapshot_taken.notify_one();
            self.release_snapshot.notified().await;
            Ok(snapshot)
        }

        async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
            self.inner.changes().await
        }
    }

    #[tokio::test]
    async fn subscription_buffers_a_write_during_initial_snapshot_scan() {
        let backend = Arc::new(SnapshotBarrierBackend {
            inner: MemoryDirectoryBackend::default(),
            snapshot_taken: Arc::new(tokio::sync::Notify::new()),
            release_snapshot: Arc::new(tokio::sync::Notify::new()),
        });
        let manager = Arc::new(GearManager::new());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(backend_feed_loop(
            Arc::new(DirectoryReplica::new(Arc::clone(&manager))),
            backend.clone(),
            Arc::new(AtomicBool::new(false)),
            cancel.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(5), backend.snapshot_taken.notified())
            .await
            .expect("snapshot scan must start after subscription");
        backend
            .inner
            .register(
                Arc::new(GearInstance::new("racing-gear", Uuid::new_v4())),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        backend.release_snapshot.notify_one();

        wait_for(
            "the write during the scan must arrive on the established feed",
            || !manager.instances_of("racing-gear").is_empty(),
        )
        .await;

        cancel.cancel();
        task.await.expect("feed loop must stop cleanly on cancel");
    }

    #[tokio::test]
    async fn init_reads_trusted_registrars_config() {
        let gear = init_with_config(serde_json::json!({
            "trusted_registrars": ["oagw", "cluster"]
        }))
        .await
        .expect("init with a trusted_registrars config section must succeed");

        let policy = gear.policy.get().expect("init must build the policy");
        assert_eq!(
            policy.trusted_registrars,
            string_set(&["oagw", "cluster"]),
            "both configured registrars must reach the policy"
        );
    }

    #[tokio::test]
    async fn init_reads_namespace_and_trust_domain_config() {
        let gear = init_with_config(serde_json::json!({
            "platform_namespaces": ["toolkit", "platform"],
            "trust_domains": ["example.org"]
        }))
        .await
        .expect("init with platform_namespaces / trust_domains config must succeed");

        let policy = gear.policy.get().expect("init must build the policy");
        assert_eq!(
            policy.platform_namespaces,
            string_set(&["toolkit", "platform"]),
            "both configured namespaces must reach the policy"
        );
        assert_eq!(
            policy.trust_domains,
            string_set(&["example.org"]),
            "the configured trust domain must reach the policy"
        );
    }

    #[tokio::test]
    async fn init_applies_configured_lease_policy() {
        let gear = init_with_config(serde_json::json!({
            "lease": { "ttl": "5s", "grace": "7s" }
        }))
        .await
        .expect("init with a lease config section must succeed");

        let manager = gear
            .gear_manager
            .get()
            .expect("init must wire the GearManager");
        assert_eq!(
            manager.lease_ttl(),
            Duration::from_secs(5),
            "configured lease ttl must reach the GearManager"
        );
        assert_eq!(
            gear.sweep_interval.get().copied(),
            Some(Duration::from_secs(7)),
            "configured lease grace must drive the sweeper cadence"
        );
    }

    #[tokio::test]
    async fn init_defaults_lease_policy_to_thirty_seconds() {
        let gear = init_with_config(serde_json::json!({}))
            .await
            .expect("init with no lease config must use the default lease policy");
        let manager = gear.gear_manager.get().expect("GearManager wired");
        assert_eq!(manager.lease_ttl(), Duration::from_secs(30));
        assert_eq!(
            gear.sweep_interval.get().copied(),
            Some(Duration::from_secs(30))
        );
    }

    #[tokio::test]
    async fn init_installs_configured_grpc_service_owners() {
        use toolkit::runtime::{Endpoint, GearInstance};

        let service = "cf.authz.v1.AuthzService";
        let gear = init_with_config(serde_json::json!({
            "grpc_service_owners": { service: "authz" }
        }))
        .await
        .expect("init with a grpc_service_owners config section must succeed");

        let manager = gear
            .gear_manager
            .get()
            .expect("init must wire the GearManager");

        // A squatter registering first is rejected in favor of the configured
        // owner; the configured owner is admitted.
        let conflict = manager
            .register_instance(Arc::new(
                GearInstance::new("evil", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .expect_err("a non-owner must not claim a configured service name");
        assert_eq!(conflict.owner, "authz");
        assert!(manager.instances_of("evil").is_empty());

        manager
            .register_instance(Arc::new(
                GearInstance::new("authz", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
            ))
            .expect("the configured owner must be admitted");
    }

    #[tokio::test]
    async fn init_rejects_unknown_config_key() {
        // A misspelled key must fail loudly at startup rather than silently
        // deserialize to an empty set and deny the intended registrar at runtime.
        let err = init_with_config(serde_json::json!({
            "trusted_registrar": ["flight-control"]
        }))
        .await
        .map(|_| ())
        .unwrap_err();
        assert!(
            err.to_string().contains("trusted_registrar"),
            "expected the error to name the offending key, got {err}"
        );
    }

    #[tokio::test]
    async fn init_rejects_invalid_grpc_service_owner_pin() {
        // A key that can never be a valid advertised service name would pin
        // nothing; fail at startup instead of shipping a dead pin.
        let bad_key = init_with_config(serde_json::json!({
            "grpc_service_owners": { ".trailing-dot-is-invalid.": "authz-resolver" }
        }))
        .await
        .map(|_| ())
        .unwrap_err();
        assert!(
            bad_key.to_string().contains("grpc_service_owners"),
            "expected the error to name the offending pin, got {bad_key}"
        );

        // An empty owner is meaningless.
        let empty_owner = init_with_config(serde_json::json!({
            "grpc_service_owners": { "cf.authz.v1.AuthzService": "" }
        }))
        .await
        .map(|_| ())
        .unwrap_err();
        assert!(
            empty_owner.to_string().contains("owner"),
            "expected the error to flag the empty owner, got {empty_owner}"
        );
    }

    #[tokio::test]
    async fn init_fails_without_a_wired_gear_manager() {
        // Missing runtime wiring must fail `init` instead of panicking on the `OnceLock`.
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("config".to_owned(), serde_json::json!({}));
        let ctx = GearCtx::new(
            "service-discovery",
            Uuid::new_v4(),
            Arc::new(StaticConfigProvider(serde_json::Value::Object(wrapped))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );

        // Note: no `gear_manager` wired, unlike `init_with_config`.
        let gear = ServiceDiscoveryGear::default();
        let err = toolkit::Gear::init(&gear, &ctx)
            .await
            .expect_err("init must fail when the GearManager was never wired");
        assert!(
            err.to_string().contains("GearManager not wired"),
            "expected the missing-GearManager error, got {err}"
        );
    }

    /// A bare `GearCtx` for the capability methods that ignore their context
    /// (`get_grpc_services`, `register_rest`); config is irrelevant here.
    fn ignored_ctx() -> GearCtx {
        GearCtx::new(
            "service-discovery",
            Uuid::new_v4(),
            Arc::new(StaticConfigProvider(serde_json::Value::Object(
                serde_json::Map::new(),
            ))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        )
    }

    #[test]
    fn wire_runtime_wires_the_gear_manager() {
        use toolkit::runtime::{GrpcInstallerStore, SystemContext};

        let gear = ServiceDiscoveryGear::default();
        let sys = SystemContext::new(
            Uuid::new_v4(),
            Arc::new(GearManager::new()),
            Arc::new(GrpcInstallerStore::new()),
        );

        gear.wire_runtime(&sys)
            .expect("wire_runtime must wire the injected GearManager");
        assert!(
            gear.gear_manager.get().is_some(),
            "wire_runtime must store the GearManager for init to use"
        );

        // A second wiring must fail loudly rather than silently swallow the
        // duplicate; the OnceLock is set-once by contract.
        gear.wire_runtime(&sys)
            .expect_err("wire_runtime must reject a second call");
    }

    #[tokio::test]
    async fn get_grpc_services_advertises_the_directory_service() {
        let gear = init_with_config(serde_json::json!({}))
            .await
            .expect("init must succeed so directory_api and policy are set");

        let services = GrpcServiceCapability::get_grpc_services(&gear, &ignored_ctx())
            .await
            .expect("get_grpc_services must succeed once init has run");

        assert_eq!(services.len(), 1, "exactly one gRPC service is exported");
        assert_eq!(
            services[0].service_name, DIRECTORY_SERVICE_NAME,
            "the exported service must be the directory service"
        );
    }

    #[tokio::test]
    async fn get_grpc_services_fails_before_init() {
        // Without init, neither directory_api nor policy is set, so the export
        // must fail rather than panic on a missing OnceLock.
        let gear = ServiceDiscoveryGear::default();
        assert!(
            GrpcServiceCapability::get_grpc_services(&gear, &ignored_ctx())
                .await
                .is_err(),
            "get_grpc_services must fail before init wires the client"
        );
    }

    #[tokio::test]
    async fn register_rest_mounts_routes() {
        let gear = init_with_config(serde_json::json!({}))
            .await
            .expect("init must succeed so the GearsService is set");

        let openapi = toolkit::api::OpenApiRegistryImpl::new();
        let _router =
            RestApiCapability::register_rest(&gear, &ignored_ctx(), axum::Router::new(), &openapi)
                .expect("register_rest must succeed once init has built the GearsService");
    }

    #[test]
    fn register_rest_fails_before_init() {
        let gear = ServiceDiscoveryGear::default();
        let openapi = toolkit::api::OpenApiRegistryImpl::new();
        assert!(
            RestApiCapability::register_rest(&gear, &ignored_ctx(), axum::Router::new(), &openapi)
                .is_err(),
            "register_rest must fail before init builds the GearsService"
        );
    }

    #[cfg(feature = "integration")]
    mod integration_tests {
        include!("../tests/integration/gear_backend.rs");
    }
}
