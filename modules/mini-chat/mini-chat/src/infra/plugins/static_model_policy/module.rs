use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mini_chat_sdk::{MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginSpecV1};
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use super::config::StaticMiniChatPolicyPluginConfig;
use super::service::Service;

/// Static model-policy plugin module for mini-chat.
///
/// Provides a config-driven model catalog for development and testing.
#[modkit::module(
    name = "static-mini-chat-model-policy-plugin",
    deps = ["types-registry"]
)]
pub struct StaticMiniChatModelPolicyPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticMiniChatModelPolicyPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for StaticMiniChatModelPolicyPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: StaticMiniChatPolicyPluginConfig = ctx.config()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            models = cfg.model_catalog.len(),
            "Loaded static mini-chat model policy plugin configuration"
        );

        // Create service and lock initialization before any external side-effects
        // so that retries fail fast without duplicate registrations.
        let service = Arc::new(Service::new(
            cfg.model_catalog,
            cfg.kill_switches,
            cfg.default_standard_limits,
            cfg.default_premium_limits,
        ));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Generate plugin instance ID
        let instance_id = MiniChatModelPolicyPluginSpecV1::gts_make_instance_id(
            "x.core._.static_mini_chat_model_policy.v1",
        );

        // Register plugin instance in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<MiniChatModelPolicyPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: MiniChatModelPolicyPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Register scoped client in ClientHub
        let api: Arc<dyn MiniChatModelPolicyPluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn MiniChatModelPolicyPluginClientV1>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id, "Static mini-chat model policy plugin registered");
        Ok(())
    }
}
