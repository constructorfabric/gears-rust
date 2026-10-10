use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::config::{OagwConfig, TokenCacheConfig};
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
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::SystemCapability;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use toolkit_security::SecurityContext;
use tracing::info;
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::api::rest::routes;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::repo::{RepositoryError, RouteRepository, UpstreamRepository};
use crate::domain::services::registry_reconcile::reconcile_registry;
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, DataPlaneService, EndpointSelector,
    RegistryProvisioner, ServiceGatewayClientV1Facade,
};
use crate::infra::metrics::OagwMetricsMeter;
use crate::infra::proxy::DataPlaneServiceImpl;
use crate::infra::storage::{
    DbRouteRepo, DbUpstreamRepo, InMemoryRouteRepo, InMemoryUpstreamRepo, Migrator,
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
    capabilities = [system, db, rest]
)]
pub struct OutboundApiGatewayGear {
    state: arc_swap::ArcSwapOption<AppState>,
    registry_client: OnceLock<Arc<dyn TypesRegistryClient>>,
    tenant_resolver: OnceLock<Arc<dyn TenantResolverClient>>,
    type_provisioning: OnceLock<Arc<dyn TypeProvisioningService>>,
    registry_provisioner: OnceLock<Arc<dyn RegistryProvisioner>>,
}

impl Default for OutboundApiGatewayGear {
    fn default() -> Self {
        Self {
            state: arc_swap::ArcSwapOption::from(None),
            registry_client: OnceLock::new(),
            tenant_resolver: OnceLock::new(),
            type_provisioning: OnceLock::new(),
            registry_provisioner: OnceLock::new(),
        }
    }
}

/// The upstream and route repositories the Control Plane runs on.
type Repositories = (Arc<dyn UpstreamRepository>, Arc<dyn RouteRepository>);

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
            return Ok((
                Arc::new(DbUpstreamRepo::new(db.clone())),
                Arc::new(DbRouteRepo::new(db)),
            ));
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
        Ok((
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
        ))
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
        let (upstream_repo, route_repo) = Self::select_storage(ctx)?;
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
        // Only `post_init` holds the registry write path; handlers and the SDK
        // facade get the Control Plane trait alone.
        let cp: Arc<dyn ControlPlaneService> = cp_impl.clone();
        self.registry_provisioner
            .set(cp_impl)
            .map_err(|_| anyhow::anyhow!("RegistryProvisioner already set"))?;

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
            metrics,
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

        // Resolve root tenant for provisioning context.
        let bootstrap_ctx = SecurityContext::builder()
            .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
            .subject_tenant_id(toolkit_security::constants::DEFAULT_TENANT_ID)
            .build()?;
        let root_tenant_id = tenant_resolver
            .get_root_tenant(&bootstrap_ctx)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve root tenant: {e}"))?
            .id
            .0;

        // -- Materialise upstreams and routes from types-registry --
        let upstreams = provisioning.list_upstreams().await?;
        let routes = provisioning.list_routes().await?;
        let counts = reconcile_registry(&*provisioner, &upstreams, &routes, root_tenant_id).await?;
        info!(
            upstreams_created = counts.upstreams.created,
            upstreams_updated = counts.upstreams.updated,
            upstreams_unchanged = counts.upstreams.unchanged,
            upstreams_removed = counts.upstreams.removed,
            upstreams_kept = counts.upstreams.kept,
            routes_created = counts.routes.created,
            routes_updated = counts.routes.updated,
            routes_unchanged = counts.routes.unchanged,
            routes_removed = counts.routes.removed,
            "Type provisioning complete"
        );

        self.type_provisioning
            .set(provisioning)
            .map_err(|_| anyhow::anyhow!("TypeProvisioningService already set"))?;

        Ok(())
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
