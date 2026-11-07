//! The periodic passes of the managed lifecycle and the bounded runs of them
//! the maintenance routes make.
//!
//! One implementation serves both. A replica's loop calls a pass on its tick;
//! an external scheduler asks for one pass through a route when the loops are
//! switched off (`background` in the gear's configuration), so the work runs
//! once per cluster instead of once per replica.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_db::{DBProvider, DbError};
use tracing::info;

use crate::domain::error::DomainError;
use crate::domain::ports::LifecycleMetrics;
use crate::infra::value_writes::{SWEEP_LIMIT, SweepPage, WriteCoordinator};
use crate::log_text::LogSafe;

/// How the retention pass takes a backlog: batches of `size`, at most
/// `max_batches` of them a pass, each its own short statement and commit.
#[derive(Debug, Clone, Copy)]
pub struct PruneBatches {
    /// Records a batch deletes at most.
    pub size: u64,
    /// Batches a pass runs at most.
    pub max_batches: u32,
}

/// Ten thousand a batch and a hundred batches a tick: a million records, about
/// seven days' worth at the declared bound of fifty million a year, so a
/// backlog after a long outage clears in days while no statement runs long.
pub const TICK_BATCHES: PruneBatches = PruneBatches {
    size: 10_000,
    max_batches: 100,
};

/// The budget of one maintenance request: a tenth of a tick's, so a request
/// stays short. A scheduler that gets `more` back calls again.
pub const REQUEST_BATCHES: PruneBatches = PruneBatches {
    size: 10_000,
    max_batches: 10,
};

/// What one retention pass did.
#[derive(Debug)]
pub struct PruneRun {
    /// Records deleted, including those of batches before a failure.
    pub pruned: u64,
    /// Whether the budget ran out, or the pass was stopped or failed, with
    /// records possibly still past their horizon.
    pub more: bool,
    /// The failure that stopped the pass early, for the caller to log or to
    /// answer with.
    pub failure: Option<DomainError>,
}

/// One refresh of the needs-review gauge, logged and never fatal. Every
/// source is published, zero included, so a fixed backlog reads zero
/// rather than its last count; a failed pass leaves the gauge as it was
/// until the next tick.
pub async fn review_once(db: &DBProvider<DbError>, metrics: &dyn LifecycleMetrics) {
    match count_needs_review(db).await {
        Ok(counts) => {
            for (source, count) in counts {
                metrics.needs_review(source, count);
            }
        }
        Err(err) => tracing::warn!(
            err = %LogSafe(&err),
            "needs-review gauge refresh failed; retried next tick"
        ),
    }
}

/// The flagged overrides per declaration source, across every tenant.
async fn count_needs_review(
    db: &DBProvider<DbError>,
) -> Result<Vec<(&'static str, u64)>, DomainError> {
    use crate::domain::value::ValueRepository as _;
    let conn = db
        .conn()
        .map_err(|err| DomainError::dependency_unavailable("database", "open a connection", err))?;
    let scope = toolkit_security::AccessScope::allow_all();
    let mut counts = Vec::with_capacity(crate::domain::declaration::SOURCES.len());
    for source in crate::domain::declaration::SOURCES {
        let count = crate::infra::storage::value_repo::ValueRepo
            .count_flagged(&conn, &scope, source)
            .await?;
        counts.push((source, count));
    }
    Ok(counts)
}

/// The daily tick: one retention pass against the clock now.
pub async fn retention_tick(
    db: &DBProvider<DbError>,
    retention: Duration,
    cancel: &CancellationToken,
    metrics: &dyn LifecycleMetrics,
) {
    prune_once(
        db,
        retention,
        time::OffsetDateTime::now_utc(),
        TICK_BATCHES,
        cancel,
        metrics,
    )
    .await;
}

