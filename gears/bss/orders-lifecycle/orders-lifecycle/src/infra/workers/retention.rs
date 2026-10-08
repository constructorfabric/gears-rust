//! The retention-purge worker (`cpt-cf-bss-orders-lifecycle-component-retention-purge`,
//! 01 §3.5, D-185): daily bounded deletion of the three bounded-retention stores under the
//! restricted retention role. It never acquires an aggregate lock, never touches a committed
//! audit row, an idempotency record or a platform outbox row, and repairs nothing.
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use toolkit_db::Db;

use super::metrics::WorkerMetrics;
use super::{WorkerError, WorkerSettings};
use crate::infra::maintenance::scope::{
    discover_clock, discover_retention, discover_retention_backlog,
};
use crate::infra::maintenance::{RetentionTable, TargetScope, TaskGrant};
use crate::infra::storage::repo::retention as purge;

/// The three stores and their windows (01 §3.5 item 2; the migration 07 guard enforces the
/// same windows independently).
pub const WINDOWS: [(RetentionTable, time::Duration); 3] = [
    (RetentionTable::PreviewDiagnostics, time::Duration::days(7)),
    (RetentionTable::RefusedAudit, time::Duration::days(90)),
    (RetentionTable::ReadAccessLog, time::Duration::days(90)),
];

/// One store's pass outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreReport {
    pub store: RetentionTable,
    pub purged: u64,
    pub batches: u32,
    /// Eligible rows still present after the pass.
    pub backlog: u64,
    /// How far past its window the oldest remaining eligible row is.
    pub oldest_overdue: Option<Duration>,
    /// Whether the pass stopped on its batch budget or cancellation with work remaining.
    pub budget_exhausted: bool,
}

/// The pass outcome per store, in `WINDOWS` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub stores: Vec<StoreReport>,
}

async fn delete_batch(
    db: &Db,
    target: TargetScope,
    store: RetentionTable,
) -> Result<u64, WorkerError> {
    let deleted: Result<u64, anyhow::Error> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                Ok(match store {
                    RetentionTable::RefusedAudit => purge::refused_audit(tx, &target).await?,
                    RetentionTable::PreviewDiagnostics => {
                        purge::preview_diagnostics(tx, &target).await?
                    }
                    RetentionTable::ReadAccessLog => purge::access_log(tx, &target).await?,
                })
            })
        })
        .await;
    Ok(deleted?)
}

fn overdue(cutoff: time::OffsetDateTime, oldest: Option<time::OffsetDateTime>) -> Option<Duration> {
    oldest.map(|oldest| (cutoff - oldest).try_into().unwrap_or(Duration::ZERO))
}

/// One pass: per store, bounded batches in deterministic `(time, primary key)` order while the
/// pass budget lasts, each committed in its own transaction, then the remaining backlog and
/// oldest overdue age. Cutoffs come from fresh database time.
///
/// # Errors
/// An ungranted task, a discovery or store failure (the batch rolls back; the next pass
/// retries), or cancellation between batches.
pub async fn run_pass(
    db: &Db,
    grant: &TaskGrant<'_>,
    settings: &WorkerSettings,
    metrics: &dyn WorkerMetrics,
    cancel: &CancellationToken,
) -> Result<RetentionReport, WorkerError> {
    let mut stores = Vec::with_capacity(WINDOWS.len());
    for (store, window) in WINDOWS {
        let mut purged = 0;
        let mut batches = 0;
        let mut budget_exhausted = false;
        loop {
            if cancel.is_cancelled() {
                return Err(WorkerError::Cancelled);
            }
            if batches >= settings.retention_batches_per_pass {
                budget_exhausted = true;
                break;
            }
            let cutoff = discover_clock(db, grant).await? - window;
            let batch =
                discover_retention(db, grant, store, cutoff, settings.retention_batch).await?;
            let target = TargetScope::from_discovered_retention(&batch);
            let Some((ids, _)) = target.retention(store) else {
                return Err(WorkerError::Scope(toolkit_db::secure::ScopeError::Invalid(
                    "retention target of another store",
                )));
            };
            let discovered = u64::try_from(ids.len()).unwrap_or(u64::MAX);
            if discovered == 0 {
                break;
            }
            let started = Instant::now();
            let deleted = delete_batch(db, target, store).await?;
            metrics.retention_batch(store, deleted, started.elapsed());
            purged += deleted;
            batches += 1;
            if discovered < settings.retention_batch {
                break;
            }
        }
        let cutoff = discover_clock(db, grant).await? - window;
        let backlog = discover_retention_backlog(db, grant, store, cutoff).await?;
        let oldest_overdue = overdue(cutoff, backlog.oldest);
        metrics.retention_backlog(store, backlog.rows, oldest_overdue);
        if backlog.rows > 0 {
            tracing::warn!(
                store = ?store,
                backlog = backlog.rows,
                oldest_overdue_seconds = oldest_overdue.map_or(0, |d| d.as_secs()),
                "bss-orders-lifecycle: retention backlog remains after the pass"
            );
        }
        stores.push(StoreReport {
            store,
            purged,
            batches,
            backlog: backlog.rows,
            oldest_overdue,
            budget_exhausted,
        });
    }
    Ok(RetentionReport { stores })
}
