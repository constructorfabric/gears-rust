//! Authorization: the shared PEP-backed seam every surface goes through.
//!
//! One decision per request per `(ResourceType, action)`, resolved here and
//! reused across that request's stages; there is no cross-request decision
//! cache in v1 (DESIGN § Authorization Model). REST and `ClientHub` reach this
//! same seam, which is what the authorization-parity tests assert.

use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

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
///
/// A read keeps the scope the PDP returned, subtree and all: a parent tenant
/// seeing its children is the platform's visibility rule. Every other action
/// is pinned to the caller's own tenant. The store looks rows up *by key
/// under the scope* on the write path -- the upsert's existing row, an edge's
/// endpoints, a replacement's stale set, a delete's target -- and under a
/// `Subtree` scope a parent's write of key K found the child's K and
/// rewrote it, and a parent's replacement of `repository = R` hard-deleted
/// the child's nodes of R. Writes land in the caller's tenant (the row's
/// `tenant_id` is always the subject's), so that is also the only tenant a
/// write may touch.
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
    let scope = enforcer
        .access_scope_with(ctx, resource, action, None, &request)
        .await
        .map_err(|error| map_enforcer_err(&error))?;
    Ok(if action == actions::READ {
        scope
    } else {
        pinned_to(&scope, tenant)
    })
}

/// Narrow `scope` to rows of `tenant` alone.
///
/// Intersection, never widening: each OR-ed constraint gains
/// `owner_tenant_id = tenant` beside the terms it already had, so a
/// constraint that excluded `tenant` still excludes it and one that admitted
/// a subtree now admits its root. Deny-all stays deny-all; allow-all becomes
/// the tenant.
#[must_use]
pub fn pinned_to(scope: &AccessScope, tenant: Uuid) -> AccessScope {
    if scope.is_deny_all() {
        return AccessScope::deny_all();
    }
    if scope.is_unconstrained() {
        return AccessScope::for_tenant(tenant);
    }
    AccessScope::from_constraints(
        scope
            .constraints()
            .iter()
            .map(|constraint| {
                let mut filters = constraint.filters().to_vec();
                filters.push(ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant));
                ScopeConstraint::new(filters)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinning intersects: it can take a subtree down to its root and can
    /// never admit a tenant the PDP left out.
    #[test]
    fn a_pinned_scope_admits_only_the_callers_tenant() {
        let (parent, child) = (Uuid::now_v7(), Uuid::now_v7());

        let subtree = pinned_to(&AccessScope::for_tenants(vec![parent, child]), parent);
        assert!(
            subtree
                .constraints()
                .iter()
                .flat_map(|c| c.filters().iter())
                .any(|f| *f == ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, parent)),
            "a subtree scope gains the caller's tenant as an equality: {subtree:?}"
        );
        assert_eq!(subtree.constraints().len(), 1, "no constraint is added");

        assert_eq!(
            pinned_to(&AccessScope::allow_all(), parent),
            AccessScope::for_tenant(parent)
        );
        assert!(pinned_to(&AccessScope::deny_all(), parent).is_deny_all());
    }

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
