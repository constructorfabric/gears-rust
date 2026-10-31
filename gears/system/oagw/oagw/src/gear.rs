use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::config::{OagwConfig, ProxyCacheConfig, RegistryProvisioningConfig, TokenCacheConfig};
use crate::domain::ssrf::SsrfGuard;
use crate::domain::type_catalog::oagw_gts_entities;
use crate::domain::type_provisioning::TypeProvisioningService;
use crate::infra::type_provisioning::TypeProvisioningServiceImpl;
#[cfg(feature = "cluster")]
use anyhow::Context;
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
#[cfg(feature = "cluster")]
use cluster_sdk::{ClusterCacheV1, ClusterProfile};
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use sea_orm_migration::MigrationTrait;
use tenant_resolver_sdk::TenantResolverClient;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
#[cfg(feature = "cluster")]
use toolkit::client_hub::ClientHub;
use toolkit::contracts::{RunnableCapability, SystemCapability};
use toolkit::{
    DatabaseCapability, Gear, GearCtx, Healthcheck, HealthcheckResult, HealthcheckStatus,
    RestApiCapability,
};
use toolkit_db::DBProvider;
use tracing::{Instrument, error, info, info_span, warn};
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::api::rest::routes;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::repo::{
    ProxyConfigReader, RegistryRowRepository, RegistryStateStore, RepositoryError, RouteRepository,
    UpstreamRepository,
};
use crate::domain::services::registry_job::{
    ProvisioningState, ProvisioningStatus, RegistryJob, run_registry_loop,
};
use crate::domain::services::registry_state::{Gate, LEASE_POLL, LEASE_TTL, jitter};
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, DataPlaneService, EndpointSelector,
    RegistryProvisioner, RegistryProvisionerImpl, ServiceGatewayClientV1Facade,
};
use crate::infra::metrics::OagwMetricsMeter;
use crate::infra::proxy::DataPlaneServiceImpl;
use crate::infra::proxy_cache::{
    CachedProxyConfigReader, FlushingRegistryRowRepo, FlushingRouteRepo, FlushingUpstreamRepo,
    ProxyConfigCache, WriteHook,
};
#[cfg(feature = "cluster")]
use crate::infra::proxy_cache::{ChangeFeed, WatchHealth, WatchStatus};
use crate::infra::storage::{
    DbRegistryRowRepo, DbRegistryStateStore, DbRouteRepo, DbUpstreamRepo, InMemoryRegistryRowRepo,
    InMemoryRouteRepo, InMemoryUpstreamRepo, Migrator,
};

/// Shared application state injected into all handlers.
#[derive(Clone)]
pub struct AppState {
    /// Control-plane service for configuration CRUD and resolution.
    pub(crate) cp: Arc<dyn ControlPlaneService>,
    /// Data-plane service that proxies requests to upstreams.
    pub(crate) dp: Arc<dyn DataPlaneService>,
    /// Load-balancing selector picking an endpoint per upstream.
    pub(crate) backend_selector: Arc<dyn EndpointSelector>,
    /// Runtime configuration derived from the gear config.
    pub(crate) config: crate::config::RuntimeConfig,
}

/// Outbound API Gateway gear: wires repos, services, and routes.
#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::gear(
    name = "oagw",
    deps = [types_registry, credstore, tenant_resolver],
    capabilities = [system, db, rest, stateful]
)]
pub struct OutboundApiGatewayGear {
    state: arc_swap::ArcSwapOption<AppState>,
    registry_client: OnceLock<Arc<dyn TypesRegistryClient>>,
    tenant_resolver: OnceLock<Arc<dyn TenantResolverClient>>,
    type_provisioning: OnceLock<Arc<dyn TypeProvisioningService>>,
    registry_provisioner: OnceLock<Arc<dyn RegistryProvisioner>>,
    metrics: OnceLock<Arc<dyn OagwMetricsPort>>,
    /// The registry-state row on a database backend; `None` in memory.
    registry_state: OnceLock<Option<Arc<dyn RegistryStateStore>>>,
    registry_config: OnceLock<RegistryProvisioningConfig>,
    /// This process: the registry lease owner, and the origin of the proxy
    /// configuration changes it publishes.
    owner: uuid::Uuid,
    /// The registry run `start` drives on a database backend.
    registry_job: OnceLock<RegistryJob>,
    /// The running registry task, from `start` to `stop`.
    registry_task: parking_lot::Mutex<Option<(CancellationToken, tokio::task::JoinHandle<()>)>>,
    /// Where provisioning stands, for the healthcheck.
    provisioning: ProvisioningState,
    /// What `start` connects to the cluster on a database with the cache
    /// enabled; `None` otherwise.
    #[cfg(feature = "cluster")]
    cache_sync: OnceLock<Option<CacheSync>>,
    /// The proxy configuration change feed, from `start` to `stop`.
    #[cfg(feature = "cluster")]
    change_feed: parking_lot::Mutex<Option<ChangeFeed>>,
}

