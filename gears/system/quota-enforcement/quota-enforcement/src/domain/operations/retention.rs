//! The retention sweeper: the leader-only task that reclaims what the hot path
//! left behind.
//!
//! Idempotency records and operation-log rows both outlive their operation on
//! purpose, so a retry can replay and an operator can audit. Both are bounded
//! by configuration, and this task deletes what has passed its bound. It also
//! removes deleted charges (ADR-0011), which every operation already treats as
//! unknown, and publishes the held allocation charges past the replay window.
//!
//! Deletion is batched and the loop yields to cancellation between batches, so
//! a shutdown never waits for a backlog and never leaves a half-deleted batch:
//! each batch is its own statement.

use std::collections::BTreeMap;
use std::sync::Arc;

use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use crate::domain::catalog::{CatalogEntry, MetricCatalog, MetricMode};
use crate::domain::ports::metrics::{
    ChargeBacklogSink, HeldCharges, MetricLabel, QeMetrics, RetentionTable,
};

const LOG_TARGET: &str = "qe.retention";

/// What one sweep reclaimed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Idempotency records deleted.
    pub idempotency: u64,
    /// Operation-log rows deleted.
    pub operation_log: u64,
    /// Deleted charges removed.
    pub charges: u64,
}

/// How often the sweeper runs, how much it deletes at once, and how long the
/// operation log is kept.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionTiming {
    /// Interval between sweeps.
    pub interval: std::time::Duration,
    /// Rows per statement. A batch bounds how long one delete holds locks.
    pub batch_size: std::num::NonZeroU32,
    /// How long operation-log rows are kept. Idempotency retention is
    /// per-`(tenant, metric)` configuration the plugin reads itself.
    pub operation_log_retention: time::Duration,
}

/// The leader-only reclamation task.
// @cpt-algo:cpt-cf-quota-enforcement-algo-retention-sweep:p1
// @cpt-state:cpt-cf-quota-enforcement-state-idempotency-record:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-retention-sweeper:p1
#[domain_model]
pub struct RetentionSweeper {
    storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
    metrics: Arc<dyn QeMetrics>,
    catalog: Arc<MetricCatalog>,
    charges: Arc<dyn ChargeBacklogSink>,
    timing: RetentionTiming,
}

impl RetentionSweeper {
    /// Bind the sweeper to its storage and telemetry, the metric labels its
    /// gauges may carry, and the cell they read.
    #[must_use]
    pub fn new(
        storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
        metrics: Arc<dyn QeMetrics>,
        catalog: Arc<MetricCatalog>,
        charges: Arc<dyn ChargeBacklogSink>,
        timing: RetentionTiming,
    ) -> Self {
        Self {
            storage,
            metrics,
            catalog,
            charges,
            timing,
        }
    }

