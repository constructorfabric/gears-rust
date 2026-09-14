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

#[cfg(test)]
mod tests {
    use super::*;

    /// The three PDP outcomes must not be swapped, and this is the whole of
    /// what separates them.
    ///
    /// A denial that reported as an outage would tell a caller to retry
    /// something they will never be allowed to do, and would page an operator
    /// for a working system. An outage that reported as a denial is worse:
    /// the authorization resolver being unreachable would read, to every
    /// caller and every dashboard, as "you may not", and a fail-closed
    /// decision would be indistinguishable from a policy one. Neither is
    /// visible in a passing integration test, because both answer *some*
    /// error.
    #[test]
    fn a_denial_and_an_outage_are_not_the_same_answer() {
        let denied = map_enforcer_err(&EnforcerError::Denied { deny_reason: None });
        assert!(
            matches!(denied, DomainError::AccessDenied),
            "a denial is a denial: {denied:?}"
        );

        // A scope the PDP returned but the gear cannot compile is also a
        // refusal, deliberately: serving it would mean serving a scope nobody
        // authorized. Fail closed, never wider.
        let uncompilable = map_enforcer_err(&EnforcerError::CompileFailed(
            authz_resolver_sdk::pep::ConstraintCompileError::AllConstraintsFailed {
                reason: "unrepresentable constraint".to_owned(),
            },
        ));
        assert!(
            matches!(uncompilable, DomainError::AccessDenied),
            "a scope that cannot be compiled fails closed: {uncompilable:?}"
        );

        // An evaluation failure is the resolver itself being unavailable. It
        // is *not* a grant and *not* a denial: it is an outage, and it has to
        // reach the caller as one so a retry is the right reaction.
        let outage = map_enforcer_err(&EnforcerError::EvaluationFailed(
            toolkit_canonical_errors::CanonicalError::internal("connection refused").create(),
        ));
        assert!(
            matches!(outage, DomainError::Unavailable { .. }),
            "an unreachable PDP is an outage, not a decision: {outage:?}"
        );
    }
}