/// The typed cluster profile of the gear, which carries the proxy
/// configuration change log (ADR-0020). The profile name appears here and in
/// the operator's cluster YAML, and nowhere else.
#[cfg(feature = "cluster")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OagwProfile;

#[cfg(feature = "cluster")]
impl ClusterProfile for OagwProfile {
    const NAME: &'static str = "oagw";
}

#[cfg(feature = "cluster")]
cluster_sdk::register_cluster_profile!(OagwProfile);

/// The write hook (and through it the proxy configuration cache), which
/// `start` connects to the cluster change log; the hub the cluster client is
/// in; and the watch state the healthcheck reads.
#[cfg(feature = "cluster")]
struct CacheSync {
    hub: Arc<ClientHub>,
    hook: WriteHook,
    watch: WatchStatus,
}

impl Default for OutboundApiGatewayGear {
    fn default() -> Self {
        Self {
            state: arc_swap::ArcSwapOption::from(None),
            registry_client: OnceLock::new(),
            tenant_resolver: OnceLock::new(),
            type_provisioning: OnceLock::new(),
            registry_provisioner: OnceLock::new(),
            metrics: OnceLock::new(),
            registry_state: OnceLock::new(),
            registry_config: OnceLock::new(),
            owner: uuid::Uuid::new_v4(),
            registry_job: OnceLock::new(),
            registry_task: parking_lot::Mutex::new(None),
            provisioning: ProvisioningState::default(),
            #[cfg(feature = "cluster")]
            cache_sync: OnceLock::new(),
            #[cfg(feature = "cluster")]
            change_feed: parking_lot::Mutex::new(None),
        }
    }
}

/// The storage of one backend.
struct Repositories {
    /// The upstreams the Control Plane runs on.
    upstreams: Arc<dyn UpstreamRepository>,
    /// The routes the Control Plane runs on.
    routes: Arc<dyn RouteRepository>,
    /// The registry rows across tenants, for the registry provisioner only.
    registry_rows: Arc<dyn RegistryRowRepository>,
    /// The registry-state row, on a database.
    registry_state: Option<Arc<dyn RegistryStateStore>>,
    /// The proxy configuration cache's reader, on a database with the
    /// cache enabled (ADR-0020).
    proxy_reader: Option<Arc<dyn ProxyConfigReader>>,
    /// The write hook every write goes through, on a database with the
    /// cache enabled.
    write_hook: Option<WriteHook>,
}

