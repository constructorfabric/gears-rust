//! The retention sweeper: the leader-only task that reclaims what the hot path
//! left behind.
//!
//! Idempotency records and operation-log rows both outlive their operation on
//! purpose, so a retry can replay and an operator can audit. Both are bounded
//! by configuration, and this task deletes what has passed its bound.
//!
//! Deletion is batched and the loop yields to cancellation between batches, so
//! a shutdown never waits for a backlog and never leaves a half-deleted batch:
//! each batch is its own statement.

use std::sync::Arc;

use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use crate::domain::ports::metrics::{QeMetrics, RetentionTable};

const LOG_TARGET: &str = "qe.retention";

/// What one sweep reclaimed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Idempotency records deleted.
    pub idempotency: u64,
    /// Operation-log rows deleted.
    pub operation_log: u64,
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
pub struct RetentionSweeper {
    storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
    metrics: Arc<dyn QeMetrics>,
    timing: RetentionTiming,
}

impl RetentionSweeper {
    /// Bind the sweeper to its storage and telemetry.
    #[must_use]
    pub fn new(
        storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
        metrics: Arc<dyn QeMetrics>,
        timing: RetentionTiming,
    ) -> Self {
        Self {
            storage,
            metrics,
            timing,
        }
    }

    /// Reclaim both tables once, in batches, stopping between batches when
    /// `cancel` fires.
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
        let before = now - self.timing.operation_log_retention;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-oplog
        let operation_log = self
            .drain(cancel, RetentionTable::OperationLog, || {
                self.storage
                    .reclaim_operation_log(self.timing.batch_size.get(), before)
            })
            .await;
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-oplog
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-return
        SweepReport {
            idempotency,
            operation_log,
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-return
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
    /// so a fresh leader reclaims without waiting out an interval.
    pub async fn run(&self, cancel: CancellationToken) {
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
                            "retention sweep reclaimed rows"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retention_tests.rs"]
mod tests;
