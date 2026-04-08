use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use mini_chat_sdk::{MiniChatAuditPluginClientV1, MiniChatAuditPluginSpecV1};

use super::config::StaticMiniChatAuditPluginConfig;
use super::service::Service;

const VENDOR: &str = "cyber-fabric";
const PRIORITY: i16 = 100;

/// Static audit plugin module for mini-chat.
///
/// Logs all audit events via `tracing` for development and testing.
/// When `enabled: false` in config, the plugin registers normally but
/// all emit methods return immediately without logging.
#[modkit::module(
    name = "static-mini-chat-audit-plugin",
    deps = ["types-registry"]
)]
pub struct StaticMiniChatAuditPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticMiniChatAuditPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for StaticMiniChatAuditPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: StaticMiniChatAuditPluginConfig = ctx.config()?;
        info!(
            enabled = cfg.enabled,
            "Loaded static mini-chat audit plugin configuration"
        );

        let service = Arc::new(Service {
            enabled: cfg.enabled,
        });
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let instance_id =
            MiniChatAuditPluginSpecV1::gts_make_instance_id("x.core._.static_mini_chat_audit.v1");

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<MiniChatAuditPluginSpecV1> {
            id: instance_id.clone(),
            vendor: VENDOR.to_owned(),
            priority: PRIORITY,
            properties: MiniChatAuditPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        let api: Arc<dyn MiniChatAuditPluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn MiniChatAuditPluginClientV1>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id, "Static mini-chat audit plugin registered");
        Ok(())
    }
}