impl OutboundApiGatewayGear {
    /// Database repositories when `ctx.db()` has a handle, in-memory
    /// otherwise (ADR-0018 *Backend Selection*). A non-null `database` value
    /// without a handle fails startup; `database: null` counts as absent.
    ///
    /// Without a global `database:` section the runtime builds no database
    /// manager, so `gears.oagw.database` alone yields no handle; this guard
    /// keeps that from silently dropping to in-memory storage.
    fn select_storage(
        ctx: &GearCtx,
        proxy_cache: &ProxyCacheConfig,
        metrics: &Arc<dyn OagwMetricsPort>,
    ) -> anyhow::Result<Repositories> {
        if let Some(db) = ctx.db() {
            let db = DBProvider::<RepositoryError>::new(db.db());
            info!("OAGW storage: database");
            let upstreams: Arc<dyn UpstreamRepository> = Arc::new(DbUpstreamRepo::new(db.clone()));
            let routes: Arc<dyn RouteRepository> = Arc::new(DbRouteRepo::new(db.clone()));
            let registry_rows: Arc<dyn RegistryRowRepository> =
                Arc::new(DbRegistryRowRepo::new(db.clone()));
            let registry_state: Option<Arc<dyn RegistryStateStore>> =
                Some(Arc::new(DbRegistryStateStore::new(db)));
            // With the cache disabled, resolution reads the database, and
            // there is no change log to append to or watch (ADR-0020).
            if !proxy_cache.enabled {
                info!("OAGW proxy configuration cache: disabled");
                return Ok(Repositories {
                    upstreams,
                    routes,
                    registry_rows,
                    registry_state,
                    proxy_reader: None,
                    write_hook: None,
                });
            }
            // Proxy resolution reads through the cache, and every write
            // evicts what it affects; `start` connects the hook to the
            // cluster change log for the other replicas (ADR-0020).
            let cache = Arc::new(ProxyConfigCache::new(
                proxy_cache.ttl,
                proxy_cache.max_entries,
                Arc::clone(metrics),
            ));
            let reader: Arc<dyn ProxyConfigReader> = Arc::new(CachedProxyConfigReader::new(
                Arc::clone(&cache),
                Arc::clone(&upstreams),
                Arc::clone(&routes),
            ));
            let hook = WriteHook::new(cache);
            return Ok(Repositories {
                upstreams: Arc::new(FlushingUpstreamRepo::new(upstreams, hook.clone())),
                routes: Arc::new(FlushingRouteRepo::new(routes, hook.clone())),
                registry_rows: Arc::new(FlushingRegistryRowRepo::new(registry_rows, hook.clone())),
                registry_state,
                proxy_reader: Some(reader),
                write_hook: Some(hook),
            });
        }
        let db_key_present = ctx
            .config_provider()
            .get_gear_config(Self::MODULE_NAME)
            .and_then(|gear| gear.get("database"))
            .is_some_and(|v| !v.is_null());
        if db_key_present {
            anyhow::bail!(
                "gears.oagw.database is configured but no database is available \
                 (is the global `database:` section missing?); refusing to start \
                 with in-memory storage"
            );
        }
        tracing::warn!(
            "OAGW has no database configured: upstreams and routes are kept in memory and lost on restart"
        );
        let upstreams = Arc::new(InMemoryUpstreamRepo::new());
        let routes = Arc::new(InMemoryRouteRepo::new());
        Ok(Repositories {
            registry_rows: Arc::new(InMemoryRegistryRowRepo::new(
                upstreams.clone(),
                routes.clone(),
            )),
            upstreams,
            routes,
            registry_state: None,
            proxy_reader: None,
            write_hook: None,
        })
    }
}

