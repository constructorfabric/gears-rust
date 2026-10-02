use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mini_chat_sdk::{MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use super::config::StaticMiniChatPolicyPluginConfig;
use super::service::Service;

/// Static model-policy plugin gear for mini-chat.
///
/// Provides a config-driven model catalog for development and testing.
#[toolkit::gear(
    name = "static-mini-chat-model-policy-plugin",
    deps = [types_registry]
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
impl Gear for StaticMiniChatModelPolicyPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: StaticMiniChatPolicyPluginConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("static model policy config: {e}"))?;
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
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<MiniChatModelPolicyPluginSpecV1>::build_registration(
                "cf.core._.static_mini_chat_model_policy.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
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
