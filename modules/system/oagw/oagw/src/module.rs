use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::config::{OagwConfig, TokenCacheConfig};
use crate::domain::type_catalog::oagw_gts_entities;
use crate::domain::type_provisioning::TypeProvisioningService;
use crate::infra::type_provisioning::TypeProvisioningServiceImpl;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use credstore_sdk::CredStoreClientV1;
use modkit::api::OpenApiRegistry;
use modkit::contracts::SystemCapability;
use modkit::{Module, ModuleCtx, RestApiCapability};
use modkit_security::SecurityContext;
use oagw_sdk::api::ServiceGatewayClientV1;
use tenant_resolver_sdk::TenantResolverClient;
use tracing::info;
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::api::rest::routes;
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, DataPlaneService, EndpointSelector,
    ServiceGatewayClientV1Facade,
};
use crate::infra::proxy::DataPlaneServiceImpl;
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

/// Shared application state injected into all handlers.
#[derive(Clone)]
pub struct AppState {
    pub(crate) cp: Arc<dyn ControlPlaneService>,
    pub(crate) dp: Arc<dyn DataPlaneService>,
    pub(crate) backend_selector: Arc<dyn EndpointSelector>,
    pub(crate) config: crate::config::RuntimeConfig,
}

/// Outbound API Gateway module: wires repos, services, and routes.
#[modkit::module(
    name = "oagw",
    deps = ["types-registry", "authz-resolver", "credstore", "tenant-resolver"],
    capabilities = [system, rest]
)]
pub struct OutboundApiGatewayModule {
    state: arc_swap::ArcSwapOption<AppState>,
    registry_client: OnceLock<Arc<dyn TypesRegistryClient>>,
    type_provisioning: OnceLock<Arc<dyn TypeProvisioningService>>,
}

impl Default for OutboundApiGatewayModule {
    fn default() -> Self {
        Self {
            state: arc_swap::ArcSwapOption::from(None),
            registry_client: OnceLock::new(),
            type_provisioning: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for OutboundApiGatewayModule {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: OagwConfig = ctx.config()?;
        info!("OAGW config: proxy_timeout_secs={}", cfg.proxy_timeout_secs);

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
            tenant_resolver,
            policy_enforcer.clone(),
            credstore.clone(),
        ));

        // -- Data Plane init (Pingora proxy engine) --
        let server_conf = Arc::new(pingora_core::server::configuration::ServerConf {
            upstream_keepalive_pool_size: 128,
            ..Default::default()
        });
        let connect_timeout = Duration::from_secs(10);
        let read_timeout = Duration::from_secs(cfg.proxy_timeout_secs);
        let pingora_proxy =
            crate::infra::proxy::pingora_proxy::PingoraProxy::new(connect_timeout, read_timeout);
        let proxy = Arc::new(crate::infra::proxy::pingora_proxy::new_http_proxy(
            &server_conf,
            pingora_proxy,
        ));
        let backend_selector: Arc<dyn EndpointSelector> =
            Arc::new(crate::infra::proxy::pingora_proxy::PingoraEndpointSelector::new());

        let token_http_config = if cfg.allow_http_upstream {
            tracing::warn!("allow_http_upstream is enabled — HTTP token endpoints also allowed");
            let mut config = modkit_http::HttpClientConfig::token_endpoint();
            config.transport = modkit_http::TransportSecurity::AllowInsecureHttp;
            Some(config)
        } else {
            None
        };

        let token_cache_config = TokenCacheConfig::from(&cfg);

        let dp: Arc<dyn DataPlaneService> = Arc::new(
            DataPlaneServiceImpl::new(
                cp.clone(),
                credstore,
                policy_enforcer,
                token_http_config,
                token_cache_config,
                backend_selector.clone(),
                proxy,
            )
            .with_request_timeout(Duration::from_secs(cfg.proxy_timeout_secs))
            .with_max_body_size(cfg.max_body_size_bytes)
            .with_allow_http_upstream(cfg.allow_http_upstream),
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
impl SystemCapability for OutboundApiGatewayModule {
    async fn post_init(&self, _sys: &modkit::runtime::SystemContext) -> anyhow::Result<()> {
        let registry = self
            .registry_client
            .get()
            .ok_or_else(|| anyhow::anyhow!("TypesRegistryClient not set — init() must run first"))?
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

        // -- Materialise upstreams, building a GTS-instance-UUID -> OAGW-UUID map --
        // Routes registered via types-registry reference upstreams by the
        // deterministic GTS instance UUID. OAGW assigns random UUIDs, so we
        // need to rewrite route upstream_ids before creating them.
        let upstreams = provisioning.list_upstreams().await?;
        let mut gts_to_oagw: std::collections::HashMap<uuid::Uuid, uuid::Uuid> =
            std::collections::HashMap::new();
        for u in &upstreams {
            let ctx = SecurityContext::builder()
                .subject_tenant_id(u.tenant_id)
                .subject_id(modkit_security::constants::DEFAULT_SUBJECT_ID)
                .build()?;
            let created = app_state
                .cp
                .create_upstream(&ctx, u.request.clone())
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to provision upstream (tenant={}): {e}", u.tenant_id)
                })?;
            if let Some(gts_id) = u.gts_instance_id {
                gts_to_oagw.insert(gts_id, created.id);
            }
            info!(
                id = %created.id,
                tenant_id = %u.tenant_id,
                alias = %created.alias,
                "Provisioned upstream from types-registry"
            );
        }

        let routes = provisioning.list_routes().await?;
        for r in &routes {
            let ctx = SecurityContext::builder()
                .subject_tenant_id(r.tenant_id)
                .subject_id(modkit_security::constants::DEFAULT_SUBJECT_ID)
                .build()?;
            // Rewrite upstream_id if it references a GTS instance UUID.
            let mut req = r.request.clone();
            if let Some(&oagw_id) = gts_to_oagw.get(&req.upstream_id) {
                req.upstream_id = oagw_id;
            }
            let created = app_state.cp.create_route(&ctx, req).await.map_err(|e| {
                anyhow::anyhow!("Failed to provision route (tenant={}): {e}", r.tenant_id)
            })?;
            info!(
                id = %created.id,
                tenant_id = %r.tenant_id,
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

impl RestApiCapability for OutboundApiGatewayModule {
    fn register_rest(
        &self,
        _ctx: &ModuleCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering OAGW REST routes");

        let state = self
            .state
            .load()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OAGW module not initialized — call init() first"))?
            .as_ref()
            .clone();

        let router = routes::register_routes(router, openapi, state);
        Ok(router)
    }
}
