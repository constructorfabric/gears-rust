//! Ports the request pipeline depends on. Adapters live in `infra`.

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway_sdk::{LlmGatewayError, LlmGatewayProviderPluginClientV1};
use model_registry_sdk::ModelInfoV1;
use secrecy::SecretString;
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

/// Plugin Directory: finds the provider plugin serving a provider type.
#[async_trait]
pub trait PluginDirectory: Send + Sync {
    /// Resolve the plugin whose `provider_type` equals the model `gts_type`.
    /// Ties are broken by plugin priority (lowest value wins).
    ///
    /// # Errors
    ///
    /// `provider_not_supported` when no registered plugin matches.
    async fn resolve(
        &self,
        provider_type: &str,
    ) -> Result<Arc<dyn LlmGatewayProviderPluginClientV1>, LlmGatewayError>;
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
