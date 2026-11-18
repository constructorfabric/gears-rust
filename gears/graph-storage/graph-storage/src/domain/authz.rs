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
    /// Purge: removing a node and its edges for good. Its own action, since
    /// erasure is stronger than a tombstone (gears-rust #5194).
    pub const PURGE: &str = "purge";
    /// Offboarding a tenant: removing its whole graph for good. Its own
    /// action on its own resource, implied by no other: nothing a producer
    /// holds should reach it.
    pub const OFFBOARD: &str = "offboard";
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

/// PDP resource type for a tenant's graph as a whole: what offboarding acts
/// on.
#[must_use]
pub fn tenant_resource() -> ResourceType {
    ResourceType::new(
        graph_storage_sdk::gts::TENANT_RESOURCE.to_owned(),
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

/// Whether `scope` admits rows of `tenant` alone: every constraint names
/// `tenant`, and only `tenant`, on `owner_tenant_id`. A deny-all admits
/// nothing, which is within it too. Anything this gear cannot decide in
/// memory is not.
#[must_use]
pub fn only(scope: &AccessScope, tenant: Uuid) -> bool {
    if scope.is_deny_all() {
        return true;
    }
    if scope.is_unconstrained() {
        return false;
    }
    let own = ScopeValue::Uuid(tenant);
    scope.constraints().iter().all(|constraint| {
        constraint.filters().iter().any(|filter| {
            filter.property() == pep_properties::OWNER_TENANT_ID
                && filter.is_representable_in_memory()
                && filter
                    .values()
                    .iter()
                    .all(|value| *value == own || value.as_uuid() == Some(tenant))
        })
    })
}

/// `scope` without the rows of `gone`, when this gear can say so.
///
/// Each constraint's tenant filter loses the tenants in `gone`; a constraint
/// left with no tenant admits nothing and is dropped. `None` when the scope
/// cannot be narrowed in memory -- unconstrained, a constraint without a
/// tenant filter, a tenant filter this build cannot enumerate, such as a
/// subtree resolved in the database, or a tenant value that is no UUID -- so
/// the caller fails closed rather than serve a scope that may still reach a
/// tenant in `gone`.
#[must_use]
pub fn without_tenants(scope: &AccessScope, gone: &[Uuid]) -> Option<AccessScope> {
    if scope.is_deny_all() {
        return Some(scope.clone());
    }
    if scope.is_unconstrained() {
        return None;
    }
    let mut kept = Vec::new();
    for constraint in scope.constraints() {
        let mut filters = Vec::new();
        let mut names_a_tenant = false;
        let mut admits_none = false;
        for filter in constraint.filters() {
            if filter.property() != pep_properties::OWNER_TENANT_ID {
                filters.push(filter.clone());
                continue;
            }
            if !filter.is_representable_in_memory() {
                return None;
            }
            names_a_tenant = true;
            // A value that is no tenant id cannot be told apart from a tenant
            // in `gone`, so the scope is refused, as `only` refuses it.
            let mut left: Vec<ScopeValue> = Vec::new();
            for value in filter.values() {
                let tenant = value.as_uuid()?;
                if !gone.contains(&tenant) {
                    left.push(value.clone());
                }
            }
            if left.is_empty() {
                admits_none = true;
                break;
            }
            filters.push(ScopeFilter::r#in(pep_properties::OWNER_TENANT_ID, left));
        }
        if !names_a_tenant {
            return None;
        }
        if !admits_none {
            kept.push(ScopeConstraint::new(filters));
        }
    }
    Some(if kept.is_empty() {
        AccessScope::deny_all()
    } else {
        AccessScope::from_constraints(kept)
    })
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

    /// A subtree loses the tenants being offboarded and keeps the rest; a
    /// scope that names no tenant, or names none it can list, cannot be
    /// narrowed and is refused rather than served.
    #[test]
    fn a_scope_loses_the_tenants_being_offboarded_or_is_refused() {
        let (parent, child, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let subtree = AccessScope::for_tenants(vec![parent, child]);
        let narrowed = without_tenants(&subtree, &[child]).expect("a listed subtree narrows");
        assert!(reaches(&narrowed, parent));
        assert!(!reaches(&narrowed, child));
        assert_eq!(
            without_tenants(&subtree, &[other]).expect("narrows"),
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::r#in(
                pep_properties::OWNER_TENANT_ID,
                vec![ScopeValue::Uuid(parent), ScopeValue::Uuid(child)],
            )])]),
        );
        assert!(
            without_tenants(&AccessScope::for_tenant(child), &[child])
                .expect("narrows")
                .is_deny_all(),
            "a scope of only the tenant being offboarded admits nothing"
        );
        assert!(without_tenants(&AccessScope::allow_all(), &[child]).is_none());
        let no_tenant =
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::eq(
                "owner_id", child,
            )])]);
        assert!(without_tenants(&no_tenant, &[child]).is_none());
        let not_a_tenant_id =
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::r#in(
                pep_properties::OWNER_TENANT_ID,
                vec![
                    ScopeValue::Uuid(parent),
                    ScopeValue::String("not-a-uuid".to_owned()),
                ],
            )])]);
        assert!(
            without_tenants(&not_a_tenant_id, &[child]).is_none(),
            "a value that may be the tenant being offboarded is refused, as `only` refuses it"
        );
        assert!(!only(&not_a_tenant_id, parent));
        let resolved_in_the_database =
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
                ScopeFilter::in_tenant_subtree(
                    pep_properties::OWNER_TENANT_ID,
                    parent,
                    true,
                    Vec::new(),
                ),
            ])]);
        assert!(without_tenants(&resolved_in_the_database, &[child]).is_none());

        // A filter on another property stays as it was; a constraint left
        // with no tenant goes, and the other one stays.
        let owner = Uuid::now_v7();
        let mixed = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![parent, child]),
            ScopeFilter::eq("owner_id", owner),
        ])]);
        assert_eq!(
            without_tenants(&mixed, &[child]).expect("narrows"),
            AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
                ScopeFilter::r#in(
                    pep_properties::OWNER_TENANT_ID,
                    vec![ScopeValue::Uuid(parent)]
                ),
                ScopeFilter::eq("owner_id", owner),
            ])]),
        );
        let two = AccessScope::from_constraints(vec![
            ScopeConstraint::new(vec![ScopeFilter::eq(
                pep_properties::OWNER_TENANT_ID,
                child,
            )]),
            ScopeConstraint::new(vec![ScopeFilter::eq(
                pep_properties::OWNER_TENANT_ID,
                parent,
            )]),
        ]);
        let narrowed = without_tenants(&two, &[child]).expect("narrows");
        assert_eq!(narrowed.constraints().len(), 1, "{narrowed:?}");
        assert!(reaches(&narrowed, parent) && !reaches(&narrowed, child));
    }

    /// A scope of the caller's own tenant alone needs no lookup; a subtree,
    /// an unconstrained scope or one decided in the database does.
    #[test]
    fn only_the_callers_own_tenant_skips_the_lookup() {
        let (own, child) = (Uuid::now_v7(), Uuid::now_v7());
        assert!(only(&AccessScope::for_tenant(own), own));
        assert!(only(&AccessScope::deny_all(), own));
        assert!(!only(&AccessScope::for_tenants(vec![own, child]), own));
        assert!(!only(&AccessScope::allow_all(), own));
        assert!(!only(&AccessScope::for_tenant(child), own));
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
