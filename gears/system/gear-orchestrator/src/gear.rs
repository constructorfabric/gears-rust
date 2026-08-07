//! Gear definition for `GearOrchestrator`

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

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
use toolkit_security::{DynInternalAuthenticator, InternalAuthConfig};

use crate::domain::service::GearsService;
use crate::server;

/// Configuration for the gear orchestrator
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct GearOrchestratorConfig {
    /// Platform-plane (internal) authentication for the `DirectoryService`
    /// gRPC RPCs. When set, every RPC requires a valid
    /// `x-toolkit-internal-token` (`cpt-cf-adr-two-plane-auth`); when absent,
    /// enforcement is disabled (Profile 1 / in-process only).
    #[serde(default)]
    pub internal_auth: Option<toolkit_security::InternalAuthConfig>,
}

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
    config: RwLock<GearOrchestratorConfig>,
    directory_api: OnceLock<Arc<dyn DirectoryClient>>,
    gear_manager: OnceLock<Arc<GearManager>>,
    gears_service: OnceLock<Arc<GearsService>>,
    /// Platform-plane validator built from `config.internal_auth`, applied to
    /// the `DirectoryService` gRPC RPCs. `None` disables enforcement.
    authenticator: OnceLock<Option<DynInternalAuthenticator>>,
}

impl Default for GearOrchestrator {
    fn default() -> Self {
        Self {
            config: RwLock::new(GearOrchestratorConfig::default()),
            directory_api: OnceLock::new(),
            gear_manager: OnceLock::new(),
            gears_service: OnceLock::new(),
            authenticator: OnceLock::new(),
        }
    }
}

/// Build the platform-plane authenticator from configuration.
///
/// The dependency-light shared-secret provider is built here directly. The
/// Kubernetes `TokenReview` provider requires the `k8s-auth` feature; without
/// it, configuring `provider: kube` is a hard error rather than a silent
/// enforcement downgrade.
#[cfg_attr(not(feature = "k8s-auth"), allow(clippy::unused_async))]
async fn build_internal_authenticator(
    cfg: Option<&InternalAuthConfig>,
) -> Result<Option<DynInternalAuthenticator>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };

    // Shared-secret (and any future dependency-light provider) builds here.
    if let Some(auth) = cfg.build_authenticator() {
        return Ok(Some(auth));
    }

    #[cfg(feature = "k8s-auth")]
    {
        if cfg.is_kube() {
            let audiences = cfg.kube_audiences().unwrap_or_default().to_vec();
            let authenticator =
                toolkit_k8s_auth::K8sTokenReviewAuthenticator::try_default(audiences)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("failed to init Kubernetes TokenReview authenticator: {e}")
                    })?;
            return Ok(Some(DynInternalAuthenticator::new(authenticator)));
        }
    }
    #[cfg(not(feature = "k8s-auth"))]
    {
        if cfg.is_kube() {
            anyhow::bail!(
                "gear-orchestrator internal_auth provider=kube requires the `k8s-auth` feature"
            );
        }
    }

    Ok(None)
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
        // Load configuration if present
        let cfg = ctx.config_or_default::<GearOrchestratorConfig>()?;

        // Build the platform-plane validator (if any) before moving `cfg`.
        let authenticator = build_internal_authenticator(cfg.internal_auth.as_ref()).await?;
        if authenticator.is_some() {
            tracing::info!("DirectoryService platform-plane enforcement enabled");
        }
        self.authenticator
            .set(authenticator)
            .map_err(|_| anyhow::anyhow!("authenticator already set (init called twice?)"))?;

        *self.config.write().await = cfg;

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

        // Build DirectoryService with the configured platform-plane validator.
        let authenticator = self.authenticator.get().cloned().flatten();
        let directory_svc = server::make_directory_service(api, authenticator);

        Ok(vec![RegisterGrpcServiceFn {
            service_name: DIRECTORY_SERVICE_NAME,
            register: Box::new(move |routes| {
                routes.add_service(directory_svc.clone());
            }),
        }])
    }
}
