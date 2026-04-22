use async_trait::async_trait;
use uuid::Uuid;

use crate::error::PluginError;
use crate::models::{Capability, CapabilityValue, HealthStatus, Message};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct SessionPluginCtx {
    pub session_type_id: Uuid,
    pub session_id: Option<Uuid>,
    pub call_ctx: PluginCallContext,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct MessagePluginCtx {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub messages: Vec<Message>,
    pub call_ctx: PluginCallContext,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct PluginCallContext {
    pub tenant_id: String,
    pub user_id: String,
    pub plugin_instance_id: String,
    pub session_type_id: Uuid,
    pub plugin_config: Option<serde_json::Value>,
    pub enabled_capabilities: Option<Vec<CapabilityValue>>,
}

#[async_trait]
pub trait ChatEngineBackendPlugin: Send + Sync {
    async fn on_session_type_configured(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    async fn on_session_created(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    async fn on_session_updated(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    async fn on_message(
        &self,
        _ctx: MessagePluginCtx,
    ) -> Result<Vec<String>, PluginError> {
        Ok(vec![])
    }

    async fn on_message_recreate(
        &self,
        _ctx: MessagePluginCtx,
    ) -> Result<Vec<String>, PluginError> {
        Ok(vec![])
    }

    async fn on_session_summary(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<String, PluginError> {
        Ok(String::new())
    }

    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    fn plugin_instance_id(&self) -> &str;
}
