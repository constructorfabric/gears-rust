use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::config::{OagwConfig, RegistryProvisioningConfig, TokenCacheConfig};
use crate::domain::ssrf::SsrfGuard;
use crate::domain::type_catalog::oagw_gts_entities;
use crate::domain::type_provisioning::TypeProvisioningService;
use crate::infra::type_provisioning::TypeProvisioningServiceImpl;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use sea_orm_migration::MigrationTrait;
use tenant_resolver_sdk::TenantResolverClient;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::{RunnableCapability, SystemCapability};
use toolkit::{
    DatabaseCapability, Gear, GearCtx, Healthcheck, HealthcheckResult, RestApiCapability,
};
use toolkit_db::DBProvider;
use tracing::{Instrument, error, info, info_span, warn};
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::api::rest::routes;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::repo::{
    RegistryRowRepository, RegistryStateStore, RepositoryError, RouteRepository, UpstreamRepository,
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
#[toolkit::gear(
    name = "oagw",
    deps = [types_registry, authz_resolver, credstore, tenant_resolver],
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
    /// This process, as the registry lease owner.
    owner: uuid::Uuid,
    /// The registry run `start` drives on a database backend.
    registry_job: OnceLock<RegistryJob>,
    /// The running registry task, from `start` to `stop`.
    registry_task: parking_lot::Mutex<Option<(CancellationToken, tokio::task::JoinHandle<()>)>>,
    /// Where provisioning stands, for the healthcheck.
    provisioning: ProvisioningState,
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
}

impl OutboundApiGatewayGear {
    /// Database repositories when `ctx.db()` has a handle, in-memory
    /// otherwise (ADR-0018 *Backend Selection*). A non-null `database` value
    /// without a handle fails startup; `database: null` counts as absent.
    ///
    /// Without a global `database:` section the runtime builds no database
    /// manager, so `gears.oagw.database` alone yields no handle; this guard
    /// keeps that from silently dropping to in-memory storage.
    fn select_storage(ctx: &GearCtx) -> anyhow::Result<Repositories> {
        if let Some(db) = ctx.db() {
            let db = DBProvider::<RepositoryError>::new(db.db());
            info!("OAGW storage: database");
            return Ok(Repositories {
                upstreams: Arc::new(DbUpstreamRepo::new(db.clone())),
                routes: Arc::new(DbRouteRepo::new(db.clone())),
                registry_rows: Arc::new(DbRegistryRowRepo::new(db.clone())),
                registry_state: Some(Arc::new(DbRegistryStateStore::new(db))),
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

        // -- Control Plane init --
        let Repositories {
            upstreams: upstream_repo,
            routes: route_repo,
            registry_rows,
            registry_state,
        } = Self::select_storage(ctx)?;
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
        let tenant_resolver = ctx.client_hub().get::<dyn TenantResolverClient>()?;

        let credstore = ctx.client_hub().get::<dyn CredStoreClientV1>()?;

        // -- AuthZ resolver for permission checks --
        let authz = ctx.client_hub().get::<dyn AuthZResolverApi>()?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let cp_impl = Arc::new(ControlPlaneServiceImpl::new(
            upstream_repo,
            route_repo,
            tenant_resolver.clone(),
            policy_enforcer.clone(),
            credstore.clone(),
            ssrf_guard.clone(),
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

        // -- Data Plane init (Pingora proxy engine) --
        let server_conf = Arc::new(pingora_core::server::configuration::ServerConf {
            upstream_keepalive_pool_size: 128,
            ..Default::default()
        });
        let connect_timeout = Duration::from_secs(10);
        let read_timeout = Duration::from_secs(cfg.proxy_timeout_secs);
        let protocol_cache_ttl = Duration::from_secs(cfg.protocol_cache_ttl_secs);
        let pingora_proxy = crate::infra::proxy::pingora_proxy::PingoraProxy::new(
            connect_timeout,
            read_timeout,
            protocol_cache_ttl,
            ssrf_guard.clone(),
        );
        let proxy = Arc::new(crate::infra::proxy::pingora_proxy::new_http_proxy(
            &server_conf,
            pingora_proxy,
        ));
        let pingora_selector = Arc::new(
            crate::infra::proxy::pingora_proxy::PingoraEndpointSelector::new(ssrf_guard.clone()),
        );
        pingora_selector.spawn_idle_sweeper(ctx.cancellation_token().clone());
        let backend_selector: Arc<dyn EndpointSelector> = pingora_selector;

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
            proxy,
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

/// The registry reconcile's background task, on a database backend
/// (ADR-0018 *Provisioning from the Types Registry*). Pattern of the cluster gear: its own
/// child token and join handle per `start`.
#[async_trait]
impl RunnableCapability for OutboundApiGatewayGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        // In memory, `post_init` already reconciled.
        let Some(job) = self.registry_job.get() else {
            return Ok(());
        };
        let config = self
            .registry_config
            .get()
            .ok_or_else(|| anyhow::anyhow!("registry config not set — init() must run first"))?;
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
            stop_registry_task(token, handle, deadline).await;
        }
        Ok(())
    }
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

impl RestApiCapability for OutboundApiGatewayGear {
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(ProvisioningHealth(self.provisioning.clone())))
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
