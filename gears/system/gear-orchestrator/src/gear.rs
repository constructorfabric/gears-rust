//! Gear definition for `GearOrchestrator`

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, OnceLock};

use toolkit::DirectoryClient;
use toolkit::context::GearCtx;
use toolkit::contracts::{
    GrpcServiceCapability, OpenApiRegistry, RegisterGrpcServiceFn, RestApiCapability,
    SystemCapability,
};
use toolkit::directory::LocalDirectoryClient;
use toolkit::registry::GearRegistry;
use toolkit::runtime::GearManager;

use cf_system_sdks::directory::DIRECTORY_SERVICE_NAME;

use crate::domain::service::GearsService;
use crate::server;

/// Gear Orchestrator - system gear for service discovery
///
/// This gear:
/// - Provides `DirectoryClient` to the `ClientHub` for in-process gears
/// - Exposes `DirectoryService` gRPC service via `grpc-hub`
/// - Tracks gear instances and provides service resolution
/// - Exposes REST API to list all registered gears
#[toolkit::gear(
    name = "gear-orchestrator",
    capabilities = [grpc, system, rest],
    client = cf_system_sdks::directory::DirectoryClient
)]
pub struct GearOrchestrator {
    directory_api: OnceLock<Arc<dyn DirectoryClient>>,
    gear_manager: OnceLock<Arc<GearManager>>,
    gears_service: OnceLock<Arc<GearsService>>,
}

impl Default for GearOrchestrator {
    fn default() -> Self {
        Self {
            directory_api: OnceLock::new(),
            gear_manager: OnceLock::new(),
            gears_service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl SystemCapability for GearOrchestrator {
    fn pre_init(&self, sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        self.gear_manager
            .set(Arc::clone(&sys.gear_manager))
            .map_err(|_| anyhow::anyhow!("GearManager already set (pre_init called twice?)"))?;
        Ok(())
    }
}

#[async_trait]
impl toolkit::Gear for GearOrchestrator {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        // Migration guard: platform-plane enforcement lives on `grpc-hub` (a
        // transport-level `InternalAuthGrpcLayer` applied to every inbound
        // RPC, `cpt-cf-adr-platform-plane-auth`), not here. This gear reads no
        // config at all, so a leftover `internal_auth` key would silently do
        // nothing — fail loudly instead of booting the `DirectoryService`
        // unauthenticated.
        if let Some(internal_auth) = ctx
            .config_provider()
            .get_gear_config(ctx.gear_name())
            .and_then(|raw| raw.get("config"))
            .and_then(|config| config.get("internal_auth"))
        {
            let provider = internal_auth.get("provider").and_then(|p| p.as_str());
            let detail = provider.map_or_else(String::new, |p| format!(" (provider = {p:?})"));
            anyhow::bail!(
                "gear-orchestrator config still sets `internal_auth`{detail}, which no longer \
                 has any effect: platform-plane enforcement moved to grpc-hub's own \
                 `internal_auth` config (cpt-cf-adr-platform-plane-auth). Move this key under \
                 `gears.grpc-hub.config` instead."
            );
        }

        // Use the injected GearManager to create the DirectoryClient
        let manager = self
            .gear_manager
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("GearManager not wired into GearOrchestrator"))?;

        let api_impl: Arc<dyn DirectoryClient> =
            Arc::new(LocalDirectoryClient::new(manager.clone()));

        // Register in ClientHub directly
        ctx.client_hub()
            .register::<dyn DirectoryClient>(api_impl.clone());

        self.directory_api
            .set(api_impl)
            .map_err(|_| anyhow::anyhow!("DirectoryClient already set (init called twice?)"))?;

        // Build compiled-gear catalog from inventory and create the GearsService
        let registry = GearRegistry::discover_and_build()
            .map_err(|e| anyhow::anyhow!("Failed to build gear registry: {e}"))?;
        let gears_service = Arc::new(GearsService::new(&registry, manager));
        self.gears_service
            .set(gears_service)
            .map_err(|_| anyhow::anyhow!("GearsService already set (init called twice?)"))?;

        tracing::info!("GearOrchestrator initialized");

        Ok(())
    }
}

impl RestApiCapability for GearOrchestrator {
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

        tracing::info!("GearOrchestrator REST routes registered");
        Ok(router)
    }
}

/// Export gRPC services to `grpc-hub`
#[async_trait]
impl GrpcServiceCapability for GearOrchestrator {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> Result<Vec<RegisterGrpcServiceFn>> {
        let api = self
            .directory_api
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("DirectoryClient not initialized"))?;

        let directory_svc = server::make_directory_service(api);

        Ok(vec![RegisterGrpcServiceFn {
            service_name: DIRECTORY_SERVICE_NAME,
            register: Box::new(move |routes| {
                routes.add_service(directory_svc.clone());
            }),
        }])
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use toolkit::client_hub::ClientHub;
    use toolkit::config::ConfigProvider;
    use uuid::Uuid;

    struct StaticConfigProvider(serde_json::Value);
    impl ConfigProvider for StaticConfigProvider {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            (gear_name == "gear-orchestrator").then_some(&self.0)
        }
    }

    async fn init_with_config(config: serde_json::Value) -> Result<()> {
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("config".to_owned(), config);
        let ctx = GearCtx::new(
            "gear-orchestrator",
            Uuid::new_v4(),
            Arc::new(StaticConfigProvider(serde_json::Value::Object(wrapped))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );
        let gear = GearOrchestrator::default();
        let gear_manager = Arc::new(GearManager::new());
        gear.gear_manager
            .set(gear_manager)
            .map_err(|_| anyhow::anyhow!("gear_manager already set"))?;
        toolkit::Gear::init(&gear, &ctx).await
    }

    #[tokio::test]
    async fn init_rejects_leftover_internal_auth_config() {
        let err = init_with_config(serde_json::json!({
            "internal_auth": { "provider": "shared_secret", "secret": "top-secret-value" }
        }))
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("internal_auth"),
            "expected a migration error mentioning internal_auth, got {err}"
        );
        assert!(
            msg.contains("grpc-hub"),
            "expected the error to point at grpc-hub's config, got {err}"
        );
        assert!(
            msg.contains("shared_secret"),
            "expected the non-sensitive provider tag to be named, got {err}"
        );
        assert!(
            !msg.contains("top-secret-value"),
            "the error must never leak the secret value, got {err}"
        );
    }

    #[tokio::test]
    async fn init_succeeds_without_internal_auth_config() {
        init_with_config(serde_json::json!({}))
            .await
            .expect("init without a leftover internal_auth key must succeed");
    }
}
