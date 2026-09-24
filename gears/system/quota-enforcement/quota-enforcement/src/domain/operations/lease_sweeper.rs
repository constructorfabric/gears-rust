//! The lease sweeper (`LeaseSweeper`): the leader-only task that reconciles
//! expired leases with storage.
//!
//! Correctness never waits for it. A lease is released the moment its TTL
//! passes (I4): readers stop counting its holds and the first writer on a
//! counter returns them. The sweeper only transitions the row to
//! `AutoReleased`, returns whatever no writer has, and emits the one
//! `lease-auto-released` event per lease, all inside the storage transaction.
//! A paused sweeper therefore defers events and row growth, not accounting,
//! and the `lease_unreclaimed_expired` gauge it publishes is how an operator
//! sees that happening.

use std::collections::BTreeMap;
use std::sync::Arc;

use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use crate::domain::catalog::MetricClassifications;
use crate::domain::ports::metrics::{LeaseBacklogSink, MetricLabel};

const LOG_TARGET: &str = "qe.lease-sweeper";

/// How often the sweeper runs and how many leases one transaction reclaims.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseSweepTiming {
    /// Interval between sweeps.
    pub interval: std::time::Duration,
    /// Leases per reclamation transaction.
    pub batch_size: std::num::NonZeroU32,
}

/// What one sweep did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseSweepReport {
    /// Leases transitioned to `AutoReleased`.
    pub reclaimed: u64,
    /// Whether a reclamation batch failed; the next tick retries it.
    pub failed: bool,
}

/// The leader-only lease reclamation task.
// @cpt-algo:cpt-cf-quota-enforcement-algo-lease-sweep:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lease-sweeper:p1
pub struct LeaseSweeper {
    storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
    classifications: Arc<MetricClassifications>,
    backlog: Arc<dyn LeaseBacklogSink>,
    timing: LeaseSweepTiming,
}

impl LeaseSweeper {
    /// Bind the sweeper to its storage, the metric labels its gauge may
    /// carry, and the cell the gauge reads.
    #[must_use]
    pub fn new(
        storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
        classifications: Arc<MetricClassifications>,
        backlog: Arc<dyn LeaseBacklogSink>,
        timing: LeaseSweepTiming,
    ) -> Self {
        Self {
            storage,
            classifications,
            backlog,
            timing,
        }
    }

    /// Reclaim expired leases batch by batch until one comes back short,
    /// stopping between batches when `cancel` fires, then publish what is
    /// still expired and unreclaimed.
    pub async fn sweep_once(
        &self,
        cancel: &CancellationToken,
        now: OffsetDateTime,
    ) -> LeaseSweepReport {
        let mut report = LeaseSweepReport::default();
        let batch = self.timing.batch_size.get();
        loop {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-lost
            if cancel.is_cancelled() {
                return report;
            }
            // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-lost
            match self.storage.reclaim_expired_leases(batch, now).await {
                Ok(expired) => {
                    let reclaimed = u64::try_from(expired.len()).unwrap_or(u64::MAX);
                    report.reclaimed += reclaimed;
                    if reclaimed < u64::from(batch) {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        error = %error,
                        "lease reclamation failed; the next tick retries it"
                    );
                    report.failed = true;
                    break;
                }
            }
        }
        // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-gauge
        self.publish_backlog(now).await;
        // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-gauge
        // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-return
        report
        // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-return
    }

    /// Sample the expired-but-unreclaimed backlog into the gauge cell.
    ///
    /// Storage names only the metrics that have a backlog, so every
    /// quota-gated metric starts at zero: a healthy empty backlog is a zero,
    /// told apart from missing telemetry, which is no data point at all. A
    /// metric outside the bootstrap snapshot has no label and is left out,
    /// which keeps the label set closed.
    async fn publish_backlog(&self, now: OffsetDateTime) {
        match self.storage.count_expired_unreclaimed_leases(now).await {
            Ok(counts) => {
                let mut backlog: BTreeMap<MetricLabel, u64> = self
                    .classifications
                    .quota_gated_labels()
                    .map(|label| (label.clone(), 0))
                    .collect();
                for (metric, count) in counts {
                    if let Some(label) = self.classifications.label(&metric) {
                        *backlog.entry(label).or_default() += count;
                    }
                }
                self.backlog.publish(Some(backlog.into_iter().collect()));
            }
            Err(error) => {
                // A stale sample would under-report an outage; withdraw it.
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "could not count the unreclaimed lease backlog"
                );
                self.backlog.publish(None);
            }
        }
    }

    /// The sweeper as leader work, run only by the elected replica.
    #[must_use]
    pub fn leader_work(self: Arc<Self>) -> crate::domain::ports::coordination::LeaderWork {
        Arc::new(
            move |cancel: CancellationToken| -> crate::domain::ports::coordination::LeaderWorkFuture {
                let this = Arc::clone(&self);
                Box::pin(async move { this.run(cancel).await })
            },
        )
    }

    /// Sweep on every tick until `cancel` fires. However the loop ends —
    /// cancelled, or aborted because a stalled sweep overran its stop budget —
    /// the gauge sample is withdrawn: a replica that no longer leads has
    /// nothing to report. The first tick fires at once, so a fresh leader
    /// reclaims without waiting.
    pub async fn run(&self, cancel: CancellationToken) {
        let _withdraw = Withdraw(self.backlog.as_ref());
        // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-interval
        let mut ticker = tokio::time::interval(self.timing.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-interval
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let report = self.sweep_once(&cancel, OffsetDateTime::now_utc()).await;
                    if report.reclaimed > 0 {
                        tracing::debug!(
                            target: LOG_TARGET,
                            reclaimed = report.reclaimed,
                            "lease sweep reclaimed expired leases"
                        );
                    }
                }
            }
        }
    }
}

/// Withdraws the backlog sample when the sweep loop ends for any reason:
/// return, cancellation, or a drop of the future by an abort.
struct Withdraw<'a>(&'a dyn LeaseBacklogSink);

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.0.publish(None);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lease_sweeper_tests.rs"]
mod tests;
