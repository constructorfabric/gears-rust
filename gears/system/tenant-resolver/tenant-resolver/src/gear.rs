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
/// The registry is consumed, not linked: `PlatformTypesRegistryApi` is the registry's own
/// local client where the registry runs in this process, and its resolving REST client
/// otherwise. The `TenantResolverPluginSpecV1` schema reaches the registry through the
/// `toolkit-gts` inventory it seeds from. Plugin discovery is lazy: it happens on the
/// first API call, never in `init`.
#[toolkit::gear(
    name = "tenant-resolver",
    gts(crates = [tenant_resolver_sdk]),
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

/// The GTS publication `gts(...)` declares: exactly the SDK's declarations, under
/// this gear's name and version, independent of configuration.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used)]
mod gts_publication_tests {
    use toolkit::config::ConfigProvider;
    use toolkit::gts_publication::GtsPublicationCapability as _;

    use super::*;

    struct NoConfig;

    impl ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
            None
        }
    }

    #[test]
    fn the_manifest_is_the_sdk_declarations() {
        let manifest = TenantResolver::default()
            .descriptor()
            .build_gts_manifest(&NoConfig)
            .expect("manifest");

        assert_eq!(manifest.publisher_name(), "tenant-resolver");
        assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
        let ids: Vec<&str> = manifest
            .documents()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let declared: Vec<String> = tenant_resolver_sdk::gts_declarations()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, declared);
        assert!(
            ids.contains(&tenant_resolver_sdk::TenantResolverPluginSpecV1::gts_type_id().as_ref()),
            "{ids:?}"
        );
    }
}
