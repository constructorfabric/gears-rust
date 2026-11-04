//! Canonical-error builders of the Orders gear that the domain and infra layers raise (the
//! precedent is the ledger and products gears' `infra/error_mapping.rs`): the order-resource
//! `resource_exhausted` errors of the limiter refusal and of D-188 version exhaustion.
//! REST renders them; it does not own them.

use toolkit_canonical_errors::resource_error;

/// Canonical scope of the Orders edge limiter's refusals: the order resource.
#[resource_error(toolkit_gts::gts_id!("cf.bss.orders.order.v1~"))]
pub struct OrdersOrderError;

/// The quota subject of an order whose commercial version numbers are exhausted (D-188).
pub const VERSION_ALLOCATION_SUBJECT: &str = "version-allocation";

/// The canonical `resource_exhausted` error of D-188 candidate exhaustion ("reject before any
/// Pricing command with the existing canonical resource-exhaustion Problem"): scoped to the Orders
/// order resource like [`rejection_error`](super::throttle::rejection_error), with the quota violation subject
/// [`VERSION_ALLOCATION_SUBJECT`] and no retry hint, because the quota never replenishes.
#[must_use]
pub fn version_allocation_exhausted() -> toolkit_canonical_errors::CanonicalError {
    OrdersOrderError::resource_exhausted("Orders commercial version numbers exhausted")
        .with_quota_violation(
            VERSION_ALLOCATION_SUBJECT,
            "the order has no commercial version number left to reserve",
        )
        .create()
}
