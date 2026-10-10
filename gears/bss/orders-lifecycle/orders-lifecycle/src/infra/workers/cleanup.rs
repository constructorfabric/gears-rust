//! The idempotency-window cleanup worker (01 §3.1 item 6; DESIGN §3.8 roster) and the bounded
//! D-188/D-198 recovery continuation it hosts (no sixth worker).
//!
//! Every discovered expired marker is swept in its own transaction by a conditional delete
//! that rechecks the exact generation, window and lease against fresh database time on the
//! current row version ([`sweep_expired_marker`]); a live, reclaimed or replaced generation
//! and a marker whose execution is unresolved are never deleted. Lapsed unresolved executions
//! are handed, one at a time and with no SQL lock held, to the recovery port **before** the
//! marker sweep (D-188: "expired unresolved attempts are recovered/terminated before marker
//! cleanup, never silently purged"), and the backlog and oldest age are reported either way.
//!
//! Both discoveries are keyset-paged across passes through [`CleanupCursor`]: a pass continues
//! behind the last candidate the previous pass visited and restarts from the oldest once a
//! page comes back short. Markers the sweep must preserve and executions the continuation
//! cannot progress therefore cannot occupy every slot of every bounded pass and starve the
//! deletable or recoverable work behind them.
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_db::Db;

use super::metrics::{CleanupTally, RecoveryTally, WorkerMetrics};
use super::{ExecutionRecovery, RecoveryOutcome, WorkerError, WorkerSettings};
use crate::infra::maintenance::scope::{
    DiscoveredExecution, DiscoveredIdempotency, ExecutionCursor, MarkerCursor, discover_clock,
    discover_expired_executions, discover_expired_idempotency_after, discover_idempotency_backlog,
};
use crate::infra::maintenance::{TargetScope, TaskGrant};
use crate::infra::storage::repo::private::{SweepOutcome, sweep_expired_marker};

/// Where the cleanup worker's bounded passes stand in the expired-marker and lapsed-execution
/// orders. Replica-local; another replica taking the key starts from the oldest.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CleanupCursor {
    /// The last expired marker visited; `None` restarts from the oldest expiry.
    pub markers: Option<MarkerCursor>,
    /// The last lapsed execution handed to recovery; `None` restarts from the oldest lease.
    pub executions: Option<ExecutionCursor>,
}

/// The pass outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    pub markers: CleanupTally,
    /// Expired markers still present after the pass and the age of the oldest.
    pub backlog: u64,
    pub oldest_expired: Option<Duration>,
    pub recovery: RecoveryTally,
    pub recovery_backlog: u64,
    pub recovery_oldest: Option<Duration>,
}

fn age(now: time::OffsetDateTime, at: Option<time::OffsetDateTime>) -> Option<Duration> {
    at.map(|at| (now - at).try_into().unwrap_or(Duration::ZERO))
}

/// Sweep the discovered expired markers, one transaction each.
async fn sweep_markers(
    db: &Db,
    expired: &[DiscoveredIdempotency],
    cancel: &CancellationToken,
) -> Result<CleanupTally, WorkerError> {
    let mut markers = CleanupTally::default();
    for candidate in expired {
        if cancel.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        let target = TargetScope::from_discovered_idempotency(candidate);
        let swept: Result<SweepOutcome, anyhow::Error> = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { Ok(sweep_expired_marker(tx, &target).await?) })
            })
            .await;
        match swept {
            Ok(SweepOutcome::Deleted) => markers.deleted += 1,
            Ok(SweepOutcome::Live) => markers.live += 1,
            Ok(SweepOutcome::Unresolved) => markers.unresolved += 1,
            Ok(SweepOutcome::Replaced) => markers.replaced += 1,
            Err(error) => {
                markers.failed += 1;
                tracing::warn!(error = %error, "bss-orders-lifecycle: marker sweep failed; retried next pass");
            }
        }
    }
    Ok(markers)
}

/// Hand each lapsed unresolved execution to the recovery port, no SQL lock held.
async fn continue_recovery(
    recovery: &dyn ExecutionRecovery,
    lapsed: &[DiscoveredExecution],
    cancel: &CancellationToken,
) -> Result<RecoveryTally, WorkerError> {
    let mut tally = RecoveryTally::default();
    for candidate in lapsed {
        if cancel.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        match recovery.recover(candidate, cancel).await {
            RecoveryOutcome::Progressed => tally.progressed += 1,
            RecoveryOutcome::Terminal => tally.terminal += 1,
            RecoveryOutcome::Deferred => tally.deferred += 1,
            RecoveryOutcome::Unavailable => tally.unavailable += 1,
        }
    }
    Ok(tally)
}

/// The next keyset position after a page: behind its last row while pages are full, back to
/// the start once a page comes back short (the whole set was visited this cycle).
fn advance<R, C>(page: &[R], limit: u64, key: impl Fn(&R) -> C) -> Option<C> {
    if (page.len() as u64) < limit {
        None
    } else {
        page.last().map(key)
    }
}

/// One bounded pass (baseline 60 s / 500 rows): the recovery continuation of lapsed unresolved
/// executions first, then the marker sweep, each continuing behind `cursor`.
///
/// # Errors
/// Ungranted task, discovery failure or cancellation between rows. A single marker's sweep
/// failure is tallied and the pass continues.
pub async fn run_pass(
    db: &Db,
    grant: &TaskGrant<'_>,
    settings: &WorkerSettings,
    recovery: &dyn ExecutionRecovery,
    metrics: &dyn WorkerMetrics,
    cursor: &mut CleanupCursor,
    cancel: &CancellationToken,
) -> Result<CleanupReport, WorkerError> {
    // D-188/D-198 continuation first: bounded, no SQL lock across the port call, so a
    // recovered attempt's marker can be swept by the same pass and an unresolved one is
    // never reached by the sweep as anything but preserved.
    let now = discover_clock(db, grant).await?;
    let lapsed =
        discover_expired_executions(db, grant, now, cursor.executions, settings.cleanup_batch)
            .await?;
    let recovery_backlog = lapsed.total;
    // Whole-set age, not the page's: the cursor may stand past the oldest execution.
    let recovery_oldest = age(now, lapsed.oldest_lease);
    let tally = continue_recovery(recovery, &lapsed.found, cancel).await?;
    cursor.executions = advance(
        &lapsed.found,
        settings.cleanup_batch,
        DiscoveredExecution::cursor,
    );
    metrics.recovery(recovery_backlog, recovery_oldest, tally);
    if recovery_backlog > 0 {
        tracing::warn!(
            backlog = recovery_backlog,
            oldest_seconds = recovery_oldest.map_or(0, |d| d.as_secs()),
            unavailable = tally.unavailable,
            "bss-orders-lifecycle: lapsed unresolved executions await recovery"
        );
    }

    let now = discover_clock(db, grant).await?;
    let expired =
        discover_expired_idempotency_after(db, grant, now, cursor.markers, settings.cleanup_batch)
            .await?;
    let markers = sweep_markers(db, &expired, cancel).await?;
    cursor.markers = advance(
        &expired,
        settings.cleanup_batch,
        DiscoveredIdempotency::cursor,
    );
    metrics.cleanup(markers);
    let now = discover_clock(db, grant).await?;
    let backlog = discover_idempotency_backlog(db, grant, now).await?;
    let oldest_expired = age(now, backlog.oldest);
    metrics.cleanup_backlog(backlog.rows, oldest_expired);
    Ok(CleanupReport {
        markers,
        backlog: backlog.rows,
        oldest_expired,
        recovery: tally,
        recovery_backlog,
        recovery_oldest,
    })
}
