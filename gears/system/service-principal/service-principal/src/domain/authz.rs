//! PDP authorization gate for service-principal management.
//!
//! Tenant-scoped: the PEP resource carries only `OWNER_TENANT_ID`. The
//! `(tenant_id, client_id)` coupling is enforced downstream by the SPI adapter
//! (a `client_id` not owned by `tenant_id` is `NotFound`), so no per-instance
//! UUID authz is needed here.

use authz_resolver_sdk::pep::{EnforcerError, ResourceType};
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// PDP resource type for the service principal. The id comes from the SDK's single
/// source of truth ([`service_principal_sdk::SERVICE_PRINCIPAL_RESOURCE_TYPE`]).
pub const SERVICE_PRINCIPAL: ResourceType = ResourceType::from_static(
    service_principal_sdk::SERVICE_PRINCIPAL_RESOURCE_TYPE,
    &[pep_properties::OWNER_TENANT_ID],
);

/// Per-verb RBAC actions (matching account-management's identity-management style,
/// not the coarse read/write/delete triad). `rotate_secret` is a distinct action
/// so credential minting can be granted independently.
pub mod actions {
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    pub const ROTATE_SECRET: &str = "rotate_secret";
    pub const REVOKE: &str = "revoke";
}

/// Map a PEP enforcement failure to a domain error (fail-closed).
/// `Denied` / `CompileFailed` → `AccessDenied` (403); `EvaluationFailed` → `Upstream` (503).
#[must_use]
// By-value (not `&EnforcerError`) so this can be passed directly as a bare
// function pointer to `Result::map_err` at the call site (e.g.
// `.map_err(authz::map_enforcer_err)`), which requires `FnOnce(EnforcerError) -> _`.
#[allow(clippy::needless_pass_by_value)]
pub fn map_enforcer_err(err: EnforcerError) -> DomainError {
    match err {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => DomainError::AccessDenied,
        EnforcerError::EvaluationFailed(_) => DomainError::Upstream {
            detail: "authorization evaluation failed".to_owned(),
        },
    }
}

/// Object-level (BOLA) check: the PDP-returned `scope` must actually cover the
/// explicit target `tenant`. A `decision=true` for *some* subtree must never
/// authorize a *different* target tenant — hence this check rather than trusting
/// the bare decision. `allow_all` (a legitimate unconstrained PDP outcome, e.g. a
/// platform superuser) permits any tenant.
///
/// # Errors
/// Returns [`DomainError::AccessDenied`] when the scope does not cover `tenant`.
pub fn ensure_scope_permits(scope: &AccessScope, tenant: Uuid) -> Result<(), DomainError> {
    if scope.is_unconstrained() || scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant) {
        Ok(())
    } else {
        Err(DomainError::AccessDenied)
    }
}

#[cfg(test)]
mod tests {
    use authz_resolver_sdk::pep::EnforcerError;
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn allow_all_scope_permits_any_tenant() {
        assert!(ensure_scope_permits(&AccessScope::allow_all(), Uuid::new_v4()).is_ok());
    }

    #[test]
    fn scope_permits_only_the_covered_tenant() {
        let target = Uuid::new_v4();
        let scope = AccessScope::for_tenant(target);
        assert!(ensure_scope_permits(&scope, target).is_ok());
        // A grant covering a *different* tenant must not authorize this target.
        assert!(matches!(
            ensure_scope_permits(&scope, Uuid::new_v4()),
            Err(DomainError::AccessDenied)
        ));
    }

    #[test]
    fn deny_all_scope_permits_nothing() {
        assert!(matches!(
            ensure_scope_permits(&AccessScope::deny_all(), Uuid::new_v4()),
            Err(DomainError::AccessDenied)
        ));
    }

    #[test]
    fn enforcer_errors_fail_closed() {
        assert!(matches!(
            map_enforcer_err(EnforcerError::Denied { deny_reason: None }),
            DomainError::AccessDenied
        ));
    }

    #[test]
    fn compile_failed_fails_closed_as_access_denied() {
        // `ConstraintsRequiredButAbsent` is the simplest real value the SDK
        // exposes for this variant: the PDP was asked for row-level
        // constraints (require_constraints(true)) and returned none, which
        // the compiler treats as a fail-closed deny.
        assert!(matches!(
            map_enforcer_err(EnforcerError::CompileFailed(
                authz_resolver_sdk::pep::ConstraintCompileError::ConstraintsRequiredButAbsent
            )),
            DomainError::AccessDenied
        ));
    }

    #[test]
    fn evaluation_failed_maps_to_upstream() {
        // `NoPluginAvailable` is the simplest real value the SDK exposes for
        // this variant (no AuthZ plugin registered to serve the request).
        assert!(matches!(
            map_enforcer_err(EnforcerError::EvaluationFailed(
                authz_resolver_sdk::AuthZResolverError::NoPluginAvailable
            )),
            DomainError::Upstream { .. }
        ));
    }
}
