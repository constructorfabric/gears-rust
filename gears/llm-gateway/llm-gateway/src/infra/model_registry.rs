//! Model Resolver adapter over `ModelRegistryClientV1::get_tenant_model`.

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;
use model_registry_sdk::{ModelInfoV1, ModelRegistryClientV1, ModelRegistryError};
use toolkit_security::SecurityContext;

use crate::domain::ports::ModelResolver;

/// Resolves models through Model Registry, mapping its errors to gateway
/// errors (unknown, unapproved, and deprecated models are indistinguishable).
pub struct ModelRegistryResolver {
    client: Arc<dyn ModelRegistryClientV1>,
}

impl ModelRegistryResolver {
    /// Build the resolver over a Model Registry client.
    #[must_use]
    pub fn new(client: Arc<dyn ModelRegistryClientV1>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ModelResolver for ModelRegistryResolver {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        model: &str,
    ) -> Result<ModelInfoV1, LlmGatewayError> {
        match self.client.get_tenant_model(ctx, model).await {
            Ok(found) => Ok(found.info),
            // Unknown, unapproved and deprecated look the same to the caller;
            // so do a disabled/missing provider and a registry-side
            // forbidden/invalid lookup: the model is not available to the tenant.
            Err(
                ModelRegistryError::ModelNotFound { .. }
                | ModelRegistryError::ModelNotApproved { .. }
                | ModelRegistryError::ModelDeprecated { .. }
                | ModelRegistryError::ProviderDisabled { .. }
                | ModelRegistryError::ProviderNotFound { .. }
                | ModelRegistryError::Forbidden { .. }
                | ModelRegistryError::Validation { .. },
            ) => Err(LlmGatewayError::model_not_found(model)),
            // Fail closed: the registry rejected the caller's identity, so the
            // gateway does not proceed (and does not report a registry outage).
            Err(ModelRegistryError::Unauthenticated { .. }) => {
                Err(LlmGatewayError::PermissionDenied)
            }
            // `Internal` and any other (or future) variant: registry unavailable.
            Err(err) => {
                // Identifiers only; the registry error chain stays off the wire.
                tracing::warn!(model, error = %err, "model registry lookup failed");
                Err(LlmGatewayError::ModelResolutionUnavailable)
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "model_registry_tests.rs"]
mod model_registry_tests;
