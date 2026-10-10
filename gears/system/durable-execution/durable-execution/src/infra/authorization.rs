//! Short-lived service authorization for every control-plane operation.
use super::storage::StoreError;
use crate::config::Config;
use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsRequest};
use authz_resolver_sdk::{PolicyEnforcer, pep::ResourceType};
use std::sync::Arc;
use toolkit_security::{AccessScope, pep_properties};

#[async_trait]
pub trait WorkerAuthorization: Send + Sync {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError>;
}

pub struct ServiceAuthorization {
    pub authn: Arc<dyn AuthNResolverClient>,
    pub enforcer: PolicyEnforcer,
    pub config: Config,
}
const RESOURCE: ResourceType = ResourceType::from_static(
    "durable_execution.run",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::OWNER_ID,
        pep_properties::RESOURCE_ID,
    ],
);
const DEFINITION_RESOURCE: ResourceType = ResourceType::from_static(
    "durable_execution.definition",
    &[pep_properties::RESOURCE_ID],
);
#[async_trait]
impl WorkerAuthorization for ServiceAuthorization {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        let secret = std::env::var(&self.config.service_client_secret_env).map_err(|error| {
            let error_category = match error {
                std::env::VarError::NotPresent => "secret_environment_missing",
                std::env::VarError::NotUnicode(_) => "secret_environment_not_unicode",
            };
            tracing::warn!(
                action,
                secret_env = %self.config.service_client_secret_env,
                error_category,
                "durable service credentials unavailable"
            );
            StoreError::AuthorizationUnavailable
        })?;
        self.authorize(
            ClientCredentialsRequest {
                client_id: self.config.service_client_id.clone(),
                client_secret: secrecy::SecretString::from(secret),
                scopes: self.config.service_scopes.clone(),
            },
            action,
        )
        .await
    }
}
impl ServiceAuthorization {
    async fn authorize(
        &self,
        request: ClientCredentialsRequest,
        action: &str,
    ) -> Result<AccessScope, StoreError> {
        let identity = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.authn.exchange_client_credentials(&request),
        )
        .await
        .map_err(|_| StoreError::AuthorizationUnavailable)?
        .map_err(|_| StoreError::AuthorizationUnavailable)?;
        let ctx = identity.security_context;
        if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
            return Err(StoreError::AuthorizationUnavailable);
        }
        let (resource, action) = action
            .strip_prefix("definition:")
            .map_or((RESOURCE, action), |action| (DEFINITION_RESOURCE, action));
        let request = authz_resolver_sdk::pep::AccessRequest::new().require_constraints(
            resource.name() != "durable_execution.definition" && action != "cancel_definition",
        );
        self.enforcer
            .access_scope_with(&ctx, &resource, action, None, &request)
            .await
            .map_err(|error| {
                warn_policy_failure(&error, action, resource.name(), &ctx);
                StoreError::Authorization(Box::new(error))
            })
    }
}

/// Classify failures without logging PDP-provided text or authorization data.
pub(super) fn warn_policy_failure(
    error: &authz_resolver_sdk::pep::EnforcerError,
    action: &str,
    resource: &str,
    ctx: &toolkit_security::SecurityContext,
) {
    let kind = match error {
        authz_resolver_sdk::pep::EnforcerError::Denied { .. } => "denied",
        authz_resolver_sdk::pep::EnforcerError::EvaluationFailed(_) => "evaluation_failed",
        authz_resolver_sdk::pep::EnforcerError::CompileFailed(_) => "constraint_compilation_failed",
    };
    tracing::warn!(action, resource, kind, subject_id=%ctx.subject_id(), tenant_id=%ctx.subject_tenant_id(), "durable authorization rejected");
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "authorization_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/authorization_diagnostics_tests.rs"]
mod diagnostics_tests;