#[async_trait]
impl Gear for OutboundApiGatewayGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: OagwConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid OAGW config: {e}"))?;
        info!("OAGW config: proxy_timeout_secs={}", cfg.proxy_timeout_secs);

        // -- SSRF guard (shared across CP, DP, selectors) --
        let ssrf_guard = Arc::new(
            SsrfGuard::from_config(&cfg.ssrf_policy)
                .map_err(|e| anyhow::anyhow!("invalid SSRF policy config: {e}"))?,
        );
        info!(ssrf_policy = ?ssrf_guard, "SSRF guard initialized");

        // -- Metrics --
        let metrics_prefix = cfg.metrics.effective_prefix("oagw");
        let scope = opentelemetry::InstrumentationScope::builder("oagw").build();
        let metrics: Arc<dyn OagwMetricsPort> = Arc::new(OagwMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(scope),
            &metrics_prefix,
        ));

        // -- Control Plane init --
        let Repositories {
            upstreams: upstream_repo,
            routes: route_repo,
            registry_rows,
            registry_state,
            proxy_reader,
            write_hook,
        } = Self::select_storage(ctx, &cfg.proxy_cache, &metrics)?;
        if registry_state.is_some() && cfg.registry_provisioning.generation.is_none() {
            tracing::warn!(
                "registry_provisioning.generation is not set: the registry generation guard is \
                 off, and a pod of an older release can revert newer registry content \
                 (set it from the deployment, e.g. Helm .Release.Revision)"
            );
        }
        self.registry_state
            .set(registry_state)
            .map_err(|_| anyhow::anyhow!("registry state already set"))?;
        self.registry_config
            .set(cfg.registry_provisioning.clone())
            .map_err(|_| anyhow::anyhow!("registry config already set"))?;
        #[cfg(feature = "cluster")]
        self.cache_sync
            .set(write_hook.map(|hook| CacheSync {
                hub: ctx.client_hub(),
                hook,
                watch: WatchStatus::default(),
            }))
            .map_err(|_| anyhow::anyhow!("proxy cache sync already set"))?;
        #[cfg(not(feature = "cluster"))]
        if write_hook.is_some() {
            anyhow::bail!(
                "OAGW's proxy configuration cache on a database needs the cluster change log \
                 (ADR-0020), and this build of OAGW has no `cluster` feature: set \
                 `proxy_cache.enabled: false`, run OAGW in memory, or build it with the \
                 cluster (the example server's `oagw-embedded`, or OAGW's `grpc-client`)"
            );
        }
        let tenant_resolver = ctx.client_hub().get::<dyn TenantResolverClient>()?;

        let credstore = ctx.client_hub().get::<dyn CredStoreClientV1>()?;

        // -- AuthZ resolver for permission checks --
        // Consumed, not depended on: wired after `init` (local or remote), so
        // the enforcer resolves it from the hub per call.
        let policy_enforcer = PolicyEnforcer::from_hub(ctx.client_hub());

        let cp_impl = Arc::new(ControlPlaneServiceImpl::new(
            upstream_repo,
            route_repo,
            tenant_resolver.clone(),
            policy_enforcer.clone(),
            credstore.clone(),
            ssrf_guard.clone(),
            proxy_reader,
        ));
        // The registry reconcile only; handlers and the SDK get `cp`.
        self.registry_provisioner
            .set(Arc::new(RegistryProvisionerImpl::new(
                cp_impl.clone(),
                registry_rows,
            )))
            .map_err(|_| anyhow::anyhow!("RegistryProvisioner already set"))?;
        let cp: Arc<dyn ControlPlaneService> = cp_impl;

        // -- Metrics --
        let metrics_prefix = cfg.metrics.effective_prefix("oagw");
        let scope = opentelemetry::InstrumentationScope::builder("oagw").build();
        let metrics: Arc<dyn OagwMetricsPort> = Arc::new(OagwMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(scope),
            &metrics_prefix,
        ));

        // -- Data Plane init (toolkit-http-proxy upstream engine, ADR-0019) --
        let engine_settings = crate::infra::proxy::engine::EngineSettings {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(cfg.proxy_timeout_secs),
            protocol_cache_ttl: Duration::from_secs(cfg.protocol_cache_ttl_secs),
            #[cfg(feature = "test-utils")]
            skip_upstream_tls_verify: false,
        };
        let engine = Arc::new(
            crate::infra::proxy::engine::ProxyEngine::new(&engine_settings, ssrf_guard.clone())
                .map_err(|e| anyhow::anyhow!("failed to build the upstream proxy engine: {e}"))?,
        );
        let pool_selector = Arc::new(
            crate::infra::proxy::endpoint_selector::PoolEndpointSelector::new(ssrf_guard.clone()),
        );
        pool_selector.spawn_idle_sweeper(ctx.cancellation_token().clone());
        let backend_selector: Arc<dyn EndpointSelector> = pool_selector;

        let token_http_config = if cfg.allow_http_upstream {
            tracing::warn!("allow_http_upstream is enabled — HTTP token endpoints also allowed");
            let mut config = toolkit_http::HttpClientConfig::token_endpoint();
            config.transport = toolkit_http::TransportSecurity::AllowInsecureHttp;
            Some(config)
        } else {
            None
        };

        let token_cache_config = TokenCacheConfig::from(&cfg);

        let dp_impl = DataPlaneServiceImpl::new(
            cp.clone(),
            credstore,
            policy_enforcer,
            token_http_config,
            token_cache_config,
            backend_selector.clone(),
            engine,
            metrics.clone(),
        )
        .with_request_timeout(Duration::from_secs(cfg.proxy_timeout_secs))
        .with_max_body_size(cfg.max_body_size_bytes)
        .with_allow_http_upstream(cfg.allow_http_upstream)
        .with_websocket_idle_timeout(Duration::from_secs(cfg.websocket_idle_timeout_secs))
        .with_websocket_close_timeout(Duration::from_secs(cfg.websocket_close_timeout_secs))
        .with_streaming_idle_timeout(Duration::from_secs(cfg.streaming_idle_timeout_secs));
        dp_impl.spawn_rate_limit_sweeper(ctx.cancellation_token().clone());
        let dp: Arc<dyn DataPlaneService> = Arc::new(dp_impl);

        // -- Facade (for external SDK consumers) --
        let oagw: Arc<dyn ServiceGatewayClientV1> = Arc::new(ServiceGatewayClientV1Facade::new(
            cp.clone(),
            dp.clone(),
            backend_selector.clone(),
        ));

        ctx.client_hub()
            .register::<dyn ServiceGatewayClientV1>(oagw.clone());

        // -- Types Registry: register GTS schemas and builtin instances --
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let entities = oagw_gts_entities();
        let entity_count = entities.len();
        let results = registry.register(entities).await?;
        let summary = RegisterSummary::from_results(&results);
        if !summary.all_succeeded() {
            for result in &results {
                if let RegisterResult::Err { gts_id, error } = result {
                    tracing::error!(
                        gts_id = gts_id.as_deref().unwrap_or("<unknown>"),
                        error = %error,
                        "Failed to register OAGW GTS entity"
                    );
                }
            }
            anyhow::bail!(
                "OAGW type registration failed: {}/{} entities failed",
                summary.failed,
                summary.total()
            );
        }
        info!(
            count = entity_count,
            "Registered OAGW GTS entities in types-registry"
        );

        self.registry_client
            .set(registry)
            .map_err(|_| anyhow::anyhow!("TypesRegistryClient already set"))?;

        self.tenant_resolver
            .set(tenant_resolver)
            .map_err(|_| anyhow::anyhow!("TenantResolverClient already set"))?;

        self.metrics
            .set(metrics)
            .map_err(|_| anyhow::anyhow!("metrics already set"))?;

        let app_state = AppState {
            cp,
            dp,
            backend_selector,
            config: (&cfg).into(),
        };

        self.state.store(Some(Arc::new(app_state)));

        Ok(())
    }
}

