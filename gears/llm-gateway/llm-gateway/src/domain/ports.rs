//! Ports the request pipeline depends on. Adapters live in `infra`.

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway_sdk::{
    LlmGatewayError, LlmGatewayProviderPluginClientV1, ProviderPluginCapabilities,
};
use model_registry_sdk::ModelInfoV1;
use secrecy::SecretString;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// Model Resolver: turns a model identifier into model info for the caller's
/// tenant.
#[async_trait]
pub trait ModelResolver: Send + Sync {
    /// Resolve `model` for the tenant of `ctx`.
    ///
    /// # Errors
    ///
    /// `model_not_found` when the model is unknown or not available for the
    /// tenant; `model_resolution_unavailable` when Model Registry fails.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        model: &str,
    ) -> Result<ModelInfoV1, LlmGatewayError>;
}

/// A plugin selected for a provider type: its client plus the facts of its
/// GTS plugin instance the pipeline needs.
#[domain_model]
#[derive(Clone)]
pub struct ResolvedPlugin {
    /// The plugin client.
    pub client: Arc<dyn LlmGatewayProviderPluginClientV1>,
    /// `tool_namespace` of the plugin instance: the prefix of the provider
    /// tools it accepts (`openai` for `openai:shell`).
    pub tool_namespace: String,
}

impl ResolvedPlugin {
    /// Bundle a plugin client with its instance `tool_namespace`.
    #[must_use]
    pub fn new(
        client: Arc<dyn LlmGatewayProviderPluginClientV1>,
        tool_namespace: impl Into<String>,
    ) -> Self {
        Self {
            client,
            tool_namespace: tool_namespace.into(),
        }
    }

    /// Integration capabilities reported by the plugin (local, no external
    /// call).
    #[must_use]
    pub fn capabilities(&self) -> ProviderPluginCapabilities {
        self.client.capabilities()
    }
}

/// Plugin Directory: finds the provider plugin serving a provider type.
#[async_trait]
pub trait PluginDirectory: Send + Sync {
    /// Resolve the plugin whose `provider_type` equals the model `gts_type`.
    /// Ties are broken by plugin priority (lowest value wins).
    ///
    /// # Errors
    ///
    /// `provider_not_supported` when no registered plugin matches.
    async fn resolve(&self, provider_type: &str) -> Result<ResolvedPlugin, LlmGatewayError>;
}

/// Source of tool schemas for the Tool Resolver (Type Registry in production).
#[async_trait]
pub trait ToolSchemaSource: Send + Sync {
    /// Fetch the JSON schema registered under the GTS id `schema_id` (without
    /// the `gts://` prefix), under the caller's security context.
    ///
    /// # Errors
    ///
    /// `schema_not_found` (carrying `schema_id`) when the schema is unknown,
    /// not visible to the caller, or Type Registry cannot answer.
    async fn schema(
        &self,
        ctx: &SecurityContext,
        schema_id: &str,
    ) -> Result<serde_json::Value, LlmGatewayError>;
}

/// Credential Broker: provides the provider API key for a model.
#[async_trait]
pub trait CredentialBroker: Send + Sync {
    /// Resolve the provider API key for `model` under the caller's context.
    /// `Ok(None)` means the model needs no key from the gateway.
    ///
    /// # Errors
    ///
    /// `credentials_unavailable` when the key cannot be fetched.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        model: &ModelInfoV1,
    ) -> Result<Option<SecretString>, LlmGatewayError>;
}
