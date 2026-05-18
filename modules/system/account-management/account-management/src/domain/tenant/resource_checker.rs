//! Resource-ownership check trait used by `delete_tenant`.
//!
//! AM has to reject soft-delete when the tenant still owns resource-group
//! rows (DESIGN §3.5). The check itself is owned by the `resource-group`
//! module, which exposes a typed client; AM holds a trait-object slot so
//! the production wiring can plug in the real client without threading a
//! third generic parameter through `TenantService<R, P>`.
//!
//! Production binds [`crate::infra::rg::RgResourceOwnershipChecker`] —
//! `resource-group` is a hard `deps` of AM, so the production wiring
//! always has a real client. [`InertResourceOwnershipChecker`] (always
//! returns `0`) is kept for unit tests only.

use async_trait::async_trait;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Contract for counting the number of resource-group rows that still
/// name `tenant_id` as their owner. A non-zero count rejects soft-delete
/// with [`DomainError::TenantHasResources`].
///
/// `ctx` is the soft-delete caller's security context — propagated to RG
/// so its `AuthZ` + `SecureORM` apply the caller's `AccessScope` (a parent
/// has read access to its children's resources per RG PRD §4). The
/// implementation narrows further to `tenant_id` via `OData` filter so the
/// answer is scoped to *that specific* child rather than the whole
/// reachable subtree.
#[async_trait]
pub trait ResourceOwnershipChecker: Send + Sync {
    /// Returns the number of RG rows owned by `tenant_id`. Any I/O
    /// failure MUST be funnelled through [`DomainError`] so the service
    /// layer can surface it through the normal error taxonomy.
    async fn count_ownership_links(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<u64, DomainError>;
}

/// No-op checker — always reports zero ownership. Reserved for unit
/// tests; not bound in production. AM declares `resource-group` as a
/// hard `deps` and the module entry-point fail-closes `init` when no
/// `ResourceGroupClient` resolves from `ClientHub`, so the
/// `RgResourceOwnershipChecker`-backed probe is always wired in
/// production.
#[domain_model]
#[derive(Debug, Default, Clone)]
pub struct InertResourceOwnershipChecker;

#[async_trait]
impl ResourceOwnershipChecker for InertResourceOwnershipChecker {
    async fn count_ownership_links(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<u64, DomainError> {
        Ok(0)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inert_checker_always_returns_zero() {
        // Use a non-anonymous ctx — the production
        // `ResourceOwnershipChecker` (RG-side) requires a real
        // caller identity per the trait doc, so the inert default
        // must accept the same shape callers will send in practice.
        // Accepting `SecurityContext::anonymous()` here would mask
        // a future contract drift where the real checker rejects
        // anonymous contexts at the entry point.
        let c = InertResourceOwnershipChecker;
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(0xCAFE))
            .subject_tenant_id(Uuid::from_u128(0x100))
            .build()
            .expect("ctx");
        assert_eq!(
            c.count_ownership_links(&ctx, Uuid::from_u128(0x1))
                .await
                .expect("ok"),
            0
        );
    }
}
