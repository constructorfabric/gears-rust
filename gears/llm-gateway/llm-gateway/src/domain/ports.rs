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

/// A service `SecurityContext` together with the generation of the exchange
/// that produced it, so a refusal invalidates only that exchange.
#[domain_model]
#[derive(Debug, Clone)]
pub struct IdentityContext {
    /// The service context.
    pub ctx: SecurityContext,
    /// Generation of the exchange the context came from.
    pub generation: u64,
}

/// Gear service identity (client credentials): the context settle and usage
/// delivery run under, since they outlive the caller's request.
#[async_trait]
pub trait ServiceIdentity: Send + Sync {
    /// The service context, cached and refreshed by the adapter.
    ///
    /// # Errors
    ///
    /// When the credentials exchange fails (or is rate-limited); callers retry
    /// with backoff and never block consumer requests on it.
    async fn context(&self) -> anyhow::Result<IdentityContext>;

    /// A downstream call refused the context of `generation`
    /// (`Unauthenticated`): drop it so the next [`Self::context`] exchanges
    /// the credentials again. A no-op when a newer exchange already replaced
    /// that generation, and for identities that cache nothing.
    async fn invalidate(&self, _generation: u64) {}
}

/// What the Quota Guard hands to the Usage Reporter once a dispatched
/// request's lease is settled (or settle was abandoned, or no lease was held).
#[domain_model]
#[derive(Debug, Clone)]
pub struct UsageReport {
    /// Response (or embedding request) `id`: the usage idempotency key.
    pub response_id: String,
    /// Tenant of the request.
    pub tenant_id: uuid::Uuid,
    /// User (subject) of the request.
    pub user_id: uuid::Uuid,
    /// Model identifier as requested.
    pub model: String,
    /// Provider type (`gts_type` of the model).
    pub provider_type: String,
    /// Terminal usage: `cost` is the AI credit quantity, with the
    /// `complete` and `estimated` flags of the DESIGN usage table.
    pub usage: llm_gateway_sdk::Usage,
    /// The same quantity in metric units (micro-credits), rounded up.
    pub quantity_micro_credits: i64,
    /// Stamped once at the terminal state.
    pub terminal_at: std::time::SystemTime,
}

/// Usage Reporter entry point (implemented in slice 5b).
#[async_trait]
pub trait UsageSink: Send + Sync {
    /// Accept the usage of one terminal request. Must not block on delivery.
    async fn record(&self, report: UsageReport);
}
