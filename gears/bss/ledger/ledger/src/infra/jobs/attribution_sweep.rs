//! `AttributionSweepJob` — the `payer-attribution-drift` detective seam
//! (design §4.7).
//!
//! ## What the design wants
//!
//! Every posted journal line carries three tenant attributions: the
//! `payer_tenant_id` (who pays), the `seller_tenant_id` (who sells), and the
//! `resource_tenant_id` (which tenant the consumed resource belongs to). The
//! design's §4.7 `payer-attribution-drift` row is a **detective** control: a
//! background sweep resolves each entry's `resource_tenant_id` up to its
//! nearest `self_managed` ancestor (the tenant that actually owns the billing
//! relationship) and compares that resolved payer to the line's recorded
//! `payer_tenant_id`. A mismatch is *drift* — the books attribute the charge to
//! a payer that the tenant hierarchy no longer agrees with — and is reported as
//! a [`AlarmCategory::PayerAttributionDrift`] alarm.
//!
//! This is the detective complement to the preventive checks on the posting
//! hot path: posting validates payer *consistency within an entry*
//! (`MixedPayer`/`MissingPayer`), but it cannot, on its own, know whether the
//! recorded payer still matches the live tenant tree — that needs the resolver.
//!
//! ## Why this is a no-op seam in the MVP
//!
//! Resolving `resource_tenant_id` → nearest `self_managed` ancestor requires
//! the `TenantResolverClient` (the AuthZ/Tenant subtree resolver). That client
//! is **not wired into this gear** — the same hermetic-test constraint that
//! kept the Slice 2C subtree work resolver-free: the gear's tests must run
//! without standing up the AuthZ/Tenant service. Without the resolver there is
//! nothing to compare against, so [`AttributionSweepJob::run`] is a documented
//! no-op: it logs (at debug) that attribution-drift detection is inactive
//! pending the resolver, and returns `Ok(())`.
//!
//! ## The drop-in future
//!
//! When the resolver lands, `run()` gains: (1) a cross-tenant enumeration of
//! posted lines under the all-tenants system scope (mirroring
//! [`crate::infra::jobs::tieout`] / [`crate::infra::jobs::verifier`]); (2) per
//! line, `resolver.nearest_self_managed_ancestor(resource_tenant_id)` compared
//! to the recorded `payer_tenant_id`; and (3) on mismatch, a fire-and-forget
//! [`AlarmCategory::PayerAttributionDrift`] alarm via `self.publisher`
//! (severity/route come from [`crate::infra::events::alarm_catalog`]). A
//! `serve()` ticker is deliberately NOT wired here — it is added together with
//! the resolver, so the gear never schedules a job that can only no-op.

use std::sync::Arc;

use toolkit_db::{DBProvider, DbError};

use crate::infra::events::publisher::LedgerEventPublisher;

/// Failure of an attribution sweep. Mirrors the chain Verifier's error shape
/// ([`crate::infra::jobs::verifier::VerifyError`]): raised ONLY on an
/// infrastructure fault (DB unreachable / read failure). Detected drift will be
/// reported via an alarm, never as `Err` — same as tie-out and the Verifier.
///
/// In the MVP no-op seam no variant is constructed yet; it exists so the
/// resolver-wired future slots in without changing the public signature.
#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    /// Storage / connection failure (driver text bounded by the caller).
    #[error("attribution-sweep db error: {0}")]
    Db(String),
}

/// The `payer-attribution-drift` detective sweep (§4.7).
///
/// Holds the wiring the resolver-backed future needs — a database provider for
/// the cross-tenant line enumeration and the event publisher for the
/// out-of-band drift alarm — even though the MVP `run()` touches neither.
pub struct AttributionSweepJob {
    /// Database provider for the (future) cross-tenant line enumeration.
    db: DBProvider<DbError>,
    /// Publisher for the (future) out-of-band `PayerAttributionDrift` alarm.
    publisher: Arc<LedgerEventPublisher>,
}

impl AttributionSweepJob {
    /// Build the job over one database provider and the event publisher.
    #[must_use]
    pub fn new(db: DBProvider<DbError>, publisher: Arc<LedgerEventPublisher>) -> Self {
        Self { db, publisher }
    }

    /// Run one attribution-drift sweep.
    ///
    /// **MVP: documented no-op.** Resolving each entry's `resource_tenant_id`
    /// to its nearest `self_managed` ancestor needs the `TenantResolverClient`,
    /// which is not wired into this gear (hermetic-test constraint). With no
    /// resolver there is nothing to compare, so this logs that detection is
    /// inactive and returns `Ok(())`. The resolver wiring, the per-entry
    /// `resource_tenant` → `payer_tenant` comparison, and the
    /// [`crate::infra::events::payloads::AlarmCategory::PayerAttributionDrift`]
    /// emission are the drop-in future (see the module docs).
    ///
    /// # Errors
    /// Never `Err` in the MVP seam. The resolver-wired future returns
    /// [`SweepError::Db`] on an infrastructure read failure; detected drift is
    /// reported via an alarm, not as `Err`.
    #[allow(
        clippy::unused_async,
        reason = "async kept to match the resolver-wired future + the other jobs' run() shape"
    )]
    pub async fn run(&self) -> Result<(), SweepError> {
        // Touch the held wiring so it is not dead-code in the MVP seam; the
        // resolver-wired future enumerates lines via `self.db` and alarms via
        // `self.publisher`.
        let _ = (&self.db, &self.publisher);
        tracing::debug!(
            "bss-ledger: attribution-drift sweep is inactive pending the AuthZ/Tenant resolver \
             (§4.7 payer-attribution-drift detective seam); no-op"
        );
        Ok(())
    }
}
