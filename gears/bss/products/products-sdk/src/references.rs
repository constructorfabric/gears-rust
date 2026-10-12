//! Owner-bound reference protocol for trusted consumers in the same binary/deployment.
//! The provider binds ownership; callers never supply an owner on an operation.
use crate::models::{ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuVersion};
use async_trait::async_trait;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// Stable identity for pricing's tenant-scoped recovery ticker.
pub const PRICING_SYSTEM_ACTOR: Uuid = Uuid::from_u128(0x00000000_0000_0f01_0000_627373722d70);
/// The subject type of pricing's system actor ([`PRICING_SYSTEM_ACTOR`]).
pub const PRICING_SYSTEM_SUBJECT_TYPE: &str = "bss-pricing.system";
/// Whether `ctx` carries pricing's system actor in either half: the subject type
/// [`PRICING_SYSTEM_SUBJECT_TYPE`] or the id [`PRICING_SYSTEM_ACTOR`]. Only in-process code acts
/// as that actor. A token's claims can carry both halves, so both gears refuse such a caller at
/// every REST door, 403 `SYSTEM_ACTOR_RESERVED`, after authentication and before the PDP
/// (products P-D-222, pricing D-424). Other system subjects (Rating's, Subscriptions') are not
/// refused.
#[must_use]
pub fn is_pricing_system_actor(ctx: &SecurityContext) -> bool {
    ctx.subject_type() == Some(PRICING_SYSTEM_SUBJECT_TYPE)
        || ctx.subject_id() == PRICING_SYSTEM_ACTOR
}
/// Pricing-specific `ClientHub` key. Products constructs the bound implementation.
pub struct PricingReferenceRegistry(pub Arc<dyn ReferenceRegistryV1>);
/// Same reservation rules and error codes as the Products REST reference door.
/// Missing or foreign-owner batch entries fail the batch; order follows the input.
/// [`ReferenceRegistryV1::skus_for_write`] is the exception (P-D-245): an id the tenant does not
/// hold, or the caller's scope does not admit, is left out, and the caller is judged once.
///
/// Every method first checks the caller: `tenant` must be the caller's own tenant, and a system
/// subject other than pricing's (`bss-pricing.system`, [`PRICING_SYSTEM_ACTOR`]) is refused, both
/// 403 `REFERENCE_OWNER_MISMATCH`. Pricing's system actor is trusted in-process; any other caller
/// is judged by the PDP on the SKU (`reference` for the four reference methods, `read` for the SKU
/// reads), 403 when it denies and 503 when it cannot answer (products P-D-222). A storage
/// failure is a 500.
#[async_trait]
pub trait ReferenceRegistryV1: Send + Sync {
    /// Reserve the logical reference `(owner, kind, ref_id)` on the SKU `sku_id`, idempotently: a
    /// live reservation of the same reference on the same SKU is answered as it stands.
    ///
    /// # Errors
    /// 404 for a SKU the tenant does not hold; 409 `REFERENCE_EXISTS` when the reference is live on
    /// another SKU (or a concurrent reservation won it twice), `SKU_RETIRING` for a retiring SKU and
    /// `SKU_FENCED` for a fenced SKU or one that is neither published nor deprecated.
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_id: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError>;
    /// Confirm a reservation, idempotently: a confirmed one is answered `Ok` again.
    ///
    /// # Errors
    /// 404 for a reservation the tenant does not hold; 403 `REFERENCE_OWNER_MISMATCH` for one
    /// another owner holds; 409 `REFERENCE_RELEASED` for a released one, which never comes back.
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        reservation_id: Uuid,
    ) -> Result<(), CanonicalError>;
    /// Release a reservation, once: a released one is answered `Ok` again and keeps its first
    /// release's attribution.
    ///
    /// # Errors
    /// 404 for a reservation the tenant does not hold; 403 `REFERENCE_OWNER_MISMATCH` for one
    /// another owner holds.
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        reservation_id: Uuid,
    ) -> Result<(), CanonicalError>;
    /// The state of each reservation, in the order asked.
    ///
    /// # Errors
    /// The batch fails as a whole on the first id, in order, that the tenant does not hold (404)
    /// or that another owner holds (403 `REFERENCE_OWNER_MISMATCH`).
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        reservation_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError>;
    /// The SKU head a write binds to, in whatever lifecycle it is, draft to retired: the caller
    /// judges whether that lifecycle admits its write.
    ///
    /// # Errors
    /// 404 for a SKU the tenant does not hold.
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_id: Uuid,
    ) -> Result<Sku, CanonicalError>;
    /// The SKU heads of the distinct ids the caller may read, in `sku_ids` order. An id the tenant
    /// does not hold, or the caller's scope does not admit, is left out (the per-id 404). The caller
    /// is judged once (P-D-222): 403 / 503 fail the whole call.
    ///
    /// The default reads [`sku_for_write`](Self::sku_for_write) per distinct id, skips a 404 and
    /// propagates every other error. Products' registry overrides it with one statement
    /// (P-D-245).
    ///
    /// # Errors
    /// 403 when the caller is refused, 503 when the policy cannot answer, 500 for a storage
    /// failure. A missing id is left out, not an error.
    async fn skus_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<Sku>, CanonicalError> {
        let mut seen = std::collections::BTreeSet::new();
        let mut found = Vec::new();
        for id in sku_ids {
            if !seen.insert(*id) {
                continue;
            }
            match self.sku_for_write(ctx, tenant, *id).await {
                Ok(sku) => found.push(sku),
                Err(error) if error.status_code() == 404 => {}
                Err(error) => return Err(error),
            }
        }
        Ok(found)
    }
    /// The SKU's version in force on `date`: of the versions effective on or before it, the
    /// latest date's highest `published_version`. `Ok(None)` when none is in force yet, for a SKU
    /// never published or a date before its first version.
    ///
    /// # Errors
    /// 404 for a SKU the tenant does not hold; 500 for a stored version whose content does not
    /// read.
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError>;
}