#[async_trait]
impl SystemCapability for OutboundApiGatewayGear {
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let registry = self
            .registry_client
            .get()
            .ok_or_else(|| anyhow::anyhow!("TypesRegistryClient not set — init() must run first"))?
            .clone();

        let tenant_resolver = self
            .tenant_resolver
            .get()
            .ok_or_else(|| anyhow::anyhow!("TenantResolverClient not set — init() must run first"))?
            .clone();

        let provisioning: Arc<dyn TypeProvisioningService> =
            Arc::new(TypeProvisioningServiceImpl::new(registry));

        let provisioner = self
            .registry_provisioner
            .get()
            .ok_or_else(|| anyhow::anyhow!("RegistryProvisioner not set — init() must run first"))?
            .clone();

        let metrics = self
            .metrics
            .get()
            .ok_or_else(|| anyhow::anyhow!("metrics not set — init() must run first"))?
            .clone();
        let job = RegistryJob {
            cp: provisioner,
            provisioning: provisioning.clone(),
            tenant_resolver,
            metrics,
            gate: self.registry_gate()?,
            jitter,
        };
        if job.gate.is_some() {
            // A database: the background task (`start`) reconciles, so a
            // held lease or a failing dependency never delays startup; the
            // pod serves what the database holds meanwhile.
            self.registry_job
                .set(job)
                .map_err(|_| anyhow::anyhow!("registry job already set"))?;
        } else {
            // In memory: the store starts empty and no other process shares
            // it, so fill it before serving; an infrastructure error fails
            // startup.
            let outcome = job
                .run_once(&CancellationToken::new())
                .instrument(info_span!("oagw_registry_provisioning"))
                .await;
            self.provisioning.record(&outcome);
            outcome?;
        }

        self.type_provisioning
            .set(provisioning)
            .map_err(|_| anyhow::anyhow!("TypeProvisioningService already set"))?;

        Ok(())
    }
}

impl OutboundApiGatewayGear {
    /// The gate in front of the registry reconcile on a database backend;
    /// `None` in memory.
    fn registry_gate(&self) -> anyhow::Result<Option<Gate>> {
        let state = self
            .registry_state
            .get()
            .ok_or_else(|| anyhow::anyhow!("registry state not set — init() must run first"))?;
        let config = self
            .registry_config
            .get()
            .ok_or_else(|| anyhow::anyhow!("registry config not set — init() must run first"))?;
        Ok(state.as_ref().map(|state| Gate {
            state: state.clone(),
            owner: self.owner,
            generation: config.generation,
            lease_ttl: LEASE_TTL,
            poll: LEASE_POLL,
            jitter,
        }))
    }
}

