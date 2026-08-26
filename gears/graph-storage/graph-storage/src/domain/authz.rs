//! Authorization: the shared PEP-backed seam every surface goes through.
//!
//! One decision per request per `(ResourceType, action)`, resolved here and
//! reused across that request's stages; there is no cross-request decision
//! cache in v1 (DESIGN § Authorization Model). REST and `ClientHub` reach this
//! same seam, which is what the authorization-parity tests assert.

use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};

use crate::domain::error::DomainError;

pub mod actions {
    /// Ontology administration (type registration; index-affecting changes).
    pub const ADMIN: &str = "admin";
    /// Ingest, scope replacement, label attach/detach.
    pub const WRITE: &str = "write";
    /// Every read surface: node read, projection, search, traversal.
    pub const READ: &str = "read";
    /// Soft deletes.
    pub const DELETE: &str = "delete";
}

/// PDP resource type for graph nodes (edges are fenced by the same scope's
/// tenant arm; `StoreCtx` carries one compiled scope per call by contract).
#[must_use]
pub fn node_resource() -> ResourceType {
    ResourceType::new(
        graph_storage_sdk::gts::NODE_RESOURCE.to_owned(),
        &[pep_properties::OWNER_TENANT_ID],
    )
}

/// PDP resource type for the ontology surface.
#[must_use]
pub fn type_resource() -> ResourceType {
    ResourceType::new(
        graph_storage_sdk::gts::TYPE_RESOURCE.to_owned(),
        &[pep_properties::OWNER_TENANT_ID],
    )
}

/// Map a PEP enforcement failure to a domain error, fail-closed:
/// `Denied` / `CompileFailed` deny; `EvaluationFailed` is a dependency
/// outage, never a grant.
#[must_use]
pub fn map_enforcer_err(error: &EnforcerError) -> DomainError {
    match error {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => DomainError::AccessDenied,
        EnforcerError::EvaluationFailed(_) => DomainError::Unavailable {
            detail: "authorization evaluation failed".to_owned(),
        },
    }
}

/// Resolve the caller's `AccessScope` for `action` on `resource`.
pub async fn scope_for(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource: &ResourceType,
    action: &str,
) -> Result<AccessScope, DomainError> {
    let tenant = ctx.subject_tenant_id();
    let request = AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, tenant)
        .require_constraints(true);
    enforcer
        .access_scope_with(ctx, resource, action, None, &request)
        .await
        .map_err(|error| map_enforcer_err(&error))
}
