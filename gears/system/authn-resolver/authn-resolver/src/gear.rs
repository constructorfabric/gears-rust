//! `AuthN` resolver gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverBearerAuthenticator, AuthNResolverClient};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit_security::DynBearerAuthenticator;
use tracing::info;

use crate::config::AuthNResolverConfig;
use crate::domain::{AuthNResolverLocalClient, Service};

/// `AuthN` Resolver gear.
///
/// This gear:
/// 1. Discovers plugin instances via types-registry
/// 2. Routes requests to the selected plugin based on vendor configuration
///
/// The registry is consumed, not linked: `PlatformTypesRegistryApi` is the registry's own
/// local client where the registry runs in this process, and its resolving REST client
/// otherwise. This gear publishes the SDK's `AuthNResolverPluginSpecV1` schema
/// (`gts_publication(...)`), admitted before any application `init`. Plugin discovery is lazy:
/// it happens on the first API call, after every plugin bound its client in `init`.
#[toolkit::gear(
    name = "authn-resolver",
    gts_publication(crates = [authn_resolver_sdk]),
)]
pub(crate) struct AuthNResolver {
    service: OnceLock<Arc<Service>>,
}

impl Default for AuthNResolver {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for AuthNResolver {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: AuthNResolverConfig = ctx.config_or_default()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // Create service
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(Arc::clone(&hub), cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register client in ClientHub. Its plugin instances are admitted before
        // this `init` and bound before anything is served, so no gate is needed.
        let api: Arc<dyn AuthNResolverClient> = Arc::new(AuthNResolverLocalClient::new(svc));
        hub.register::<dyn AuthNResolverClient>(Arc::clone(&api));

        // Register the tenant-plane bearer bridge for the OoP runtime to pick up
        // (if-absent, so an explicitly-supplied authenticator wins).
        if hub.get::<DynBearerAuthenticator>().is_err() {
            let bridge = DynBearerAuthenticator::new(AuthNResolverBearerAuthenticator::new(api));
            hub.register::<DynBearerAuthenticator>(Arc::new(bridge));
        }

        Ok(())
    }
}

/// The GTS publication `gts_publication(...)` declares: exactly the SDK's declarations, under
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
        let manifest = AuthNResolver::default()
            .descriptor()
            .build_gts_manifest(&NoConfig)
            .expect("manifest");

        assert_eq!(manifest.publisher_name(), "authn-resolver");
        assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
        let ids: Vec<&str> = manifest
            .documents()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let declared: Vec<String> = authn_resolver_sdk::gts_declarations()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, declared);
        assert!(
            ids.contains(&authn_resolver_sdk::AuthNResolverPluginSpecV1::gts_type_id().as_ref()),
            "{ids:?}"
        );
    }
}