/// On a database backend: the proxy configuration change feed, with the
/// cache enabled (ADR-0020 *Cluster Setup*), and the registry reconcile's background task (ADR-0018
/// *Provisioning from the Types Registry*). Pattern of the cluster gear: its
/// own child token and join handle per `start`.
#[async_trait]
impl RunnableCapability for OutboundApiGatewayGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        // A `start` after a `start` without `stop`: end what the earlier one
        // began, so none of its tasks is left running with no owner.
        if self.running() {
            warn!("OAGW started again without a stop; stopping what the earlier start began");
            self.stop(deadline_after(RESTART_STOP_GRACE)).await?;
        }
        // What can fail comes first, so a failing `start` leaves nothing
        // running. In memory, `post_init` already reconciled.
        let registry = match self.registry_job.get() {
            Some(job) => Some((
                job,
                self.registry_config.get().ok_or_else(|| {
                    anyhow::anyhow!("registry config not set — init() must run first")
                })?,
            )),
            None => None,
        };
        // Before the reconcile, so its writes reach the other replicas. A
        // missing `oagw` profile or a cache without watches is a
        // misconfiguration and fails startup.
        #[cfg(feature = "cluster")]
        if let Some(Some(sync)) = self.cache_sync.get() {
            let feed = ChangeFeed::start(
                ClusterCacheV1::resolver(&sync.hub).profile(OagwProfile),
                &sync.hook,
                self.owner,
                &sync.watch,
                cancel.child_token(),
            )
            .await
            .with_context(|| {
                format!(
                    "OAGW proxy configuration cache: cluster profile `{}`",
                    OagwProfile::NAME
                )
            })?;
            *self.change_feed.lock() = Some(feed);
        }
        let Some((job, config)) = registry else {
            return Ok(());
        };
        // A child of the runtime's root token: the task also ends when the
        // runtime shuts down without a stop phase (a later gear failed to
        // start), and `stop` cancels only this `start`'s child, so a
        // stop→start cycle gets a live one.
        let token = cancel.child_token();
        let span = info_span!(
            "oagw_registry_provisioning",
            owner = %self.owner,
            generation = ?config.generation
        );
        let handle = tokio::spawn(
            run_registry_loop(
                job.clone(),
                config.recheck_interval,
                self.provisioning.clone(),
                token.clone(),
            )
            .instrument(span),
        );
        *self.registry_task.lock() = Some((token, handle));
        // Returns at once: the start phase awaits gears in turn.
        Ok(())
    }

    async fn stop(&self, deadline: CancellationToken) -> anyhow::Result<()> {
        let task = self.registry_task.lock().take();
        if let Some((token, handle)) = task {
            stop_registry_task(token, handle, deadline.clone()).await;
        }
        // After the reconcile, whose last writes it still publishes.
        #[cfg(feature = "cluster")]
        {
            let feed = self.change_feed.lock().take();
            if let Some(feed) = feed {
                feed.stop(deadline).await;
            }
        }
        #[cfg(not(feature = "cluster"))]
        drop(deadline);
        Ok(())
    }
}

impl OutboundApiGatewayGear {
    /// Whether a task of an earlier `start` still runs.
    fn running(&self) -> bool {
        let running = self.registry_task.lock().is_some();
        #[cfg(feature = "cluster")]
        let running = running || self.change_feed.lock().is_some();
        running
    }
}

/// How long a repeated `start` waits for what the earlier one began to stop.
const RESTART_STOP_GRACE: Duration = Duration::from_secs(5);

/// A token cancelled once `after` has passed.
fn deadline_after(after: Duration) -> CancellationToken {
    let deadline = CancellationToken::new();
    let fire = deadline.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        fire.cancel();
    });
    deadline
}

/// Cancel the registry task and wait for it, aborting it at the deadline.
async fn stop_registry_task(
    token: CancellationToken,
    mut handle: tokio::task::JoinHandle<()>,
    deadline: CancellationToken,
) {
    // The run stops between writes and releases its lease.
    token.cancel();
    tokio::select! {
        joined = &mut handle => log_registry_task_end(joined),
        () = deadline.cancelled() => {
            // A run blocked in a call.
            handle.abort();
            warn!("Registry task cut off at the shutdown deadline; its lease expires with the TTL");
            log_registry_task_end(handle.await);
        }
    }
}

/// Log a panic of the registry task.
fn log_registry_task_end(joined: Result<(), tokio::task::JoinError>) {
    if let Err(e) = joined
        && e.is_panic()
    {
        error!(error = %e, "Registry task panicked");
    }
}

/// Registry provisioning in the readiness report: Degraded while a run is
/// pending, failed or not clean, never Unhealthy, so a slow or failing
/// reconcile neither takes a pod out of traffic nor restarts it.
struct ProvisioningHealth(ProvisioningState);

#[async_trait]
impl Healthcheck for ProvisioningHealth {
    fn name(&self) -> &'static str {
        "oagw-registry-provisioning"
    }

    async fn check(&self) -> HealthcheckResult {
        match self.0.get() {
            ProvisioningStatus::Settled => HealthcheckResult::healthy(),
            ProvisioningStatus::Pending => {
                HealthcheckResult::degraded("registry provisioning has not finished a run yet")
                    .with_code("registry_provisioning_pending")
            }
            ProvisioningStatus::NotClean => HealthcheckResult::degraded(
                "registry provisioning skipped instances or kept upstreams; \
                 see oagw_registry_provisioning_failures_total",
            )
            .with_code("registry_provisioning_not_clean"),
            ProvisioningStatus::Failed => {
                HealthcheckResult::degraded("the last registry provisioning run failed; retrying")
                    .with_code("registry_provisioning_failed")
            }
        }
    }
}