    /// Reclaim every table once, in batches, stopping between batches when
    /// `cancel` fires, then publish the held allocation charges.
    ///
    /// A failure is recorded and ends that table's pass; the next tick retries
    /// it. Reclamation is best effort by design: nothing the hot path needs
    /// depends on it having finished.
    pub async fn sweep_once(&self, cancel: &CancellationToken, now: OffsetDateTime) -> SweepReport {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-idem
        // @cpt-begin:cpt-cf-quota-enforcement-state-idempotency-record:p1:inst-idemst-reclaim
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-window
        let idempotency = self
            .drain(cancel, RetentionTable::Idempotency, || {
                self.storage
                    .reclaim_expired_idempotency(self.timing.batch_size.get(), now)
            })
            .await;
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-window
        // @cpt-end:cpt-cf-quota-enforcement-state-idempotency-record:p1:inst-idemst-reclaim
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-idem
        let before = now.checked_sub(self.timing.operation_log_retention);
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-oplog
        // A cutoff before the calendar starts means no entry is old enough yet.
        let operation_log = match before {
            Some(before) => {
                self.drain(cancel, RetentionTable::OperationLog, || {
                    self.storage
                        .reclaim_operation_log(self.timing.batch_size.get(), before)
                })
                .await
            }
            None => 0,
        };
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-oplog
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge
        let charges = self
            .drain(cancel, RetentionTable::Charges, || {
                self.storage
                    .reclaim_expired_charges(self.timing.batch_size.get(), now)
            })
            .await;
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge-gauge
        // A replica that lost its leadership publishes no sample: the new
        // leader's is the only one.
        if !cancel.is_cancelled() {
            self.publish_held_charges(now).await;
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge-gauge
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-return
        SweepReport {
            idempotency,
            operation_log,
            charges,
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-return
    }

    /// Sample the allocation charges that hold an amount on an active Quota
    /// past their return window, per metric.
    ///
    /// As with the lease backlog, every served quota-gated metric starts at
    /// zero, and a metric with no served catalogue entry has no label and is
    /// left out.
    async fn publish_held_charges(&self, now: OffsetDateTime) {
        match self.storage.read_held_allocation_charge_stats(now).await {
            Ok(stats) => {
                let mut backlog: BTreeMap<MetricLabel, HeldCharges> = self
                    .catalog
                    .served_entries()
                    .iter()
                    .filter(|served| served.mode() == MetricMode::QuotaGated)
                    .map(|served| (served.label().clone(), HeldCharges::default()))
                    .collect();
                self.hold_metrics(stats.iter().map(|row| &row.metric)).await;
                for row in stats {
                    if let Some(CatalogEntry::Served(served)) = self.catalog.peek(&row.metric) {
                        let age = u64::try_from((now - row.oldest_created_at).whole_seconds())
                            .unwrap_or(0);
                        let held = backlog.entry(served.label().clone()).or_default();
                        held.count += row.count;
                        held.oldest_age_secs = held.oldest_age_secs.max(age);
                    }
                }
                self.charges.publish(Some(backlog.into_iter().collect()));
            }
            Err(error) => {
                // A stale sample would hide a lost return; withdraw it.
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "could not read the held allocation charges"
                );
                self.charges.publish(None);
            }
        }
    }

    /// Read the metric types of `metrics` this replica does not hold, so a
    /// metric that only another replica has served is not left out of the
    /// sample. A read that fails leaves them out, as before.
    async fn hold_metrics<'a>(
        &self,
        metrics: impl Iterator<Item = &'a quota_enforcement_sdk::MetricId>,
    ) {
        if let Err(error) = self
            .catalog
            .load(metrics.map(quota_enforcement_sdk::MetricId::as_gts))
            .await
        {
            tracing::warn!(
                target: LOG_TARGET,
                error = %error,
                "could not read the metric types of the held charges; metrics this replica \
                 does not hold are left out of the sample"
            );
        }
    }

    /// Delete batch after batch until one comes back short, which means the
    /// table has nothing more that has expired.
    async fn drain<F, Fut>(
        &self,
        cancel: &CancellationToken,
        table: RetentionTable,
        mut batch: F,
    ) -> u64
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<u64, quota_enforcement_sdk::StorageError>>,
    {
        let mut total = 0;
        loop {
            if cancel.is_cancelled() {
                return total;
            }
            match batch().await {
                Ok(deleted) => {
                    total += deleted;
                    self.metrics.record_retention_reclaimed(table, deleted);
                    if deleted < u64::from(self.timing.batch_size.get()) {
                        return total;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        table = table.as_str(),
                        error = %error,
                        "retention sweep failed; the next tick retries it"
                    );
                    self.metrics.record_retention_failure(table);
                    return total;
                }
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

    /// Sweep on every tick until `cancel` fires. The first tick fires at once,
    /// so a fresh leader reclaims without waiting out an interval. However
    /// the loop ends, the held-charge sample is withdrawn.
    pub async fn run(&self, cancel: CancellationToken) {
        let _withdraw = Withdraw(self.charges.as_ref());
        let mut ticker = tokio::time::interval(self.timing.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticker.tick() => {
                    let report = self.sweep_once(&cancel, OffsetDateTime::now_utc()).await;
                    if report != SweepReport::default() {
                        tracing::debug!(
                            target: LOG_TARGET,
                            idempotency = report.idempotency,
                            operation_log = report.operation_log,
                            charges = report.charges,
                            "retention sweep reclaimed rows"
                        );
                    }
                }
            }
        }
    }
}

/// Withdraws the held-charge sample when the sweep loop ends for any reason.
struct Withdraw<'a>(&'a dyn ChargeBacklogSink);

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.0.publish(None);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retention_tests.rs"]
mod tests;
