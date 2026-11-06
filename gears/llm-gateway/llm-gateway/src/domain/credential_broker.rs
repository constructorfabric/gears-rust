//! Default Credential Broker.

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;
use model_registry_sdk::ModelInfoV1;
use secrecy::SecretString;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::ports::CredentialBroker;

/// The only place that reads the provider key secret reference from model
/// info.
///
/// Model Registry does not expose the reference yet, so every model is
/// treated as keyless and authentication falls to the OAGW upstream
/// (DESIGN §4.9).
#[must_use]
pub fn secret_ref(_model: &ModelInfoV1) -> Option<&str> {
    None
}

/// Broker that resolves no key for any model and calls no key store.
#[domain_model]
#[derive(Debug, Default)]
pub struct KeylessCredentialBroker;

#[async_trait]
impl CredentialBroker for KeylessCredentialBroker {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _model: &ModelInfoV1,
    ) -> Result<Option<SecretString>, LlmGatewayError> {
        Ok(None)
    }
}