/// One audit retention pass, logged and never fatal: records past their
/// horizon leave, an explicit `retain_until` or `occurred_at` plus the
/// configured default, batch by batch, until a batch comes back short,
/// the pass's cap is reached or the lifecycle is stopped. What the pass
/// did not reach, or a failed batch left, waits for the next pass; what
/// earlier batches deleted stays deleted. Returns how many records went.
pub async fn prune_once(
    db: &DBProvider<DbError>,
    default_retention: Duration,
    now: time::OffsetDateTime,
    batches: PruneBatches,
    cancel: &CancellationToken,
    metrics: &dyn LifecycleMetrics,
) -> u64 {
    prune_pass(db, default_retention, now, batches, cancel, metrics)
        .await
        .pruned
}

/// [`prune_once`] with the whole outcome, failure included, for a caller that
/// answers with it instead of only logging it.
pub async fn prune_pass(
    db: &DBProvider<DbError>,
    default_retention: Duration,
    now: time::OffsetDateTime,
    batches: PruneBatches,
    cancel: &CancellationToken,
    metrics: &dyn LifecycleMetrics,
) -> PruneRun {
    let mut run = prune(db, default_retention, now, batches, cancel).await;
    // Reported whatever happened: a failed pass and one with nothing to
    // prune both return zero, and only this tells them apart.
    metrics.retention_pass(
        if run.failure.is_some() {
            "failed"
        } else {
            "ok"
        },
        run.pruned,
    );
    if run.pruned > 0 {
        info!(
            pruned = run.pruned,
            "audit records past their retention horizon pruned"
        );
    }
    if let Some(err) = &run.failure {
        tracing::warn!(
            pruned = run.pruned,
            err = %LogSafe(err),
            "audit retention pass failed; the rest is retried by the next pass"
        );
        run.more = true;
    }
    run
}

/// The retention pass itself.
async fn prune(
    db: &DBProvider<DbError>,
    default_retention: Duration,
    now: time::OffsetDateTime,
    batches: PruneBatches,
    cancel: &CancellationToken,
) -> PruneRun {
    let failed = |pruned: u64, failure: DomainError| PruneRun {
        pruned,
        more: true,
        failure: Some(failure),
    };
    let Ok(retention) = time::Duration::try_from(default_retention) else {
        return failed(
            0,
            DomainError::Internal {
                diagnostic: "the audit retention does not fit a timestamp span".to_owned(),
            },
        );
    };
    let conn = match db.conn() {
        Ok(conn) => conn,
        Err(err) => {
            return failed(
                0,
                DomainError::dependency_unavailable("database", "open a connection", err),
            );
        }
    };
    let scope = toolkit_security::AccessScope::allow_all();
    // The configured retention first, where the trigger reads it: the
    // database then refuses deleting anything younger by any path, and a
    // write that fails fails the pass, which the pass metric reports.
    let days = u32::try_from(default_retention.as_secs().div_euclid(86_400)).unwrap_or(u32::MAX);
    if let Err(err) = crate::infra::storage::audit_store::AuditStore
        .record_retention(&conn, &scope, days)
        .await
    {
        return failed(0, err);
    }
    let mut pruned = 0;
    // The budget is spent unless a short batch shows nothing is left.
    let mut more = true;
    for _ in 0..batches.max_batches {
        if cancel.is_cancelled() {
            break;
        }
        match crate::infra::storage::audit_store::AuditStore
            .prune_expired(&conn, &scope, now, retention, batches.size)
            .await
        {
            Ok(batch) => {
                pruned += batch;
                if batch < batches.size {
                    more = false;
                    break;
                }
            }
            Err(err) => return failed(pruned, err),
        }
    }
    PruneRun {
        pruned,
        more,
        failure: None,
    }
}

/// One pass of the sweep, logged and never fatal: what it could not
/// release waits for the next pass.
pub async fn sweep_once(writes: &WriteCoordinator, cancel: &CancellationToken) {
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
    match writes.sweep_page(SWEEP_LIMIT, cancel).await {
        Ok(SweepPage { released: 0, .. }) => {}
        Ok(SweepPage { released, .. }) => info!(released, "expired staged secrets released"),
        Err(err) => tracing::warn!(
            err = %LogSafe(&err),
            "pending-secret sweep failed; retried next tick"
        ),
    }
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
}