/// The OAGW schema migrations.
impl DatabaseCapability for OutboundApiGatewayGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        Migrator::migrations()
    }
}

/// The gear's readiness: on a database with the cache enabled the proxy
/// configuration change log watch and the cluster requirements, then
/// registry provisioning. The worst part decides the status and the code;
/// on a tie, the earlier part. The message joins those of every part that is
/// not healthy, in order, so a second degraded part is not hidden.
struct OagwHealth(Vec<Arc<dyn Healthcheck>>);

#[async_trait]
impl Healthcheck for OagwHealth {
    fn name(&self) -> &'static str {
        "oagw"
    }

    async fn check(&self) -> HealthcheckResult {
        let mut worst = HealthcheckResult::healthy();
        let mut messages = Vec::new();
        for part in &self.0 {
            let result = part.check().await;
            if result.status != HealthcheckStatus::Healthy
                && let Some(message) = &result.message
            {
                messages.push(message.clone());
            }
            if severity(result.status) > severity(worst.status) {
                worst = result;
            }
        }
        if messages.len() > 1 {
            worst.message = Some(messages.join("; "));
        }
        worst
    }
}

/// How long OAGW's healthcheck waits for the cluster SDK's readiness check:
/// half the API gateway's default 500 ms per check, so a slow cluster leaves
/// time for OAGW to answer with its own code.
#[cfg(feature = "cluster")]
const CLUSTER_CHECK_BUDGET: Duration = Duration::from_millis(250);

/// How long a cluster readiness check may run past [`CLUSTER_CHECK_BUDGET`]
/// while OAGW answers with the previous result: one refresh of the SDK's
/// descriptors.
#[cfg(feature = "cluster")]
const CLUSTER_CHECK_STALE: Duration = Duration::from_secs(10);

/// The code of a cluster readiness check that does not answer in time, or
/// fails without an answer.
#[cfg(feature = "cluster")]
const CODE_CLUSTER_READINESS_SLOW: &str = "cluster_readiness_slow";

/// The cluster SDK's readiness as a part of OAGW's. OAGW needs the cluster
/// only to keep its cache fresh, and the TTL bounds staleness without it
/// (ADR-0020 *Failure Handling*): a cluster still starting, or a profile
/// whose backend is degraded, leaves OAGW `Degraded` and proxying, with the
/// SDK's message and code. A misconfiguration stays `Unhealthy`.
///
/// The SDK's check can be slow (a remote cluster that does not answer), so
/// it runs on a task of its own, and OAGW waits for it for at most
/// [`CLUSTER_CHECK_BUDGET`]. A check still running is left to finish and
/// shared with the next call; meanwhile OAGW answers with the last result
/// for up to [`CLUSTER_CHECK_STALE`], then Degraded `cluster_readiness_slow`.
#[cfg(feature = "cluster")]
struct ClusterHealth {
    check: Arc<dyn Healthcheck>,
    state: parking_lot::Mutex<ClusterCheck>,
}

/// The check under way, if any, and the last one's result.
#[cfg(feature = "cluster")]
#[derive(Default)]
struct ClusterCheck {
    running: Option<RunningCheck>,
    last: Option<HealthcheckResult>,
}

/// A cluster readiness check under way, shared by the calls that wait for
/// it.
#[cfg(feature = "cluster")]
#[derive(Clone)]
struct RunningCheck {
    result: futures_util::future::Shared<
        futures_util::future::BoxFuture<'static, Result<HealthcheckResult, String>>,
    >,
    started: tokio::time::Instant,
    task: tokio::task::AbortHandle,
}

#[cfg(feature = "cluster")]
impl ClusterHealth {
    fn new(check: Arc<dyn Healthcheck>) -> Self {
        Self {
            check,
            state: parking_lot::Mutex::new(ClusterCheck::default()),
        }
    }

