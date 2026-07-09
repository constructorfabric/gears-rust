//! Seller-type guard for provisioning. Per the architecture's §4.12 predicate,
//! only a tenant whose TYPE owns a billing ledger (a "seller") may have its
//! ledger seeded. The guard reads the target tenant's chained GTS tenant-type
//! from AM (`get_tenant`) and checks it against the gear-configured seller set
//! (`BssLedgerConfig::seller_tenant_types`), rejecting a non-owner (buyer/leaf)
//! type. The seller predicate is owned by the ledger, not encoded as an AM
//! tenant-type trait — GTS mandates closed (`additionalProperties: false`)
//! trait schemas, so a downstream `bss_ledger_owner` trait is not registrable.

use std::collections::HashSet;
use std::sync::Arc;

use account_management_sdk::AccountManagementClient;
use async_trait::async_trait;
use toolkit::api::canonical_prelude::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Stamps `resource_type` on the rejection (the seller's ledger).
#[resource_error("gts.cf.bss.ledger.ledger.v1~")]
struct LedgerResource;

/// Narrow port: resolve a tenant's chained GTS tenant-type id. Adapts AM's
/// `get_tenant` so the guard is unit-testable without faking the whole client.
#[async_trait]
pub(crate) trait TenantTypeReader: Send + Sync {
    /// The tenant's chained `gts.cf.core.am.tenant_type.v1~…` id, or `None`
    /// when AM resolved no type (the field is best-effort on a registry blip).
    ///
    /// # Errors
    /// The AM `CanonicalError` — `NotFound` for an unknown / out-of-subtree
    /// tenant, `PermissionDenied`, or a transient `Internal`/`Unavailable`.
    async fn tenant_type(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Option<String>, CanonicalError>;
}

/// [`TenantTypeReader`] backed by the AM [`AccountManagementClient`].
pub(crate) struct AmTenantTypeReader {
    am: Arc<dyn AccountManagementClient>,
}

impl AmTenantTypeReader {
    pub(crate) fn new(am: Arc<dyn AccountManagementClient>) -> Self {
        Self { am }
    }
}

#[async_trait]
impl TenantTypeReader for AmTenantTypeReader {
    async fn tenant_type(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Option<String>, CanonicalError> {
        // `get_tenant` is itself subtree-scoped: a target outside the caller's
        // PDP subtree surfaces as `NotFound` (a second BOLA layer beneath the
        // provisioning authz gate).
        Ok(self.am.get_tenant(ctx, tenant_id).await?.tenant_type)
    }
}

/// Gate: assert the target tenant's TYPE is a configured seller (ledger owner)
/// before its reference rows are seeded.
pub(crate) struct SellerGuard {
    tenant_types: Arc<dyn TenantTypeReader>,
    /// Chained GTS tenant-type ids that own a ledger (from `BssLedgerConfig`).
    seller_types: HashSet<String>,
}

impl SellerGuard {
    pub(crate) fn new(
        tenant_types: Arc<dyn TenantTypeReader>,
        seller_types: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            tenant_types,
            seller_types: seller_types.into_iter().collect(),
        }
    }

    /// `Ok(())` when the target tenant's type is in the configured seller set.
    ///
    /// # Errors
    /// `FailedPrecondition` when the tenant has no resolved type or its type is
    /// not a seller (a buyer/leaf); otherwise propagates the AM `CanonicalError`
    /// (`NotFound` for an unknown / out-of-subtree tenant, transient
    /// `Internal`/`Unavailable`).
    pub(crate) async fn assert_owns_ledger(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<(), CanonicalError> {
        let tenant_type = self
            .tenant_types
            .tenant_type(ctx, tenant_id)
            .await?
            .ok_or_else(|| {
                LedgerResource::failed_precondition()
                    .with_precondition_violation(
                        "tenant_type",
                        "tenant has no resolved type; cannot confirm it owns a ledger".to_owned(),
                        "TENANT_TYPE_UNKNOWN",
                    )
                    .create()
            })?;
        if self.seller_types.contains(&tenant_type) {
            Ok(())
        } else {
            Err(LedgerResource::failed_precondition()
                .with_precondition_violation(
                    "tenant_type",
                    format!("tenant type {tenant_type} does not own a billing ledger"),
                    "TENANT_TYPE_NOT_LEDGER_OWNER",
                )
                .create())
        }
    }
}

#[cfg(test)]
#[path = "seller_guard_tests.rs"]
mod tests;
