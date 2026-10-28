//! Static `AuthN` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::StaticAuthNPluginConfig;
use crate::domain::Service;

/// Static `AuthN` resolver plugin gear.
///
/// Provides token-to-identity mapping from configuration.
///
/// **Plugin registration pattern:**
/// - Gateway registers the plugin schema (GTS type definition)
/// - This plugin registers its instance (implementation metadata)
/// - This plugin registers its scoped client (implementation in `ClientHub`)
#[toolkit::gear(
    name = "static-authn-plugin",
    deps = [types_registry]
)]
pub struct StaticAuthNPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticAuthNPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for StaticAuthNPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration
        let cfg: StaticAuthNPluginConfig = ctx.config_expanded_or_default()?;
        if matches!(cfg.mode, crate::config::AuthNMode::AcceptAll) {
            tracing::warn!(
                "Static AuthN plugin is running in `accept_all` mode - \
                 all bearer tokens will be accepted with a hardcoded identity. \
                 Do NOT use this mode in production."
            );
        }

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            mode = ?cfg.mode,
            token_count = cfg.tokens.len(),
            "Loaded plugin configuration"
        );

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
                "cf.builtin.static_authn_resolver.plugin.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Create service from config
        let service = Arc::new(Service::from_config(&cfg));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn AuthNResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn AuthNResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id);
        Ok(())
    }
}
