//! Glue wiring the echo Service to `IdpPluginClient` and publishing the GTS instance — see crate root (lib.rs) for behaviour and prod-safety warnings.

use std::sync::{Arc, OnceLock};

use account_management_sdk::{IdpPluginClient, IdpPluginSpecV1};
use anyhow::Context as _;
use async_trait::async_trait;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::{info, warn};
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::StaticIdpPluginConfig;
use crate::domain::Service;

/// Static `IdP` plugin for bootstrap and tenant lifecycle without an external `IdP`.
///
/// `init` publishes `PluginV1<IdpPluginSpecV1>` with the configured vendor/priority,
/// awaits admission, then binds [`Service`] as a scoped `IdpPluginClient`.
/// AM selects by `cfg.idp.vendor` (default `"cf"`) and priority.
///
/// The host must link a database-backed types-registry. Its system-gear init
/// runs first without a `deps` edge; a missing registry fails boot.
/// Remote publication is deferred to T41/T42.
#[toolkit::gear(name = "static-idp-plugin")]
pub struct StaticIdpPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticIdpPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for StaticIdpPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        warn!(
            "Static IdP plugin is running in echo mode - every provision/deprovision \
             succeeds without contacting a real IdP. Do NOT use this plugin in production."
        );

        let cfg: StaticIdpPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        let service = Arc::new(Service::new());

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) = PluginV1::<IdpPluginSpecV1>::build_registration(
            "cf.builtin.static_idp.plugin.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        // Publish to types-registry; the backend is registered only once the instance is
        // admitted (or found up to date).
        let registry = ctx
            .client_hub()
            .get::<dyn PlatformTypesRegistryApi>()
            .with_context(|| {
                format!(
                    "{}: no PlatformTypesRegistryApi to publish to; a database-backed \
                     types-registry must run in this process",
                    Self::MODULE_NAME
                )
            })?;
        let publisher = PublisherContext {
            name: Self::MODULE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").parse()?,
        };
        let gts_id = GtsId::try_new(instance_id.as_ref())?;
        publish_startup_entities(
            registry.as_ref(),
            &publisher,
            &[(gts_id, instance_json)],
            ctx.cancellation_token(),
        )
        .await?;

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // AM's lazy IdP resolver (`account_management::infra::idp::
        // LazyIdpProvider`) reads this scoped registration on first
        // API call via `ClientHub::try_get_scoped` keyed on the
        // catalogue instance id. The scope MUST equal `instance_id`
        // (the same value `PluginV1::build_registration` derived
        // above) so the lazy `choose_plugin_instance` → `get_scoped`
        // chain finds this trait object.
        let api: Arc<dyn IdpPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id);
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
