//! Per-operation permission checks for the Management API (DESIGN §
//! Authentication & Authorization): `create`, `override`, `read` and
//! `delete` on the upstream and route GTS types.
//!
//! Only the API/SDK entry points of the Control Plane check them. The
//! registry reconcile writes as the gear itself and asks the policy engine
//! only for ancestor binds (ADR-0018).

use authz_resolver_sdk::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use oagw_sdk::{ROUTE_SCHEMA, UPSTREAM_SCHEMA};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::{DeniedOn, DomainError};

/// Properties a management grant may be constrained on. `id` is listed so a
/// row-level grant compiles and [`coverage`] refuses it as
/// `AUTHZ_SCOPE_NOT_SUPPORTED`, rather than failing compilation (a 500).
const MANAGED_PROPERTIES: &[&str] = &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// Upstream resource.
pub(in crate::domain::services) static UPSTREAM_RESOURCE: ResourceType =
    ResourceType::from_static(UPSTREAM_SCHEMA, MANAGED_PROPERTIES);

/// Upstream resource of the ancestor bind checks. They take the policy's
/// decision and do not apply the grant's scope, so a grant constrained on
/// `id` must fail compilation (fail closed) rather than allow a bind to any
/// ancestor upstream.
pub(in crate::domain::services) static BIND_UPSTREAM_RESOURCE: ResourceType =
    ResourceType::from_static(UPSTREAM_SCHEMA, &[pep_properties::OWNER_TENANT_ID]);

/// Route resource.
pub(in crate::domain::services) static ROUTE_RESOURCE: ResourceType =
    ResourceType::from_static(ROUTE_SCHEMA, MANAGED_PROPERTIES);

/// A management operation: the action the policy engine is asked for, and
/// the resource it acts on when there is one.
#[domain_model]
#[derive(Clone, Copy)]
pub(in crate::domain::services) enum Action {
    Create,
    /// List, or read a resolved configuration.
    List,
    Read(Uuid),
    /// Update (full replacement).
    Override(Uuid),
    Delete(Uuid),
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::List | Self::Read(_) => "read",
            Self::Override(_) => "override",
            Self::Delete(_) => "delete",
        }
    }

    fn resource_id(self) -> Option<Uuid> {
        match self {
            Self::Create | Self::List => None,
            Self::Read(id) | Self::Override(id) | Self::Delete(id) => Some(id),
        }
    }
}

/// Which management resource a check is for. Selects the resource type and
/// the entity a denial is reported on.
#[domain_model]
#[derive(Clone, Copy)]
pub(in crate::domain::services) enum Managed {
    Upstream,
    Route,
}

impl Managed {
    fn resource(self) -> &'static ResourceType {
        match self {
            Self::Upstream => &UPSTREAM_RESOURCE,
            Self::Route => &ROUTE_RESOURCE,
        }
    }

    fn entity(self) -> DeniedOn {
        match self {
            Self::Upstream => DeniedOn::Upstream,
            Self::Route => DeniedOn::Route,
        }
    }
}

/// Ask the policy engine whether the caller may run `action` on a management
/// resource of its own tenant.
///
/// The repositories already scope rows to the caller's tenant, so no row
/// constraints are required.
pub(in crate::domain::services) async fn authorize(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    managed: Managed,
    action: Action,
) -> Result<(), DomainError> {
    authorize_owned(enforcer, ctx, managed, action, ctx.subject_tenant_id()).await
}

/// [`authorize`] on a resource that `owner` owns, which may be an ancestor
/// of the caller's tenant (a resolved proxy target).
pub(in crate::domain::services) async fn authorize_owned(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    managed: Managed,
    action: Action,
    owner: Uuid,
) -> Result<(), DomainError> {
    let (id, action) = (action.resource_id(), action.name());
    let tenant_id = ctx.subject_tenant_id();
    let mut request = AccessRequest::new()
        .require_constraints(false)
        .context_tenant_id(tenant_id)
        .resource_property(pep_properties::OWNER_TENANT_ID, owner);
    if owner == tenant_id {
        // The call acts on the caller's tenant alone, and [`coverage`] needs
        // only that tenant: the policy engine need not expand the subtree.
        request = request.tenant_mode(TenantMode::RootOnly);
    }
    let denied = |why: &str, policy_reason: Option<&str>| {
        tracing::info!(
            subject_id = %ctx.subject_id(),
            tenant_id = %tenant_id,
            resource_type = managed.resource().name(),
            action,
            resource_id = ?id,
            owner_tenant_id = %owner,
            why,
            policy_reason,
            "OAGW management call denied"
        );
    };
    let scope = enforcer
        .access_scope_with(ctx, managed.resource(), action, id, &request)
        .await
        .map_err(|e| {
            if let EnforcerError::AccessDenied(authz_resolver_sdk::AccessDenial::Pdp {
                reason: deny_reason,
            }) = &e
            {
                denied(
                    "policy denied",
                    deny_reason.as_ref().map(|r| r.error_code.as_str()),
                );
            }
            DomainError::from(e).on_entity(managed.entity())
        })?;
    require_tenant_coverage(&scope, owner, managed.entity()).inspect_err(|e| {
        if let DomainError::Forbidden { reason, .. } = e {
            // AUTHZ_DENIED: no part of the tenant; AUTHZ_SCOPE_NOT_SUPPORTED:
            // only part of it.
            denied("grant does not cover the tenant", Some(reason));
        }
    })
}

