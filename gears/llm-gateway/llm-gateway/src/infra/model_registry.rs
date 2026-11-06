//! Model Resolver adapter over `ModelRegistryClientV1::get_tenant_model`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;
use model_registry_sdk::{ModelInfoV1, ModelRegistryClientV1, ModelRegistryError};
use toolkit_security::SecurityContext;

use crate::config::DependencyTimeouts;
use crate::domain::ports::ModelResolver;

/// Resolves models through Model Registry, mapping its errors to gateway
/// errors (unknown, unapproved, and deprecated models are indistinguishable).
pub struct ModelRegistryResolver {
    client: Arc<dyn ModelRegistryClientV1>,
    /// `timeouts.dependency.model_registry`: bound of one lookup. On expiry
    /// the lookup is `model_resolution_unavailable` (fail closed).
    timeout: Duration,
}

impl ModelRegistryResolver {
    /// Build the resolver over a Model Registry client.
    #[must_use]
    pub fn new(client: Arc<dyn ModelRegistryClientV1>) -> Self {
        Self {
            client,
            timeout: DependencyTimeouts::default().model_registry,
        }
    }

    /// Bound one lookup by `timeout` (`timeouts.dependency.model_registry`).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl ModelResolver for ModelRegistryResolver {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        model: &str,
    ) -> Result<ModelInfoV1, LlmGatewayError> {
        let lookup = tokio::time::timeout(self.timeout, self.client.get_tenant_model(ctx, model));
        let Ok(lookup) = lookup.await else {
            tracing::warn!(
                model,
                timeout_ms = self.timeout.as_millis(),
                "model registry lookup timed out"
            );
            return Err(LlmGatewayError::ModelResolutionUnavailable);
        };
        match lookup {
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
                // Identifiers and the full source chain; the chain stays off the
                // wire. The service logs the outcome code separately, without
                // the cause, so this is the only record of why.
                tracing::warn!(model, error = %error_chain(&err), "model registry lookup failed");
                Err(LlmGatewayError::ModelResolutionUnavailable)
            }
        }
    }
}

/// `err` and its `source` chain, `: `-separated.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "model_registry_tests.rs"]
mod model_registry_tests;
