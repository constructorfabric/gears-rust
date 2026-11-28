//! Resource-ownership check trait used by `delete_tenant`.
//!
//! AM has to reject soft-delete while the tenant still owns something
//! (DESIGN §3.5). AM holds a trait-object slot so the production wiring
//! can plug in the configured answer without threading a third generic
//! parameter through `TenantService<R, P>`:
//!
//! * by default, [`crate::infra::rg::RgResourceOwnershipChecker`] —
//!   whether the tenant owns any resource group in `resource-group`;
//! * with `tenant_ownership.vendor` configured,
//!   [`crate::infra::tenant_ownership::LazyTenantOwnershipChecker`] —
//!   the product's tenant-ownership plugin.
//!
//! [`InertResourceOwnershipChecker`] (always returns `0`) is kept for unit
//! tests only.
//!
//! The check takes no `SecurityContext`: the answer to "does tenant T own
//! anything?" must not depend on who is deleting T. A caller-scoped check
//! lets a caller who cannot see a tenant's resources delete the tenant
//! from under them, and refuses a caller who can. The tenant-level
//! `DELETE` authorization has already run when the check is asked, and the
//! check discloses nothing beyond a count.

use async_trait::async_trait;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Contract for counting what `tenant_id` still owns. A non-zero count
/// rejects soft-delete with [`DomainError::TenantHasResources`].
///
/// Implementations MUST answer from a caller-independent view (see the
/// module docs).
#[async_trait]
pub trait ResourceOwnershipChecker: Send + Sync {
    /// Returns a non-zero count when `tenant_id` still owns something that
    /// blocks its soft-delete. Callers only compare the result against
    /// zero. Any I/O failure MUST be funnelled through [`DomainError`] so
    /// the service layer can surface it through the normal error taxonomy.
    async fn count_ownership_links(&self, tenant_id: Uuid) -> Result<u64, DomainError>;
}

/// No-op checker — always reports zero ownership. Reserved for unit
/// tests; not bound in production, where the gear entry-point always wires
/// either the resource-group checker or the configured plugin.
#[domain_model]
#[derive(Debug, Default, Clone)]
pub struct InertResourceOwnershipChecker;

#[async_trait]
impl ResourceOwnershipChecker for InertResourceOwnershipChecker {
    async fn count_ownership_links(&self, _tenant_id: Uuid) -> Result<u64, DomainError> {
        Ok(0)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inert_checker_always_returns_zero() {
        let c = InertResourceOwnershipChecker;
        assert_eq!(
            c.count_ownership_links(Uuid::from_u128(0x1))
                .await
                .expect("ok"),
            0
        );
    }
}