/// Refuse a granted `scope` that does not cover every row `owner` owns. The
/// repositories filter by tenant only, so a narrower grant cannot be applied
/// (fail closed). A grant on other tenants only is a plain denial; one on
/// part of the tenant is `AUTHZ_SCOPE_NOT_SUPPORTED`.
fn require_tenant_coverage(
    scope: &AccessScope,
    owner: Uuid,
    entity: DeniedOn,
) -> Result<(), DomainError> {
    match coverage(scope, owner) {
        Coverage::Whole => Ok(()),
        Coverage::None => Err(DomainError::forbidden("access denied by policy").on_entity(entity)),
        Coverage::Partial => Err(DomainError::forbidden_with_reason(
            oagw_sdk::reason::permission::AUTHZ_SCOPE_NOT_SUPPORTED,
            "the policy grants this only on part of the tenant",
        )
        .on_entity(entity)),
    }
}

/// How much of a tenant a constrained grant covers.
#[domain_model]
enum Coverage {
    /// Every row of the tenant.
    Whole,
    /// Some rows only, or a filter only a database can decide.
    Partial,
    /// No row of the tenant.
    None,
}

/// How much of `tenant_id` `scope` grants. Whole: unconstrained, or one
/// constraint filters only on `owner_tenant_id` with values that include the
/// tenant (`eq` or `in`, as tr-authz answers). None: no constraint can admit
/// a row of the tenant. Partial: anything else.
fn coverage(scope: &AccessScope, tenant_id: Uuid) -> Coverage {
    if scope.is_unconstrained() {
        return Coverage::Whole;
    }
    let admits_tenant = |filter: &toolkit_security::ScopeFilter| {
        filter.property() == pep_properties::OWNER_TENANT_ID
            && filter.is_representable_in_memory()
            && filter.uuid_values().contains(&tenant_id)
    };
    let excludes_tenant = |filter: &toolkit_security::ScopeFilter| {
        filter.property() == pep_properties::OWNER_TENANT_ID
            && filter.is_representable_in_memory()
            && !filter.uuid_values().contains(&tenant_id)
    };
    let constraints = scope.constraints();
    if constraints
        .iter()
        .any(|c| !c.is_empty() && c.filters().iter().all(admits_tenant))
    {
        Coverage::Whole
    } else if constraints
        .iter()
        .all(|c| c.filters().iter().any(excludes_tenant))
    {
        Coverage::None
    } else {
        Coverage::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_security::{ScopeConstraint, ScopeFilter};

    fn scope(filters: Vec<ScopeFilter>) -> AccessScope {
        AccessScope::single(ScopeConstraint::new(filters))
    }

    /// A partial grant is refused as unsupported, on the given resource.
    #[test]
    fn partial_grant_is_refused_as_unsupported() {
        let tenant = Uuid::new_v4();
        let partial = scope(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
            ScopeFilter::eq("id", Uuid::new_v4()),
        ]);
        match require_tenant_coverage(&partial, tenant, DeniedOn::Route) {
            Err(DomainError::Forbidden { reason, entity, .. }) => {
                assert_eq!(
                    reason,
                    oagw_sdk::reason::permission::AUTHZ_SCOPE_NOT_SUPPORTED
                );
                assert_eq!(entity, DeniedOn::Route);
            }
            other => panic!("expected a scope refusal, got {other:?}"),
        }
        assert!(
            require_tenant_coverage(&AccessScope::allow_all(), tenant, DeniedOn::Route).is_ok()
        );
    }

    #[test]
    fn coverage_classifies_constrained_grants() {
        let tenant = Uuid::new_v4();
        let other = Uuid::new_v4();
        let owner = pep_properties::OWNER_TENANT_ID;

        assert!(matches!(
            coverage(&AccessScope::allow_all(), tenant),
            Coverage::Whole
        ));
        assert!(matches!(
            coverage(
                &scope(vec![ScopeFilter::in_uuids(owner, vec![other, tenant])]),
                tenant
            ),
            Coverage::Whole
        ));
        assert!(matches!(
            coverage(&scope(vec![ScopeFilter::eq(owner, other)]), tenant),
            Coverage::None
        ));
        // Decided in SQL only: not taken as the whole tenant, even rooted at it.
        assert!(matches!(
            coverage(
                &scope(vec![ScopeFilter::in_tenant_subtree(
                    owner,
                    tenant,
                    true,
                    Vec::new()
                )]),
                tenant
            ),
            Coverage::Partial
        ));
        // The tenant plus a row filter: part of the tenant.
        assert!(matches!(
            coverage(
                &scope(vec![
                    ScopeFilter::eq(owner, tenant),
                    ScopeFilter::eq("id", Uuid::new_v4()),
                ]),
                tenant
            ),
            Coverage::Partial
        ));
    }
}
