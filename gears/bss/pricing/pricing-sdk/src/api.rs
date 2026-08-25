//! The in-process read contract consumers resolve from `ClientHub`.
//!
//! Read-model resolution itself (`{skuId, planId, priceId}`, model kind, tier
//! bands, evaluation-policy fields, window intervals, the consumer contracts)
//! arrives with the slices that own those payloads; the Foundation-owned entry
//! point is the frontier a consumer pins **before** resolving anything.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::catalog_version::PinFrontier;

/// The published-catalog read contract (`cpt-cf-bss-pricing-interface-catalog-read-model`).
///
/// Callers are service identities (Tariffs, Rating, Subscriptions, Billing)
/// holding `plan × read`; the value is tenant-scoped like every read.
#[async_trait::async_trait]
pub trait PricingCatalogClientV1: Send + Sync {
    /// Read the caller's tenant pin-eligibility frontier — the version to pin
    /// for the whole of a resolution or rating run — or `None` when the tenant
    /// has no pin-eligible version yet.
    ///
    /// Consumers MUST pin this value rather than deriving one: resolving at a
    /// version that is not pin-eligible can make a single pin resolve two
    /// different contents over time (D-101/D-114). On read-model outage this
    /// fails closed; it never serves a stale frontier.
    ///
    /// `None` is a **state, not an error**: a tenant whose first publish has not
    /// completed has nothing to pin, and that is a normal condition at
    /// onboarding rather than a fault. It is deliberately not folded into an
    /// error either, because this gear answers absent-or-out-of-scope with one
    /// indistinguishable rejection — reporting "no frontier yet" that way would
    /// make it indistinguishable from a denial. A caller that cannot proceed
    /// without a version decides that for itself.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies,
    /// `Unavailable` on read-model outage.
    async fn pin_frontier(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Option<PinFrontier>, CanonicalError>;
}
