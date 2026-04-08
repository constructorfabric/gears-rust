use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, OnceLock};

use modkit::Module;
use modkit::context::ModuleCtx;
use modkit::contracts::{OpenApiRegistry, RestApiCapability};

use crate::domain::local_client::NodesRegistryLocalClient;
use crate::domain::service::Service;
use nodes_registry_sdk::NodesRegistryClient;

#[modkit::module(
    name = "nodes-registry",
    capabilities = [rest],
    client = nodes_registry_sdk::NodesRegistryClient
)]
pub struct NodesRegistry {
    service: OnceLock<Arc<Service>>,
}

impl Default for NodesRegistry {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for NodesRegistry {
    async fn init(&self, ctx: &ModuleCtx) -> Result<()> {
        // Create the service
        let service = Arc::new(Service::new());
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Expose the client to the ClientHub
        let api: Arc<dyn NodesRegistryClient> = Arc::new(NodesRegistryLocalClient::new(service));
        ctx.client_hub().register::<dyn NodesRegistryClient>(api);

        Ok(())
    }
}

impl RestApiCapability for NodesRegistry {
    fn register_rest(
        &self,
        _ctx: &ModuleCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        let router = crate::api::rest::routes::register_routes(router, openapi, service);

        tracing::info!("Nodes registry REST routes registered");
        Ok(router)
    }
}
