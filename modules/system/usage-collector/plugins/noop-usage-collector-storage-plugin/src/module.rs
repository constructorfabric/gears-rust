//! No-op usage-collector storage plugin module.
//!
//! Registers a GTS plugin instance in the types registry and exposes
//! [`usage_collector_sdk::UsageCollectorPluginClientV1`] under a scope keyed by that instance.

use std::sync::Arc;

use async_trait::async_trait;
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use usage_collector_sdk::{UsageCollectorPluginClientV1, UsageCollectorStoragePluginSpecV1};

use crate::config::NoopUsageCollectorStorageConfig;
use crate::domain::Service;

/// No-op usage-collector storage plugin for development and testing.
#[modkit::module(
    name = "noop-usage-collector-storage-plugin",
    deps = ["types-registry", "usage-collector"]
)]
#[derive(Default)]
struct NoopUsageCollectorStoragePlugin;

// @cpt-dod:cpt-cf-usage-collector-dod-sdk-and-ingest-core-noop-plugin:p1
#[async_trait]
impl Module for NoopUsageCollectorStoragePlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: NoopUsageCollectorStorageConfig = ctx.config_or_default()?;
        info!(
            %cfg.vendor,
            cfg.priority,
            "Loaded {} configuration",
            Self::MODULE_NAME,
        );

        let instance_id = UsageCollectorStoragePluginSpecV1::gts_make_instance_id(
            "cf.core._.noop_usage_collector_storage_plugin.v1",
        );

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<UsageCollectorStoragePluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: UsageCollectorStoragePluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        let service = Service::new();

        let api: Arc<dyn UsageCollectorPluginClientV1> = Arc::new(service);
        ctx.client_hub()
            .register_scoped::<dyn UsageCollectorPluginClientV1>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(
            %instance_id,
            "Registered {}",
            Self::MODULE_NAME,
        );

        Ok(())
    }
}
