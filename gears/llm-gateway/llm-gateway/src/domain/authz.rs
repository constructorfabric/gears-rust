//! PEP gate for the LLM Gateway resource.
//!
//! One LLM Gateway resource type scoped by `OWNER_TENANT_ID` (the caller's
//! tenant). The GTS identifiers are a design assumption (DESIGN §4.10).

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use llm_gateway_sdk::LlmGatewayError;
use toolkit_gts::gts_id;
use toolkit_security::{SecurityContext, pep_properties};

/// GTS id of the LLM Gateway PEP resource type.
pub const LLM_GATEWAY_RESOURCE_TYPE: &str = gts_id!("cf.llmgw.gateway.request.v1~");

/// PEP resource type for LLM Gateway operations.
pub const RESOURCE: ResourceType = ResourceType::from_static(
    LLM_GATEWAY_RESOURCE_TYPE,
    &[pep_properties::OWNER_TENANT_ID],
);

/// PEP action vocabulary. Renaming is a contract change against the PDP
/// policy bundle.
pub mod actions {
    /// Create a response (sync and streaming).
    pub const CREATE_RESPONSE: &str = "create_response";
    /// Create embeddings.
    pub const CREATE_EMBEDDING: &str = "create_embedding";
}

/// Run the PEP check for `action`, before any other dependency call.
///
/// The gate is subject- and tenant-level: the resource is scoped by the
/// caller's own tenant (`OWNER_TENANT_ID`) and no row constraints are
/// required, because the gateway reads no tenant rows.
///
/// # Errors
///
/// `permission_denied` on a PDP denial or an uncompilable decision. A PDP
/// evaluation failure (resolver unreachable) fails closed as
/// `gateway_unavailable` (HTTP 503, retryable): the platform PEP mapping
/// (usage-collector, account-management) treats it as service-unavailable
/// rather than a denial, and the gateway has no dedicated authorization
/// outage code. The request is never permitted.
pub async fn authorize(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<(), LlmGatewayError> {
    let request = AccessRequest::new()
        .require_constraints(false)
        .resource_property(
            pep_properties::OWNER_TENANT_ID,
            ctx.subject_tenant_id().to_string(),
        );
    match enforcer
        .access_scope_with(ctx, &RESOURCE, action, None, &request)
        .await
    {
        Ok(_scope) => Ok(()),
        Err(EnforcerError::EvaluationFailed(_)) => {
            tracing::warn!(action, "authorization check failed; failing closed");
            Err(LlmGatewayError::GatewayUnavailable)
        }
        Err(_) => Err(LlmGatewayError::PermissionDenied),
    }
}
