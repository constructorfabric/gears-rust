//! The **frozen** event-name set.
//!
//! Frozen means what it says: the names below are the contract, and a consumer
//! subscribing to `PriceWindowActivated` is entitled to keep receiving it under
//! that name forever. Adding a name is a contract change; renaming one is a
//! break, which is why [`CatalogEvent::as_str`] is asserted against literal
//! strings in the tests rather than derived from the variant identifiers — a
//! refactor that renames a variant must not silently rename a wire event.
//!
//! Delivery properties, all normative (`design/01-foundation.md`):
//!
//! - Emitted from a **transactional outbox** — an event exists if and only if
//!   its commit happened.
//! - **Ordered per `(tenantId, aggregateId)`**. Not globally ordered: a global
//!   order would serialize every tenant's publishing behind one sequence, and
//!   no consumer needs cross-aggregate order.
//! - **At-least-once**, carrying correlation and idempotency keys, so a
//!   consumer dedups rather than assuming exactly-once.
//!
//! There is **no deletion event**, and its absence is a design property rather
//! than an omission: published rows are never deleted. A row leaves service by
//! being superseded or by its plan being retired ([`CatalogEvent::PlanRetired`])
//! and stays readable as history — so a consumer never has to reconcile a
//! disappearance.

use std::fmt;

use toolkit_macros::domain_model;

/// A frozen catalog event name.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogEvent {
    /// A plan aggregate was created.
    ///
    /// Produced by `plan_repo::create_draft_on` (`inst-pa-return`), which is
    /// where an authored plan and a cloned one meet — so both announce, and
    /// neither door can announce without the other.
    PlanCreated,
    /// A plan's draft content changed — one event per authoring mutation
    /// (S2 §7, S10 §7, PRD AC #98 "emitted after mutation").
    ///
    /// Produced by `plan_repo::record_revision_mutation`, the rail every facet
    /// write already passes through. Discarding a draft reaches that rail too
    /// and is deliberately silent: D-145 makes it a terminal lifecycle flip
    /// rather than an edit, and the set below has no name for it.
    PlanUpdated,
    /// A plan publish committed, carrying a **pending** version ref.
    PlanPublished,
    /// A plan was retired — its own publish unit (D-128).
    PlanRetired,
    /// A plan migration was scheduled.
    PlanMigrationScheduled,
    /// Post-commit read-model warming did not complete inside the SLO. The
    /// re-drive continues past it; completion is observed via the warm marker,
    /// which is why there is no matching "recovered" name.
    PlanPublishDegraded,
    /// A bundle's composition or rev-share changed.
    BundleUpdated,
    /// A price row was created.
    PriceCreated,
    /// A price row was superseded by a successor on its canonical scope key.
    PriceUpdated,
    /// A `PriceWindow` was scheduled.
    PriceWindowScheduled,
    /// A `PriceWindow` became effective.
    PriceWindowActivated,
    /// A `PriceWindow` reached its `effectiveTo`.
    PriceWindowExpired,
    /// A scheduled `PriceWindow` was cancelled before activation.
    PriceWindowCancelled,
    /// An overlay revision published, and with it the `overlay_index` shard it
    /// sits in (D-248). The fourteenth name, and the only one whose
    /// aggregate is **not** a plan: an overlay is tenant-scoped and may target no
    /// plan at all, so there is no plan stream to order it within.
    PriceOverlayPublished,
}

impl CatalogEvent {
    /// Every event of the frozen set, stable order.
    pub const ALL: &'static [Self] = &[
        Self::PlanCreated,
        Self::PlanUpdated,
        Self::PlanPublished,
        Self::PlanRetired,
        Self::PlanMigrationScheduled,
        Self::PlanPublishDegraded,
        Self::BundleUpdated,
        Self::PriceCreated,
        Self::PriceUpdated,
        Self::PriceWindowScheduled,
        Self::PriceWindowActivated,
        Self::PriceWindowExpired,
        Self::PriceWindowCancelled,
        Self::PriceOverlayPublished,
    ];

    /// The wire event name. Frozen.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlanCreated => "PlanCreated",
            Self::PlanUpdated => "PlanUpdated",
            Self::PlanPublished => "PlanPublished",
            Self::PlanRetired => "PlanRetired",
            Self::PlanMigrationScheduled => "PlanMigrationScheduled",
            Self::PlanPublishDegraded => "PlanPublishDegraded",
            Self::BundleUpdated => "BundleUpdated",
            Self::PriceCreated => "PriceCreated",
            Self::PriceUpdated => "PriceUpdated",
            Self::PriceWindowScheduled => "PriceWindowScheduled",
            Self::PriceWindowActivated => "PriceWindowActivated",
            Self::PriceWindowExpired => "PriceWindowExpired",
            Self::PriceWindowCancelled => "PriceWindowCancelled",
            Self::PriceOverlayPublished => "PriceOverlayPublished",
        }
    }
}

impl fmt::Display for CatalogEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
