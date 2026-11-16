//! Authorization: the shared PEP-backed seam every surface goes through.
//!
//! One decision per request per `(ResourceType, action)`, resolved here and
//! reused across that request's stages; there is no cross-request decision
//! cache in v1 (DESIGN § Authorization Model). REST and `ClientHub` reach this
//! same seam, which is what the authorization-parity tests assert.

use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
use toolkit_macros::domain_model;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, SecurityContext, pep_properties,
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
/// outage, never a grant; an unknown outcome class is an internal failure.
#[must_use]
pub fn map_enforcer_err(error: &EnforcerError) -> DomainError {
    match error {
        EnforcerError::AccessDenied(_) | EnforcerError::CompileFailed(_) => {
            DomainError::AccessDenied
        }
        EnforcerError::EvaluationFailed(_) => DomainError::Unavailable {
            detail: "authorization evaluation failed".to_owned(),
        },
        unknown => {
            tracing::error!(error = %unknown, "authz failed with an unknown outcome");
            DomainError::internal("authorization failed with an unknown outcome")
        }
    }
}

/// Who makes a call and, when it names one, the tenant the call is about.
///
/// A caller acting for a tenant it is not a subject of -- a service identity
/// with one platform-wide token, as Construct is (gears-rust #5623) -- names
/// the target tenant, and the PDP decides whether it may act there. Without
/// a target a call is about the caller's own tenant, exactly as before.
/// `From<&SecurityContext>` keeps every caller that passes a context alone
/// unchanged.
#[domain_model]
#[derive(Clone, Copy)]
pub struct Call<'a> {
    pub ctx: &'a SecurityContext,
    pub tenant: Option<Uuid>,
}

impl<'a> Call<'a> {
    /// A call by `ctx`'s subject about `tenant`.
    #[must_use]
    pub fn in_tenant(ctx: &'a SecurityContext, tenant: Uuid) -> Self {
        Self {
            ctx,
            tenant: Some(tenant),
        }
    }
}

impl<'a> From<&'a SecurityContext> for Call<'a> {
    fn from(ctx: &'a SecurityContext) -> Self {
        Self { ctx, tenant: None }
    }
}

/// Resolve the caller's `AccessScope` for `action` on `resource`.
///
/// A read keeps the scope the PDP returned, subtree and all: a parent tenant
/// seeing its children is the platform's visibility rule. Every other action
/// is pinned to the caller's own tenant (or to the target, below). The store looks rows up *by key
/// under the scope* on the write path -- the upsert's existing row, an edge's
/// endpoints, a replacement's stale set, a delete's target -- and under a
/// `Subtree` scope a parent's write of key K found the child's K and
/// rewrote it, and a parent's replacement of `repository = R` hard-deleted
/// the child's nodes of R. Writes land in the caller's tenant (the row's
/// `tenant_id` is always the subject's), so that is also the only tenant a
/// write may touch.
///
/// With a `target`, the call is about that tenant alone, for every action,
/// reads included: the PDP is asked about the target as the context tenant
/// in `RootOnly` mode, a scope that does not name the target is a refusal,
/// and the scope that remains is pinned to the target, so no read mixes two
/// tenants. A refusal is `AccessDenied`, which every surface answers as not
/// found: a caller learns nothing about a tenant it cannot reach.
pub async fn scope_for(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    target: Option<Uuid>,
    resource: &ResourceType,
    action: &str,
) -> Result<AccessScope, DomainError> {
    if let Some(target) = target {
        let request = AccessRequest::new()
            .context_tenant_id(target)
            .tenant_mode(TenantMode::RootOnly)
            .resource_property(pep_properties::OWNER_TENANT_ID, target)
            .require_constraints(true);
        // Every refusal is logged with ids only: one service identity can act
        // in many tenants, and a caller trying one tenant after another is
        // visible nowhere else -- the answer is a plain 404, and the PEP does
        // not log a denial.
        let refused = |why: &str| {
            tracing::info!(
                subject_id = %ctx.subject_id(),
                subject_tenant = %ctx.subject_tenant_id(),
                target_tenant = %target,
                resource = resource.name(),
                action,
                why,
                "graph-storage: a call about a target tenant was refused"
            );
        };
        let scope = match enforcer
            .access_scope_with(ctx, resource, action, None, &request)
            .await
        {
            Ok(scope) => scope,
            Err(error) => {
                let mapped = map_enforcer_err(&error);
                if matches!(mapped, DomainError::AccessDenied) {
                    refused("the policy denied it");
                }
                return Err(mapped);
            }
        };
        if !reaches(&scope, target) {
            refused("the granted scope does not name the tenant");
            return Err(DomainError::AccessDenied);
        }
        tracing::debug!(
            subject_id = %ctx.subject_id(),
            target_tenant = %target,
            resource = resource.name(),
            action,
            "graph-storage: a call about a target tenant"
        );
        return Ok(pinned_to(&scope, target));
    }
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

/// Whether the PDP's `scope` grants anything in `tenant`.
///
/// An unconstrained scope does. Otherwise a constraint reaches the tenant
/// only when it names it, by equality or in a set, on `owner_tenant_id`: a
/// constraint that names another tenant, a tenant subtree this build cannot
/// decide in memory, or no tenant at all grants nothing here. Deciding it
/// rather than pinning blindly is what tells a refusal (404) from an empty
/// tenant (an empty answer).
#[must_use]
pub fn reaches(scope: &AccessScope, tenant: Uuid) -> bool {
    if scope.is_deny_all() {
        return false;
    }
    if scope.is_unconstrained() {
        return true;
    }
    let wanted = ScopeValue::Uuid(tenant);
    scope.constraints().iter().any(|constraint| {
        constraint.filters().iter().any(|filter| {
            filter.property() == pep_properties::OWNER_TENANT_ID
                && filter.is_representable_in_memory()
                && filter
                    .values()
                    .iter()
                    .any(|value| *value == wanted || value.as_uuid() == Some(tenant))
        })
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
    use authz_resolver_sdk::pep::AccessDenial;

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

    /// A scope reaches a tenant only when it names it on `owner_tenant_id`:
    /// a grant elsewhere, a deny-all and a constraint that names no tenant
    /// are all refusals, and an unconstrained scope is a grant.
    #[test]
    fn a_scope_reaches_only_a_tenant_it_names() {
        let (target, other) = (Uuid::now_v7(), Uuid::now_v7());
        assert!(reaches(&AccessScope::for_tenant(target), target));
        assert!(reaches(
            &AccessScope::for_tenants(vec![other, target]),
            target
        ));
        assert!(reaches(&AccessScope::allow_all(), target));
        assert!(!reaches(&AccessScope::for_tenant(other), target));
        assert!(!reaches(&AccessScope::deny_all(), target));
        let no_tenant =
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::eq(
                "owner_id", target,
            )])]);
        assert!(
            !reaches(&no_tenant, target),
            "a non-tenant property names no tenant"
        );
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
        let denied = map_enforcer_err(&EnforcerError::AccessDenied(AccessDenial::Pdp {
            reason: None,
        }));
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
