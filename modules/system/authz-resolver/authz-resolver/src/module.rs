//! `AuthZ` resolver module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverPluginSpecV1};
use modkit::Module;
use modkit::context::ModuleCtx;
use modkit::contracts::SystemCapability;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::AuthZResolverConfig;
use crate::domain::{AuthZResolverLocalClient, Service};

/// `AuthZ` Resolver module.
///
/// This module:
/// 1. Registers the plugin schema in types-registry
/// 2. Discovers plugin instances via types-registry
/// 3. Routes requests to the selected plugin based on vendor configuration
///
/// Plugin discovery is lazy: happens on first API call after types-registry
/// is ready.
#[modkit::module(
    name = "authz-resolver",
    deps = ["types-registry"],
    capabilities = [system]
)]
pub(crate) struct AuthZResolver {
    service: OnceLock<Arc<Service>>,
}

impl Default for AuthZResolver {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

// Marked as `system` so that init() runs in the system-module phase.
// This ensures the AuthZResolver client is available in ClientHub before
// other system modules that depend on it.
impl SystemCapability for AuthZResolver {}

#[async_trait]
impl Module for AuthZResolver {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: AuthZResolverConfig = ctx.config()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // Register plugin schema in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let schema_str = AuthZResolverPluginSpecV1::gts_schema_with_refs_as_string();
        let mut schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
        // Workaround for a bug in gts-macros: derived (child) schemas generated via
        // gts_schema_with_refs_allof() omit "additionalProperties": false at the top level,
        // even when the base schema declares it. The types-registry rejects this as loosening
        // the base constraint. Patch it here until gts-macros is fixed upstream.
        if let Some(obj) = schema_json.as_object_mut() {
            obj.insert(
                "additionalProperties".to_owned(),
                serde_json::Value::Bool(false),
            );
        }
        let results = registry.register(vec![schema_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;
        info!(
            schema_id = %AuthZResolverPluginSpecV1::gts_schema_id(),
            "Registered plugin schema in types-registry"
        );

        // Create service
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(hub, cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register client in ClientHub
        let api: Arc<dyn AuthZResolverClient> = Arc::new(AuthZResolverLocalClient::new(svc));
        ctx.client_hub().register::<dyn AuthZResolverClient>(api);

        Ok(())
    }
}
