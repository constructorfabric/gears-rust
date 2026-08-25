//! `pricingSnapshotRef` — the composite reference consumers pin.
//!
//! A snapshot ref is the catalog-side identifiers sufficient for the manifest's
//! `pricingSnapshotRef`: the version ref, the resolved price ids, and the
//! evaluation-policy version. It is stamped at publish with a **pending**
//! version ref and finalized to the committed `CatalogVersion` when
//! `CatalogVersionPublished` fires — **immutable thereafter**.
//!
//! Two properties are normative rather than convenient
//! (`design/01-foundation.md` §4.4):
//!
//! - **The normative composition system of record is Tariffs.** What this gear stamps is the
//!   *aligned entry*, and it MUST NOT diverge from the Tariffs composition. A
//!   second, subtly different composition here would not be a duplicate — it
//!   would be a second answer to "what was this subscriber priced from".
//! - **Posted invoice periods never re-query mutable catalog rows.** They
//!   resolve through the pin, which is why the finalize step is one-way: a ref
//!   that could be re-pointed would let a later publish change what an already
//!   posted period was priced from.
//!
//! # [`PricingSnapshotRef`] is the catalog-side model, and it is not on any live
//! path
//!
//! Stated here because the type reads as live — it is public, it has a
//! constructor, three accessors and a finalizer, and the two normative properties
//! above are asserted **on it**, in `snapshot_tests`. A reader meeting it would
//! reasonably conclude that the composition and the one-way pin have their home
//! in this file. They do not, and the two facts a reader needs are:
//!
//! - **Its only producer is
//!   [`PublishReceipt::snapshot_ref`](crate::domain::publish::PublishReceipt::snapshot_ref),
//!   which nothing outside `domain::publish_tests` calls.** The composition that
//!   actually reaches consumers is built beside this type, as the three keys
//!   `pendingVersionRef` / `priceIds` / `evaluationPolicyVersion` of
//!   [`outbox_repo`](crate::infra::storage::repo::outbox_repo)'s `PlanPublished`
//!   payload, and the 202 body re-lists two of the three
//!   (`api::rest::publish::PublishReceiptView`, which carries no policy version).
//! - **[`PricingSnapshotRef::finalize`] has no caller outside
//!   `snapshot_tests`.** The live one-way pin is a row compare-and-swap on
//!   `pricing_catalog_version_ref`
//!   ([`catalog_version_ref_repo::finalize`](crate::infra::storage::repo::catalog_version_ref_repo::finalize)),
//!   which that module's own doc calls the "storage-side sibling" of
//!   [`VersionRef::finalize`] — "not one mechanism and cannot be: that one moves
//!   a value a caller holds, this one moves a row several sweeps can reach".
//!
//! **The producer is deliberately not built, and the type is deliberately not
//! deleted.** Building one would be a design change: D-30 puts the composition
//! system of record in Tariffs and the catalog "never stamps snapshots" —
//! `09-price-overlays.md` §1.7 and that slice's definition of done, with the
//! single named exception of
//! the `migrated-origin` payload (D-102), which is per-subscription governed
//! history served read-only on its own surface and composes nothing here. What
//! this gear owes is the *identifiers*
//! (`cpt-cf-bss-pricing-fr-pricing-snapshot`), and it stamps them: the pending
//! handle and its subject in `pricing_catalog_version_ref`, the three segments in
//! the published payload. Deleting the type would take with it the only place the
//! aligned entry is one value rather than three keys of a `json!`.
//!
//! **What owes its use is `outbox_repo`'s payload build.** The day the
//! three-segment composition needs one home — a second emitter, or a consumer
//! that reads the entry back — this is the type it should be routed through, and
//! `outbox_repo_tests` is where that adoption would be observed. Until then the
//! wire spelling is guarded there and the invariants are guarded here, and the
//! two are separate spellings of one composition.

use bss_pricing_sdk::CatalogVersion;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The version half of a snapshot ref: pending at publish, committed once the
/// registry has assigned a version.
///
/// Two states rather than an `Option<CatalogVersion>` plus a handle string: the
/// registry batches approved publishes (D-47), so between the publish commit
/// and `CatalogVersionPublished` the ref genuinely *has* an identity — the
/// pending handle — and callers must be able to carry and match it. An
/// `Option` would model that interval as "no version yet" and lose the handle
/// that resolves it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionRef {
    /// The registry's pending handle, stamped at publish.
    Pending(String),
    /// The committed version. Terminal.
    Committed(CatalogVersion),
}