    /// The check under way, or a new one.
    fn running(&self) -> RunningCheck {
        use futures_util::FutureExt;

        let mut state = self.state.lock();
        if let Some(running) = &state.running {
            return running.clone();
        }
        let check = Arc::clone(&self.check);
        let task = tokio::spawn(async move { check.check().await });
        let abort = task.abort_handle();
        let result = async move { task.await.map_err(|e| e.to_string()) }
            .boxed()
            .shared();
        let running = RunningCheck {
            result,
            started: tokio::time::Instant::now(),
            task: abort,
        };
        state.running = Some(running.clone());
        running
    }

    /// The SDK's result as a part of OAGW's.
    fn mapped(mut result: HealthcheckResult) -> HealthcheckResult {
        use cluster_sdk::requirements::{CODE_PROFILE_DEGRADED, CODE_STARTING};

        let transient = matches!(
            result.code.as_deref(),
            Some(CODE_STARTING | CODE_PROFILE_DEGRADED)
        );
        if result.status == HealthcheckStatus::Unhealthy && transient {
            result.status = HealthcheckStatus::Degraded;
        }
        result
    }
}

#[cfg(feature = "cluster")]
impl Drop for ClusterHealth {
    fn drop(&mut self) {
        if let Some(running) = self.state.get_mut().running.take() {
            running.task.abort();
        }
    }
}

#[cfg(feature = "cluster")]
#[async_trait]
impl Healthcheck for ClusterHealth {
    fn name(&self) -> &'static str {
        "oagw-cluster"
    }

    async fn check(&self) -> HealthcheckResult {
        let running = self.running();
        let waited = tokio::time::timeout(CLUSTER_CHECK_BUDGET, running.result.clone()).await;
        let mut state = self.state.lock();
        match waited {
            Ok(ended) => {
                if state
                    .running
                    .as_ref()
                    .is_some_and(|current| current.result.ptr_eq(&running.result))
                {
                    state.running = None;
                }
                match ended {
                    Ok(result) => {
                        state.last = Some(result.clone());
                        Self::mapped(result)
                    }
                    Err(failed) => HealthcheckResult::degraded(format!(
                        "the cluster readiness check failed: {failed}"
                    ))
                    .with_code(CODE_CLUSTER_READINESS_SLOW),
                }
            }
            Err(_) => {
                let running_for = running.started.elapsed();
                match &state.last {
                    Some(last) if running_for < CLUSTER_CHECK_STALE => Self::mapped(last.clone()),
                    _ => HealthcheckResult::degraded(format!(
                        "the cluster readiness check has not answered for {} ms",
                        running_for.as_millis()
                    ))
                    .with_code(CODE_CLUSTER_READINESS_SLOW),
                }
            }
        }
    }
}

fn severity(status: HealthcheckStatus) -> u8 {
    match status {
        HealthcheckStatus::Healthy => 0,
        HealthcheckStatus::Degraded => 1,
        HealthcheckStatus::Unhealthy => 2,
    }
}

impl OutboundApiGatewayGear {
    /// On a database with the cache enabled: the change log watch and the
    /// cluster requirements.
    #[cfg(feature = "cluster")]
    fn cache_health(&self) -> Vec<Arc<dyn Healthcheck>> {
        let mut parts: Vec<Arc<dyn Healthcheck>> = Vec::new();
        if let Some(Some(sync)) = self.cache_sync.get() {
            parts.push(Arc::new(WatchHealth(sync.watch.clone())));
            parts.push(Arc::new(ClusterHealth::new(
                cluster_sdk::cluster_readiness(Arc::clone(&sync.hub)),
            )));
        }
        parts
    }

    /// Nothing: without the cluster, OAGW caches nothing.
    #[cfg(not(feature = "cluster"))]
    #[expect(
        clippy::unused_self,
        reason = "the same signature as the `cluster` build's, which reads the cache sync"
    )]
    fn cache_health(&self) -> Vec<Arc<dyn Healthcheck>> {
        Vec::new()
    }
}

impl RestApiCapability for OutboundApiGatewayGear {
    /// Holds the shared provisioning and watch state, so it answers before
    /// `start` and after `stop` too. The cache's parts come first, so on a
    /// tie their code wins over provisioning's.
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        let mut parts = self.cache_health();
        parts.push(Arc::new(ProvisioningHealth(self.provisioning.clone())));
        Some(Arc::new(OagwHealth(parts)))
    }

    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let state = self
            .state
            .load()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OAGW gear not initialized — call init() first"))?
            .as_ref()
            .clone();

        let mgmt_enabled = state.config.management_api_enabled;
        info!(
            management_api_enabled = mgmt_enabled,
            "Registering OAGW REST routes"
        );

        let router = routes::register_routes(router, openapi, state);
        Ok(router)
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
