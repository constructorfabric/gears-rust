use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::config::{OagwConfig, TokenCacheConfig};
use crate::domain::ssrf::SsrfGuard;
use crate::domain::type_catalog::oagw_gts_entities;
use crate::domain::type_provisioning::TypeProvisioningService;
use crate::infra::type_provisioning::TypeProvisioningServiceImpl;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::SystemCapability;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_security::SecurityContext;
use tracing::info;
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::api::rest::routes;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, DataPlaneService, EndpointSelector,
    OAuthEnrollmentService, ServiceGatewayClientV1Facade,
};
use crate::infra::metrics::OagwMetricsMeter;
use crate::infra::proxy::DataPlaneServiceImpl;
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

/// Shared application state injected into all handlers.
#[derive(Clone)]
pub struct AppState {
    pub(crate) cp: Arc<dyn ControlPlaneService>,
    pub(crate) dp: Arc<dyn DataPlaneService>,
    pub(crate) oauth: Arc<dyn OAuthEnrollmentService>,
    pub(crate) backend_selector: Arc<dyn EndpointSelector>,
    pub(crate) config: crate::config::RuntimeConfig,
}

/// Outbound API Gateway gear: wires repos, services, and routes.
#[toolkit::gear(
    name = "oagw",
    deps = [types_registry, authz_resolver, credstore, tenant_resolver],
    capabilities = [system, rest]
)]
pub struct OutboundApiGatewayGear {
    state: arc_swap::ArcSwapOption<AppState>,
    registry_client: OnceLock<Arc<dyn TypesRegistryClient>>,
    tenant_resolver: OnceLock<Arc<dyn TenantResolverClient>>,
    type_provisioning: OnceLock<Arc<dyn TypeProvisioningService>>,
}

impl Default for OutboundApiGatewayGear {
    fn default() -> Self {
        Self {
            state: arc_swap::ArcSwapOption::from(None),
            registry_client: OnceLock::new(),
            tenant_resolver: OnceLock::new(),
            type_provisioning: OnceLock::new(),
        }
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
        let upstream_repo = Arc::new(InMemoryUpstreamRepo::new());
        let route_repo = Arc::new(InMemoryRouteRepo::new());
        let tenant_resolver = ctx.client_hub().get::<dyn TenantResolverClient>()?;

        let credstore = ctx.client_hub().get::<dyn CredStoreClientV1>()?;

        // -- AuthZ resolver for permission checks --
        let authz = ctx.client_hub().get::<dyn AuthZResolverClient>()?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let cp: Arc<dyn ControlPlaneService> = Arc::new(ControlPlaneServiceImpl::new(
            upstream_repo,
            route_repo,
            tenant_resolver.clone(),
            policy_enforcer.clone(),
            credstore.clone(),
            ssrf_guard.clone(),
        ));

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
        let backend_selector: Arc<dyn EndpointSelector> = Arc::new(
            crate::infra::proxy::pingora_proxy::PingoraEndpointSelector::new(ssrf_guard.clone()),
        );

        let token_http_config = if cfg.allow_http_upstream {
            tracing::warn!("allow_http_upstream is enabled — HTTP token endpoints also allowed");
            let mut config = toolkit_http::HttpClientConfig::token_endpoint();
            config.transport = toolkit_http::TransportSecurity::AllowInsecureHttp;
            Some(config)
        } else {
            None
        };

        // Effective token HTTP policy, honoring `allow_http_upstream`, shared by
        // both the data plane and the interactive OAuth enrollment service so
        // discovery, registration, and code exchange match the data plane's
        // transport security.
        let oauth_http_config = token_http_config
            .clone()
            .unwrap_or_else(toolkit_http::HttpClientConfig::token_endpoint);

        let token_cache_config = TokenCacheConfig::from(&cfg);

        // Shared per-user token store: the single seam through which the
        // `oauth2_auth_code` plugin (reader) and the enrollment service (writer)
        // agree on the storage key derived from `(subject, upstream_id)`.
        let token_store: Arc<dyn crate::infra::oauth::UserTokenStore> = Arc::new(
            crate::infra::oauth::CredStoreUserTokenStore::new(credstore.clone()),
        );
        // Transient PKCE/CSRF state lives only in memory with a TTL; never in
        // durable secret storage.
        let pending = Arc::new(
            crate::infra::oauth::pending_store::PendingAuthorizationStore::new(
                Duration::from_secs(600),
            ),
        );

        let dp: Arc<dyn DataPlaneService> = Arc::new(
            DataPlaneServiceImpl::new(
                cp.clone(),
                credstore.clone(),
                token_store.clone(),
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
            .with_websocket_max_frame_size(cfg.websocket_max_frame_size_bytes)
            .with_streaming_idle_timeout(Duration::from_secs(cfg.streaming_idle_timeout_secs)),
        );

        // -- Interactive OAuth enrollment service --
        let oauth: Arc<dyn OAuthEnrollmentService> = Arc::new(
            crate::infra::oauth::OAuthEnrollmentServiceImpl::with_http_config(
                cp.clone(),
                token_store,
                pending,
                cfg.oauth_callback_url.clone(),
                cfg.oauth_return_to_allowlist.clone(),
                oauth_http_config,
            ),
        );

        // -- Facade (for external SDK consumers) --
        let oagw: Arc<dyn ServiceGatewayClientV1> =
            Arc::new(ServiceGatewayClientV1Facade::new(cp.clone(), dp.clone()));

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
            oauth,
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

        // -- Materialize provisioned upstreams and routes into in-memory repos --
        let app_state = self
            .state
            .load()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AppState not set — init() must run first"))?
            .as_ref()
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
        // GTS instance UUIDs are passed through as `CreateUpstreamRequest.id`
        // and `CreateRouteRequest.id`, so OAGW uses the config-provided IDs
        // directly. Route `upstream_id` already references the upstream's GTS
        // instance UUID, so no remapping is needed.
        let upstreams = provisioning.list_upstreams().await?;
        for u in &upstreams {
            let tenant_id = u.tenant_id.unwrap_or(root_tenant_id);
            let ctx = SecurityContext::builder()
                .subject_tenant_id(tenant_id)
                .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
                .build()?;
            let created = app_state
                .cp
                .create_upstream(&ctx, u.request.clone())
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to provision upstream (tenant={tenant_id}): {e}")
                })?;
            info!(
                id = %created.id,
                tenant_id = %tenant_id,
                alias = %created.alias,
                "Provisioned upstream from types-registry"
            );
        }

        let routes = provisioning.list_routes().await?;
        for r in &routes {
            let tenant_id = r.tenant_id.unwrap_or(root_tenant_id);
            let ctx = SecurityContext::builder()
                .subject_tenant_id(tenant_id)
                .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
                .build()?;
            let created = app_state
                .cp
                .create_route(&ctx, r.request.clone())
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to provision route (tenant={tenant_id}): {e}")
                })?;
            info!(
                id = %created.id,
                tenant_id = %tenant_id,
                "Provisioned route from types-registry"
            );
        }

        info!(
            upstreams = upstreams.len(),
            routes = routes.len(),
            "Type provisioning complete"
        );

        self.type_provisioning
            .set(provisioning)
            .map_err(|_| anyhow::anyhow!("TypeProvisioningService already set"))?;

        Ok(())
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
