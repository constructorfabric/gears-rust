//! `llm-gateway` gear entry point.

use std::sync::{Arc, OnceLock};

use anyhow::Context;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use llm_gateway_sdk::LlmGatewayClientV1;
use model_registry_sdk::ModelRegistryClientV1;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};

use crate::api::rest::routes;
use crate::config::LlmGatewayConfig;
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::{KeylessCredentialBroker, LlmGatewayLocalClient, Service};
use crate::infra::model_registry::ModelRegistryResolver;
use crate::infra::plugin_directory::GtsPluginDirectory;
use crate::infra::tool_schemas::TypesRegistryToolSchemas;

/// LLM Gateway gear: wires the domain service, registers
/// `Arc<dyn LlmGatewayClientV1>` in `ClientHub`, and mounts the REST routes.
///
/// The `LlmGatewayProviderPluginSpecV1` GTS schema reaches `types-registry`
/// through the `toolkit-gts` link-time inventory, so no per-init
/// registration call is needed.
#[toolkit::gear(
    name = "llm-gateway",
    deps = [types_registry, authz_resolver],
    capabilities = [rest]
)]
#[derive(Default)]
pub struct LlmGatewayModule {
    service: OnceLock<Arc<Service>>,
}

#[async_trait]
impl Gear for LlmGatewayModule {
    #[tracing::instrument(skip_all, fields(module = "llm-gateway"))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: LlmGatewayConfig = ctx.config_or_default()?;
        let hub = ctx.client_hub();

        // PEP boundary: fail init without an authz-resolver client rather
        // than ever serving without authorization.
        let authz = hub
            .get::<dyn AuthZResolverApi>()
            .with_context(|| format!("{} requires an authz-resolver client", Self::MODULE_NAME))?;
        let enforcer = PolicyEnforcer::new(authz);

        let registry = hub
            .get::<dyn ModelRegistryClientV1>()
            .with_context(|| format!("{} requires a model-registry client", Self::MODULE_NAME))?;

        let service = Arc::new(Service::new(
            enforcer,
            Arc::new(ModelRegistryResolver::new(registry)),
            Arc::new(GtsPluginDirectory::new(hub.clone())),
            Arc::new(KeylessCredentialBroker),
            Arc::new(TypesRegistryToolSchemas::new(hub.clone())),
            Arc::new(ProviderRulesRegistry::permissive()),
            &cfg.limits,
        ));

        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn LlmGatewayClientV1> = Arc::new(LlmGatewayLocalClient::new(service));
        hub.register::<dyn LlmGatewayClientV1>(client);
        Ok(())
    }
}

impl RestApiCapability for LlmGatewayModule {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} Service not initialized", Self::MODULE_NAME))?;
        Ok(routes::register_routes(router, openapi, service))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_tests.rs"]
mod module_tests;
