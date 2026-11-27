//! Tenant-lifecycle cleanup service for in-process consumers.
//!
//! A thin adapter over `GroupService` implementing the SDK
//! `ResourceGroupTenantCleanup` trait: it removes the groups a hard-deleted
//! tenant owns, without `AuthZ` enforcement. See the trait's docs for why this
//! surface cannot be PEP-gated and why it must never be exposed through REST.

use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupTenantCleanup;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::group_service::GroupService;
use crate::domain::repo::GroupRepositoryTrait;

/// Adapter exposing tenant group cleanup through the SDK trait.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct RgTenantCleanupService<GR: GroupRepositoryTrait> {
    group_service: Arc<GroupService<GR>>,
}

impl<GR: GroupRepositoryTrait> RgTenantCleanupService<GR> {
    /// Create a new `RgTenantCleanupService`.
    #[must_use]
    pub fn new(group_service: Arc<GroupService<GR>>) -> Self {
        Self { group_service }
    }
}

#[async_trait]
impl<GR: GroupRepositoryTrait> ResourceGroupTenantCleanup for RgTenantCleanupService<GR> {
    async fn delete_tenant_groups(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        type_code: &str,
    ) -> Result<u64, CanonicalError> {
        // Bypass AuthZ: the tenant is already soft-deleted, so no PEP-gated
        // actor can be admitted for it (see the trait docs). `ctx` is for
        // audit correlation only.
        self.group_service
            .delete_tenant_groups_unscoped(tenant_id, type_code)
            .await
            .map_err(CanonicalError::from)
    }
}
