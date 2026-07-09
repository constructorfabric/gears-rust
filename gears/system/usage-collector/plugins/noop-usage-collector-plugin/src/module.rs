use std::sync::Arc;

use async_trait::async_trait;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::{info, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use usage_collector_sdk::{UsageCollectorPluginSpecV1, UsageCollectorPluginV1};

use crate::config::NoopUsageCollectorPluginConfig;
use crate::plugin::NoopBackend;

/// No-op Usage Collector storage backend plugin module.
///
/// Conforms to the storage Plugin SPI and performs the full GTS registration
/// handshake, but persists nothing. It exists so the plugin-host binding
/// resolves end-to-end in development and testing without a real DB backend.
#[toolkit::gear(
    name = "noop-usage-collector-plugin",
    deps = ["types-registry"]
)]
#[derive(Default)]
pub struct NoopUsageCollectorPlugin;

#[async_trait]
impl Gear for NoopUsageCollectorPlugin {
    // @cpt-flow:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration (vendor + priority; persists nothing else).
        let cfg: NoopUsageCollectorPluginConfig = ctx.config_expanded_or_default()?;

        warn!(
            target: "noop-usage-collector-plugin",
            "Loaded the no-op usage-collector backend - persists nothing; development/testing use only"
        );

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded no-op usage-collector plugin configuration"
        );

        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-clienthub-register
        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<UsageCollectorPluginSpecV1>::build_registration(
                "cf.core._.noop_usage_collector.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Register the scoped no-op backend client in ClientHub under the GTS
        // instance scope so the plugin host resolves it on first dispatch.
        ctx.client_hub()
            .register_scoped::<dyn UsageCollectorPluginV1>(
                ClientScope::gts_id(&instance_id),
                Arc::new(NoopBackend::new()) as Arc<dyn UsageCollectorPluginV1>,
            );
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-clienthub-register

        info!(
            instance_id = %instance_id,
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Registered noop usage-collector plugin instance"
        );
        Ok(())
    }
}
