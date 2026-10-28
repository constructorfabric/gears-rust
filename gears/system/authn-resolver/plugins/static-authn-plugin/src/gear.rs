//! Static `AuthN` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::context::GearCtx;
use toolkit::contracts::SystemCapability;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::StaticAuthNPluginConfig;
use crate::domain::Service;

/// Static token-to-identity `AuthN` plugin.
///
/// The schema comes from `toolkit-gts` inventory. After proxy wiring, `post_init`
/// publishes the instance through consumed `PlatformTypesRegistryApi`, awaits
/// admission, then binds its scoped client. Failure leaves no client and fails boot.
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::gear(name = "static-authn-plugin", capabilities = [system])]
pub struct StaticAuthNPlugin {
    pending: OnceLock<Publication>,
}

/// What `init` prepares and `post_init` publishes.
struct Publication {
    hub: Arc<ClientHub>,
    cancel: CancellationToken,
    instance_id: GtsId,
    instance: serde_json::Value,
    service: Arc<Service>,
}

impl Default for StaticAuthNPlugin {
    fn default() -> Self {
        Self {
            pending: OnceLock::new(),
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
        let (instance_id, instance) = PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
            "cf.builtin.static_authn_resolver.plugin.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        let publication = Publication {
            hub: ctx.client_hub(),
            cancel: ctx.cancellation_token().clone(),
            instance_id: GtsId::try_new(instance_id.as_ref())?,
            instance,
            service: Arc::new(Service::from_config(&cfg)),
        };
        self.pending
            .set(publication)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        Ok(())
    }
}

#[async_trait]
impl SystemCapability for StaticAuthNPlugin {
    /// Publish the instance, then bind the client: after proxy wiring, so the registry's
    /// platform API is in the hub wherever the registry runs.
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let p = self
            .pending
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))?;

        let registry = p.hub.get::<dyn PlatformTypesRegistryApi>().map_err(|e| {
            anyhow::anyhow!("static-authn-plugin: no PlatformTypesRegistryApi to publish to: {e}")
        })?;
        publish_startup_entities(
            registry.as_ref(),
            &PublisherContext {
                name: Self::MODULE_NAME.to_owned(),
                version: env!("CARGO_PKG_VERSION").parse()?,
            },
            &[(p.instance_id.clone(), p.instance.clone())],
            &p.cancel,
        )
        .await?;

        // Register scoped client in ClientHub, only past admission.
        let api: Arc<dyn AuthNResolverPluginClient> = Arc::clone(&p.service) as _;
        p.hub.register_scoped::<dyn AuthNResolverPluginClient>(
            ClientScope::gts_id(p.instance_id.as_ref()),
            api,
        );

        info!(instance_id = %p.instance_id, "static AuthN plugin published and bound");
        Ok(())
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
