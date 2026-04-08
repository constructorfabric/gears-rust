//! Tenant resolver module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use modkit::Module;
use modkit::context::ModuleCtx;
use modkit::contracts::SystemCapability;
use tenant_resolver_sdk::{TenantResolverClient, TenantResolverPluginSpecV1};
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::TenantResolverConfig;
use crate::domain::{Service, TenantResolverLocalClient};

/// Tenant Resolver module.
///
/// This module:
/// 1. Registers the plugin schema in types-registry
/// 2. Discovers plugin instances via types-registry
/// 3. Routes requests to the selected plugin based on vendor configuration
///
/// Plugin discovery is lazy: happens on first API call after types-registry
/// is ready.
#[modkit::module(
    name = "tenant-resolver",
    deps = ["types-registry"],
    capabilities = [system]
)]
pub(crate) struct TenantResolver {
    service: OnceLock<Arc<Service>>,
}

impl Default for TenantResolver {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

// Marked as `system` so that init() runs in the system-module phase.
// This ensures the TenantResolver client is available in ClientHub before
// other system modules that depend on it.
impl SystemCapability for TenantResolver {}

#[async_trait]
impl Module for TenantResolver {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: TenantResolverConfig = ctx.config()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // Register plugin schema in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let schema_str = TenantResolverPluginSpecV1::gts_schema_with_refs_as_string();
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
            schema_id = %TenantResolverPluginSpecV1::gts_schema_id(),
            "Registered plugin schema in types-registry"
        );

        // Create service
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(hub, cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register local client in ClientHub
        let api: Arc<dyn TenantResolverClient> = Arc::new(TenantResolverLocalClient::new(svc));
        ctx.client_hub().register::<dyn TenantResolverClient>(api);

        Ok(())
    }
}