impl VersionRef {
    /// Has this ref been finalized?
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }

    /// The committed version, when there is one.
    #[must_use]
    pub const fn committed(&self) -> Option<CatalogVersion> {
        match self {
            Self::Pending(_) => None,
            Self::Committed(version) => Some(*version),
        }
    }

    /// The pending handle, while the ref is still pending.
    #[must_use]
    pub fn pending_ref(&self) -> Option<&str> {
        match self {
            Self::Pending(handle) => Some(handle),
            Self::Committed(_) => None,
        }
    }

    /// Move `Pending -> Committed`, exactly once.
    ///
    /// Consuming rather than `&mut self` on purpose: the caller cannot keep the
    /// pending ref it just finalized, so there is no path where an old handle
    /// and its committed version are both live.
    ///
    /// # Errors
    ///
    /// [`DomainError::LifecycleForbidden`] when the ref is already committed.
    /// Re-finalizing is not idempotent-and-harmless: a duplicate
    /// `CatalogVersionPublished` carrying a different version would silently
    /// re-point a pin that posted periods already resolved through.
    pub fn finalize(self, version: CatalogVersion) -> Result<Self, DomainError> {
        match self {
            Self::Pending(_) => Ok(Self::Committed(version)),
            Self::Committed(existing) => Err(DomainError::LifecycleForbidden(format!(
                "version ref already committed at {}; refused re-finalize to {}",
                existing.get(),
                version.get()
            ))),
        }
    }
}

/// The composite reference pinned on charges and `BillableItem`s.
///
/// Fields are private and [`PricingSnapshotRef::finalize`] is the only way to
/// move the version ref, so the immutability the design set states is a
/// property of the type rather than a convention every call site has to keep.
///
/// **Not on a live path** — see the module doc: the only producer is
/// [`PublishReceipt::snapshot_ref`](crate::domain::publish::PublishReceipt::snapshot_ref),
/// nothing consumes it outside tests, and the finalizer below has no caller
/// outside `snapshot_tests`. The wire's composition is `outbox_repo`'s, and the
/// live one-way pin is that module's row compare-and-swap.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingSnapshotRef {
    version_ref: VersionRef,
    price_ids: Vec<Uuid>,
    evaluation_policy_version: String,
}

impl PricingSnapshotRef {
    /// Stamp a snapshot ref at publish.
    #[must_use]
    pub fn new(
        version_ref: VersionRef,
        price_ids: Vec<Uuid>,
        evaluation_policy_version: String,
    ) -> Self {
        Self {
            version_ref,
            price_ids,
            evaluation_policy_version,
        }
    }

    /// The version ref.
    #[must_use]
    pub const fn version_ref(&self) -> &VersionRef {
        &self.version_ref
    }

    /// The resolved price ids this snapshot froze.
    #[must_use]
    pub fn price_ids(&self) -> &[Uuid] {
        &self.price_ids
    }

    /// The evaluation-policy version the resolved rows are evaluated under.
    #[must_use]
    pub fn evaluation_policy_version(&self) -> &str {
        &self.evaluation_policy_version
    }

    /// Finalize the version ref on `CatalogVersionPublished`.
    ///
    /// **No caller outside `snapshot_tests`.** The sweep that answers
    /// `CatalogVersionPublished` today finalizes the *row*, through
    /// [`catalog_version_ref_repo::finalize`](crate::infra::storage::repo::catalog_version_ref_repo::finalize),
    /// and never a value of this type. Kept, not deleted: it is the domain-side
    /// statement of the one-way pin, and the storage-side sibling's own doc
    /// argues the two cannot be one mechanism.
    ///
    /// # Errors
    ///
    /// [`DomainError::LifecycleForbidden`] when the ref is already committed;
    /// see [`VersionRef::finalize`].
    pub fn finalize(self, version: CatalogVersion) -> Result<Self, DomainError> {
        Ok(Self {
            version_ref: self.version_ref.finalize(version)?,
            price_ids: self.price_ids,
            evaluation_policy_version: self.evaluation_policy_version,
        })
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;
