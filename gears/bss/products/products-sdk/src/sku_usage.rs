//! Pricing's usage of SKUs, as the SKU reads show it — one port that pricing
//! fills (**P-D-197**; pricing **D-428**).
//!
//! # Why the port is here and pricing fills it
//!
//! The SKUs screen shows, per SKU, how many prices and plans use it ("5 prices"
//! with currency chips, "unpriced", "3 plans"). Those facts are pricing's, and
//! products must not depend on pricing. So the contract sits in this crate,
//! pricing implements it and registers it in `ClientHub` at its init as
//! `dyn SkuUsageV1`, and the gear resolves it at each read — not at its own
//! init, because the two gears boot in either order.
//!
//! # Information, never a fence input
//!
//! A SKU read shows the answer, or `null` when no port is registered, when it
//! refuses the caller, or when it cannot answer; the read never fails for it.
//! Fences, retirement and type changes read the local reference registry only
//! (P-D-188, P-D-194): no remote count sits on a fence.
//!
//! # A filter, when the caller asks for one
//!
//! The SKU list filters by the same facts (`priced`, `in_plan`; **P-D-212**):
//! [`SkuUsageV1::usage_sets`] answers the tenant's priced and in-plan SKUs as
//! two sets. There the answer is not decoration: a refusal is the list's 403,
//! and an absent port, an error or a call past its bound the list's 503 —
//! never an unfiltered page.
//!
//! # A picker's scope
//!
//! The pickers narrow the list to the SKUs one book prices or one plan revision
//! names, or to the others (`priced_in`, `not_priced_in`, `not_in_revision`;
//! **P-D-246**): [`SkuUsageV1::sku_ids_in`] answers the SKUs of one
//! [`UsageScope`]. As with the sets, a refusal is the list's 403 and anything
//! else that is not an answer its 503 — never an empty set.
//!
//! # No serde here
//!
//! As in [`crate::usage_types`]: the gear's REST DTOs own serde and map onto
//! these types.

use async_trait::async_trait;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuUsageResource;

/// The canonical error when the port refuses the caller — a **403**: the
/// caller holds no pricing `price_book_entry:read`. A SKU read shows it as
/// `usage: null`. Carries no PDP detail, which stays in the implementation's
/// logs.
#[must_use]
pub fn sku_usage_denied() -> CanonicalError {
    SkuUsageResource::permission_denied()
        .with_reason("the SKU usage port refused this caller")
        .create()
}

/// The canonical error when the port cannot answer — a **503**: its storage or
/// its authorization is unavailable. A SKU read shows it as `usage: null`.
#[must_use]
pub fn sku_usage_unavailable(detail: impl Into<String>) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

/// A SKU's prices by state; a rejected price is not counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PriceCounts {
    pub approved: u64,
    pub pending: u64,
    pub draft: u64,
}

/// What pricing reports about one SKU (pricing D-428).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkuUsage {
    /// The SKU asked about.
    pub sku_id: Uuid,
    /// The SKU's price-book entries in every book of the tenant, in every
    /// reference state.
    pub entries: u64,
    /// The distinct currencies of those entries' books, sorted.
    pub currencies: Vec<String>,
    /// The prices of those entries, added up by state.
    pub prices: PriceCounts,
    /// The distinct plans with a draft, pending, scheduled or published
    /// revision whose items name one of those entries — distinct across the
    /// SKU's entries, never a sum of the entries' counts. The revisions are
    /// counted by their stored state (pricing D-453).
    pub plans: u64,
}

/// The tenant's SKUs pricing uses, as two sets (P-D-212): what the SKU list's
/// `priced` and `in_plan` filters keep or drop. Each is sorted and distinct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkuUsageSets {
    /// The SKUs with an entry in a book of the tenant, in any reference state:
    /// exactly those whose [`SkuUsage::entries`] is above zero.
    pub priced: Vec<Uuid>,
    /// The SKUs whose entries a plan item of a draft, pending, scheduled or
    /// published revision names: exactly those whose [`SkuUsage::plans`] is
    /// above zero.
    /// An item that names a SKU without an entry does not count, as it does not
    /// in `plans`.
    pub in_plan: Vec<Uuid>,
}

/// What [`SkuUsageV1::sku_ids_in`] reads the SKUs of (P-D-246).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UsageScope {
    /// The SKUs with an entry in this price book, in any reference state.
    Book(Uuid),
    /// The SKUs this plan revision's items name: plan content.
    Revision(Uuid),
}

/// Pricing's usage of SKUs, which pricing registers on `ClientHub` as
/// `dyn SkuUsageV1`.
///
/// **Cancel-safety.** Products runs each call on a task of its own and aborts
/// it at its bound, or when the read that asked is dropped
/// (`api::rest::usage`). A call may therefore be dropped at any `.await`: an
/// implementation must only read, and must hold nothing that a drop leaves
/// half done (RS-43).
#[async_trait]
pub trait SkuUsageV1: Send + Sync + 'static {
    /// The usage of each distinct id of `sku_ids` in `tenant`, once, in the
    /// order first asked. An unknown id, a SKU of another tenant and a bundle
    /// SKU (it has no entry) answer zeros. The tenant argument narrows the
    /// read; it never grants access.
    ///
    /// # Errors
    ///
    /// [`sku_usage_denied`] (403) for a caller without pricing
    /// `price_book_entry:read`; [`sku_usage_unavailable`] (503) when the
    /// answer cannot be read. **Neither is a page of zeros.**
    ///
    /// The call may be aborted at any `.await` (see the trait): it reads only.
    async fn usage(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<SkuUsage>, CanonicalError>;

    /// The tenant's priced and in-plan SKUs (P-D-212), under the same rule as
    /// [`Self::usage`] and read set-based: the same number of statements
    /// whatever the number of SKUs. The tenant argument narrows the read; it
    /// never grants access.
    ///
    /// # Errors
    ///
    /// As [`Self::usage`]: [`sku_usage_denied`] (403) for a caller without
    /// pricing `price_book_entry:read`, [`sku_usage_unavailable`] (503) when the
    /// sets cannot be read. **Neither is an empty set.**
    ///
    /// The call may be aborted at any `.await` (see the trait): it reads only.
    async fn usage_sets(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
    ) -> Result<SkuUsageSets, CanonicalError>;

    /// The distinct SKU ids of `scope` in `tenant`, sorted (P-D-246): a
    /// [`UsageScope::Book`]'s SKUs with an entry in the book, in any reference
    /// state, under pricing `price_book_entry:read`; a [`UsageScope::Revision`]'s
    /// SKUs its items name, under pricing `price_book_entry:read` AND
    /// `plan:read`, because a revision's SKUs are plan content. One statement per
    /// call, whatever the number of SKUs.
    ///
    /// A book or a revision the tenant does not hold — unknown, another
    /// tenant's, or outside the caller's scope — answers the empty set, as one
    /// that names no SKU does: the answer is no existence oracle. The tenant
    /// argument narrows the read; it never grants access.
    ///
    /// # Errors
    ///
    /// [`sku_usage_denied`] (403) for a caller without a grant the scope takes;
    /// [`sku_usage_unavailable`] (503) when the set cannot be read. **Neither is
    /// an empty set.**
    ///
    /// The call may be aborted at any `.await` (see the trait): it reads only.
    async fn sku_ids_in(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        scope: UsageScope,
    ) -> Result<Vec<Uuid>, CanonicalError>;
}
