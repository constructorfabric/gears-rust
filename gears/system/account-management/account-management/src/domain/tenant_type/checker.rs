//! Pre-write tenant-type barrier for Feature 2.3 (`tenant-type-enforcement`).
//!
//! Before changing `tenants` or `tenant_closure`, check the child's effective
//! `allowed_parent_types` against the parent's chained ID. Same-type nesting
//! requires the type's own ID in that list (`algo-same-type-nesting-admission`).
//!
//! [`InertTenantTypeChecker`] admits everything in unit tests only. Production
//! wires [`crate::infra::types_registry::GtsTenantTypeChecker`] through the local
//! database-backed registry; a missing `TypesRegistryApi` fails AM init.
//! Each check reads both schemas in one batch for the request's [`SecurityContext`]
//! (P33). See the adapter for registry error classification.
//!
//! Disallowed pairings return [`DomainError::TypeNotAllowed`] (400,
//! `failed_precondition`, `TYPE_NOT_ALLOWED`); unregistered types or malformed
//! traits return [`DomainError::InvalidTenantType`] (400, `invalid_argument`,
//! `INVALID_TENANT_TYPE`).

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Pre-write tenant-type compatibility barrier.
///
/// The child's effective `allowed_parent_types` must contain the parent's
/// chained ID, including its own ID for same-type nesting
/// (`algo-same-type-nesting-admission`).
///
/// Read for the supplied request caller without retaining context or caching
/// type definitions across calls (P33,
/// `dod-tenant-type-enforcement-gts-availability-surface`).
#[async_trait]
pub trait TenantTypeChecker: Send + Sync {
    /// Validate parent-child type compatibility.
    ///
    /// # Errors
    ///
    /// * [`DomainError::TypeNotAllowed`] — the parent type is not a member of
    ///   the child type's effective `allowed_parent_types`, or same-type
    ///   nesting was requested but the trait does not include the child
    ///   type's own identifier.
    /// * [`DomainError::InvalidTenantType`] — the child type is not
    ///   registered or its effective trait is malformed.
    /// * [`DomainError::ServiceUnavailable`] — the GTS Types Registry is
    ///   unreachable, times out, or returns a trait-resolution failure.
    // @cpt-begin:cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation:p1:inst-algo-apte-trait-contract
    // @cpt-begin:cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission:p1:inst-algo-stn-trait-contract
    async fn check_parent_child(
        &self,
        ctx: &SecurityContext,
        parent_type: Uuid,
        child_type: Uuid,
    ) -> Result<(), DomainError>;
    // @cpt-end:cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission:p1:inst-algo-stn-trait-contract
    // @cpt-end:cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation:p1:inst-algo-apte-trait-contract
}

/// No-op checker — admits every parent / child pairing. Reserved for
/// unit tests; not bound in production: the gear entry-point
/// fail-closes `init` when `TypesRegistryApi` cannot be resolved from
/// `ClientHub`.
#[domain_model]
#[derive(Debug, Default, Clone)]
pub struct InertTenantTypeChecker;

#[async_trait]
impl TenantTypeChecker for InertTenantTypeChecker {
    async fn check_parent_child(
        &self,
        _ctx: &SecurityContext,
        _parent_type: Uuid,
        _child_type: Uuid,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

/// Build an `Arc<dyn TenantTypeChecker>` over [`InertTenantTypeChecker`].
/// Used by unit tests so callers don't have to spell out the
/// `Arc::new(InertTenantTypeChecker)` form at every site.
#[must_use]
pub fn inert_tenant_type_checker() -> Arc<dyn TenantTypeChecker + Send + Sync> {
    Arc::new(InertTenantTypeChecker)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inert_checker_admits_every_pairing() {
        let c = InertTenantTypeChecker;
        let parent = Uuid::from_u128(0x1);
        let child = Uuid::from_u128(0x2);
        c.check_parent_child(&SecurityContext::anonymous(), parent, child)
            .await
            .expect("inert admits any pairing");
    }

    #[tokio::test]
    async fn inert_checker_admits_same_type_nesting() {
        let c = InertTenantTypeChecker;
        let same = Uuid::from_u128(0x3);
        c.check_parent_child(&SecurityContext::anonymous(), same, same)
            .await
            .expect("inert admits same-type nesting");
    }
}
