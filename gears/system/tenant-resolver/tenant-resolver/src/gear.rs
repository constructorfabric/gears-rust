//! Tenant resolver gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::Gear;
use toolkit::context::GearCtx;
use tracing::info;

use crate::config::TenantResolverConfig;
use crate::domain::{Service, TenantResolverLocalClient};

/// Tenant Resolver gear.
///
/// This gear:
/// 1. Discovers plugin instances via types-registry
/// 2. Routes requests to the selected plugin based on vendor configuration
///
/// The `TenantResolverPluginSpecV1` schema itself reaches `types-registry`
/// automatically via the `toolkit-gts` link-time inventory — no per-init
/// registration is needed. Plugin discovery is lazy: happens on first API
/// call after types-registry is ready.
#[toolkit::gear(
    name = "tenant-resolver",
    deps = [types_registry],
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

#[async_trait]
impl Gear for TenantResolver {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: TenantResolverConfig = ctx.config_or_default()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // Create service
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(hub, cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register local client in ClientHub
        let api: Arc<dyn TenantResolverClient> = Arc::new(TenantResolverLocalClient::new(svc));
        ctx.client_hub().register::<dyn TenantResolverClient>(api);

        Ok(())
    }
}
