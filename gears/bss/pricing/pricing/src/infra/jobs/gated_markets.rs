//! The gated-market gauge's refresher — D-246's missing half, on D-250's tick.
//!
//! # Why a job at all, and why not the warm sweep
//!
//! `pricing_tax_not_sellable_ga` is **catalog-wide**: "how many markets are gated
//! now". Every other path in this gear is per-plan, and D-246 records what that
//! cost — an un-dimensioned series written per pre-check is last-writer-wins across
//! every plan and tenant, so a tax-exclusive plan checked after one gating five
//! markets wrote `0` and cleared §7's alarm while those five stayed gated. That was
//! removed as a `[C]`.
//!
//! The instrument is therefore an **observable gauge**: the exporter asks for the
//! value at collection time and no publish claims to know other plans' state. What
//! the gauge reads is an `AtomicI64`, and this job is the only thing that writes it.
//!
//! A pass inside the read-model warm sweep was **declined** by D-246: the warm
//! path's whole contract is bounded per-tenant work (D-163 clause 2), and a
//! catalog-wide scan on it would make one tenant's backlog everyone's latency. A
//! dedicated task touches neither the warm path nor the publish path; its cost is a
//! second background task and a value up to one tick stale.
//!
//! # Why the callback does not do the read itself
//!
//! It cannot. `opentelemetry` 0.31 types the callback
//! `Box<dyn Fn(&dyn AsyncInstrument<T>) + Send + Sync>` — **synchronous**, with the
//! crate's own doc requiring it to complete in a finite amount of time — so a
//! database read inside it is not expressible, never mind advisable. D-246's decided
//! mechanism was measured before it was built and did not exist as stated; the shape
//! that survives is cache-plus-refresher, and this is the refresher.
//!
//! # Staleness is the trade, and it is stated rather than hidden
//!
//! The value is up to one tick old. D-250 takes the tick from
//! `window_activation_tick_secs`' reasoning: 60s is many orders inside anything that
//! moves this quantity, since a market gated on tax-category readiness moves when an
//! operator declares a category or the future Tax Engine answers — months, by the
//! PRD's risk table — while the read is a catalog-wide scan rather than an index
//! probe.

use std::sync::Arc;

use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};

use crate::domain::error::DomainError;

use crate::config::JobsConfig;
use crate::domain::ports::metrics::PricingMetricsPort;
use crate::infra::storage::repo::price_repo;

/// What one refresh observed, for the caller to log and for tests to assert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatedMarketsReport {
    /// The catalog-wide count this pass published to the gauge.
    pub gated_markets: i64,
}

/// The refresher, over one provider and one metrics sink.
pub struct GatedMarketsJob {
    db: DBProvider<DbError>,
    metrics: Arc<dyn PricingMetricsPort>,
    jobs: JobsConfig,
}

impl GatedMarketsJob {
    /// Build the job over one database provider, the sink and the validated cadences.
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        metrics: Arc<dyn PricingMetricsPort>,
        jobs: JobsConfig,
    ) -> Self {
        Self { db, metrics, jobs }
    }

    /// The tick this job runs on, in seconds (D-250).
    #[must_use]
    pub const fn tick_secs(&self) -> u64 {
        self.jobs.gated_markets_tick_secs
    }

    /// Run one pass: read the catalog-wide count and publish it to the gauge.
    ///
    /// Read under [`AccessScope::allow_all`] because the quantity **is** the whole
    /// catalog's — a per-tenant read would answer a different question, and it is
    /// the question D-246 removed a `[C]` for answering. Nothing is written to the
    /// store, so the narrowing rule `crate::infra::jobs` states for writes does not
    /// arise here.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] over whatever the store refuses. A failed pass publishes nothing and leaves the
    /// previous value standing, which is the safe direction: the gauge going stale
    /// is visible as a flat series, while publishing `0` on a read failure would
    /// clear §7's alarm over markets that are still gated — the same defect in a
    /// different costume.
    pub async fn run_once(&self) -> Result<GatedMarketsReport, DomainError> {
        let conn = self.db.conn().map_err(|e| {
            DomainError::Internal(format!("bss-pricing: gated-market refresh: {e}"))
        })?;
        let gated_markets = price_repo::gated_markets(
            &conn,
            &AccessScope::allow_all(),
            crate::domain::tax_display::TAX_ENGINE_GA,
        )
        .await
        .map_err(|e| DomainError::Internal(format!("bss-pricing: gated-market refresh: {e}")))?;

        self.metrics.tax_not_sellable_ga(gated_markets);

        Ok(GatedMarketsReport { gated_markets })
    }
}

#[cfg(test)]
#[path = "gated_markets_tests.rs"]
mod gated_markets_tests;
