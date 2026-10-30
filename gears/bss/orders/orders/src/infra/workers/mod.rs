//! The five Orders-owned workers (DESIGN Foundation contract §3.8; 01 §3.4 item 5): per-state
//! TTL expiry, draft auto-void, idempotency-window cleanup, retention purge and audit
//! verification/checkpointing.
//!
//! Coordination is the toolkit's session advisory lock on the host connection,
//! `Db::try_lock("bss-orders", <key>)` with exactly one non-blocking attempt: a
//! contended pass skips and reschedules, the guard is held for the bounded pass and released
//! explicitly, and lifecycle cancellation stops scheduling and drops the running pass at
//! whatever await it is parked on (every pass body is cancel-safe in that sense: see
//! [`guarded_pass_within`]); the bodies' own `is_cancelled` checks only stop a pass that is
//! between awaits early. Session loss is not fencing: every write stays protected by its own transactional
//! recheck (locked-row eligibility, conditional generation/window delete, conditional bounded
//! deletion, checkpoint sequence uniqueness), so two overlapping passes cannot duplicate an
//! effect. No lease, renewal, fence table or sixth worker exists here.
//!
//! Each worker reaches storage only through its restricted class connection
//! ([`connections::WorkerConnections`]); the expiry/auto-void bodies and the D-188/D-198
//! recovery continuation are ports whose production bodies arrive with Stage 3/5 and are
//! explicitly unavailable until then.
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit_db::{Db, LockConfig};
use uuid::Uuid;

use crate::infra::engine::errors;
use crate::infra::maintenance::scope::{DiscoveredExecution, DiscoveryError};
use crate::infra::maintenance::{MaintenanceAuthority, MaintenanceTask, TaskGrant};
use crate::infra::storage::repo::audit::AuditStoreError;
use toolkit_db::secure::ScopeError;

pub mod audit;
pub mod cleanup;
pub mod connections;
pub mod expiry;
pub mod metrics;
pub mod retention;

pub use connections::{RoleClass, WorkerConnections};
pub use metrics::{PassResult, WorkerMetrics};

/// The gear namespace of every Orders advisory key.
pub const LOCK_NAMESPACE: &str = "bss-orders";

/// Pass deadline of the 60-second-baseline sweep and cleanup workers (D-218): comfortably
/// below the baseline tick and above [`connections::STATEMENT_TIMEOUT`].
pub const MINUTE_PASS_BUDGET: Duration = Duration::from_secs(45);
/// Pass deadline of one audit namespace (D-218): a verification slice plus, once per checkpoint
/// interval, the streamed checkpoint capture.
pub const AUDIT_PASS_BUDGET: Duration = Duration::from_mins(5);
/// Pass deadline of the daily retention purge (D-218): up to 24 batches of 5,000 rows per store.
pub const RETENTION_PASS_BUDGET: Duration = Duration::from_mins(30);

/// The authoritative roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerKind {
    /// Per-state TTL expiry (key `expiry`).
    Expiry,
    /// Draft auto-void (key `draft-auto-void`).
    DraftAutoVoid,
    /// Idempotency-window cleanup and the recovery continuation (key `idempotency-cleanup`).
    IdempotencyCleanup,
    /// Retention purge of the three bounded stores (key `retention-purge`).
    RetentionPurge,
    /// Audit verification and checkpointing (key `audit/<namespace>`).
    Audit,
}
impl WorkerKind {
    /// The advisory key within the gear namespace (the audit worker appends `/<namespace>`).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Expiry => "expiry",
            Self::DraftAutoVoid => "draft-auto-void",
            Self::IdempotencyCleanup => "idempotency-cleanup",
            Self::RetentionPurge => "retention-purge",
            Self::Audit => "audit",
        }
    }
    /// The audit worker's per-namespace key.
    #[must_use]
    pub fn namespace_key(namespace: Uuid) -> String {
        format!("{}/{namespace}", Self::Audit.key())
    }
    /// The tasks a worker needs at least one of to be scheduled.
    #[must_use]
    pub fn tasks(self) -> &'static [MaintenanceTask] {
        match self {
            Self::Expiry => &[MaintenanceTask::StateExpiry],
            Self::DraftAutoVoid => &[MaintenanceTask::DraftAutoVoid],
            Self::IdempotencyCleanup => &[MaintenanceTask::IdempotencyCleanup],
            Self::RetentionPurge => &[MaintenanceTask::RetentionPurge],
            Self::Audit => &[
                MaintenanceTask::AuditVerification,
                MaintenanceTask::AuditCheckpoint,
            ],
        }
    }
    /// The deadline of one pass under the advisory key (D-218). An elapsed pass is a failed
    /// pass: its body is dropped, its open transaction rolls back with it, and the key is
    /// released before the next tick.
    #[must_use]
    pub fn pass_budget(self) -> Duration {
        match self {
            Self::Expiry | Self::DraftAutoVoid | Self::IdempotencyCleanup => MINUTE_PASS_BUDGET,
            Self::Audit => AUDIT_PASS_BUDGET,
            Self::RetentionPurge => RETENTION_PASS_BUDGET,
        }
    }
    const ALL: [Self; 5] = [
        Self::Expiry,
        Self::DraftAutoVoid,
        Self::IdempotencyCleanup,
        Self::RetentionPurge,
        Self::Audit,
    ];
}

/// A pass failure. The next tick retries; nothing is repaired or widened.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The worker's maintenance task is not granted, or its body is not delivered.
    #[error("maintenance task is not granted")]
    NotGranted,
    /// The class connection the task needs is not configured.
    #[error("maintenance connection class `{}` is not configured", .0.config_key())]
    NotConfigured(RoleClass),
    /// Candidate discovery failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// A secure-scope query was rejected or failed.
    #[error(transparent)]
    Scope(#[from] ScopeError),
    /// An audit store read failed.
    #[error(transparent)]
    Audit(#[from] AuditStoreError),
    /// Audit digest or checkpoint encoding failed.
    #[error(transparent)]
    Encoding(#[from] crate::domain::audit::AuditError),
    /// Any other database or store failure.
    #[error("worker store failure: {0}")]
    Store(#[from] anyhow::Error),
    /// Lifecycle cancellation interrupted the pass.
    #[error("pass cancelled")]
    Cancelled,
    /// The pass outlived its deadline (D-218) and was dropped.
    #[error("pass exceeded its {}s deadline", .0.as_secs())]
    DeadlineExceeded(Duration),
}
impl From<toolkit_db::DbError> for WorkerError {
    fn from(error: toolkit_db::DbError) -> Self {
        Self::Store(error.into())
    }
}

/// Validated cadences and batch bounds (baselines from 01 §3.1 item 6, §3.5, D-100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSettings {
    /// Cleanup worker tick period.
    pub cleanup_interval: Duration,
    /// Expired markers and lapsed executions handled per cleanup pass.
    pub cleanup_batch: u64,
    /// Retention worker tick period.
    pub retention_interval: Duration,
    /// Rows deleted per retention batch.
    pub retention_batch: u64,
    /// Retention batches per store per pass.
    pub retention_batches_per_pass: u32,
    /// Minimum age of the last checkpoint before a new capture, and the hold after a mismatch.
    pub checkpoint_interval: Duration,
    /// Audit worker tick period (one verification slice per namespace).
    pub verification_interval: Duration,
    /// Orders verified per namespace per slice.
    pub verification_orders_per_pass: u64,
    /// Expiry and auto-void tick period.
    pub sweep_interval: Duration,
    /// Candidates per expiry or auto-void sweep.
    pub sweep_batch: u64,
}
impl WorkerSettings {
    /// The design baselines: cleanup every 60 s / 500 rows, daily purge batches of 5,000,
    /// daily checkpoints, a verification slice of 500 orders per namespace per minute, and
    /// expiry/auto-void sweeps every 60 s over 500 candidates.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "configuration always supplies explicit settings; the worker tests start from the \
                      design baselines"
        )
    )]
    #[must_use]
    pub fn baseline() -> Self {
        Self {
            cleanup_interval: Duration::from_secs(60),
            cleanup_batch: 500,
            retention_interval: Duration::from_hours(24),
            retention_batch: 5000,
            retention_batches_per_pass: 24,
            checkpoint_interval: Duration::from_hours(24),
            verification_interval: Duration::from_secs(60),
            verification_orders_per_pass: 500,
            sweep_interval: Duration::from_secs(60),
            sweep_batch: 500,
        }
    }
}

/// Outcome of one D-188/D-198 recovery continuation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    dead_code,
    reason = "no recovery body is delivered (D-188/D-198 Stage 5 ports), so only `Unavailable` \
              is constructed; the others are the port's contract with the cleanup tally"
)]
pub enum RecoveryOutcome {
    /// Bounded progress was made; the execution stays unresolved.
    Progressed,
    /// The execution reached a terminal status (committed/settled, refused or abandoned).
    Terminal,
    /// Inputs are unavailable right now; preserved for a later pass.
    Deferred,
    /// No recovery body is delivered for this execution kind: preserved and reported.
    Unavailable,
}

/// The bounded recovery continuation of expired unresolved executions (DESIGN D-188:
/// "Recovery is an engine service invoked by request retries and a bounded continuation in the
/// existing idempotency maintenance worker"; D-198 for controls). The worker calls it without
/// holding any SQL lock; the body owns its own fenced transactions and remote calls.
///
/// # Cancel safety
///
/// `recover` must be cancel-safe: [`guarded_pass_within`] drops it at any await on lifecycle
/// cancellation or when the pass deadline elapses, so `cancel` is only an early-exit hint, never
/// a promise that the body runs to its next check. An implementation must therefore fence its
/// own effects: every durable step is its own transaction guarded by the execution's fencing
/// generation, and a remote command is sent only under an idempotency key derived from the
/// execution, so a drop after the send and before its outcome is recorded leaves the execution
/// unresolved with its lease lapsing; the next pass (or a same-key request retry) re-reads it
/// and resumes from the recorded state, and the cleanup cursor is not advanced past an
/// execution whose step was dropped.
#[async_trait]
pub trait ExecutionRecovery: Send + Sync {
    async fn recover(
        &self,
        candidate: &DiscoveredExecution,
        cancel: &CancellationToken,
    ) -> RecoveryOutcome;
}

/// Until S3 (commercial attempts) and S5 (receiver controls) deliver the bodies, every lapsed
/// unresolved execution is preserved and reported as unavailable, never purged or abandoned.
#[derive(Debug, Default, Clone, Copy)]
pub struct RecoveryUnavailable;
#[async_trait]
impl ExecutionRecovery for RecoveryUnavailable {
    async fn recover(&self, _: &DiscoveredExecution, _: &CancellationToken) -> RecoveryOutcome {
        RecoveryOutcome::Unavailable
    }
}

/// The last pass result per worker (readiness detail and tests); metrics carry the history.
#[derive(Debug, Default)]
pub struct WorkerStatus {
    last: parking_lot::Mutex<std::collections::HashMap<WorkerKind, PassResult>>,
}
impl WorkerStatus {
    fn record(&self, kind: WorkerKind, result: PassResult) {
        self.last.lock().insert(kind, result);
    }
    /// The last result of one worker, if it has run.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "readiness does not report per-worker results yet; the gear and worker tests read them"
        )
    )]
    #[must_use]
    pub fn last(&self, kind: WorkerKind) -> Option<PassResult> {
        self.last.lock().get(&kind).copied()
    }
}

/// What the scheduler is built from.
pub struct WorkerParts {
    /// The host connection: owner of the attested advisory-lock session.
    pub host: Db,
    /// The restricted per-class connections the workers reach storage through.
    pub connections: Arc<WorkerConnections>,
    /// The configured maintenance task grants and service actor.
    pub authority: Arc<MaintenanceAuthority>,
    /// Cadences and batch bounds.
    pub settings: WorkerSettings,
    /// The observability sink.
    pub metrics: Arc<dyn WorkerMetrics>,
    /// The per-state TTL expiry body.
    pub expiry: Arc<dyn expiry::TransitionSweep>,
    /// The draft auto-void body.
    pub auto_void: Arc<dyn expiry::TransitionSweep>,
    /// The D-188/D-198 recovery continuation body.
    pub recovery: Arc<dyn ExecutionRecovery>,
    /// The last pass result per worker.
    pub status: Arc<WorkerStatus>,
}

/// The running worker tasks; `stop` cancels them and waits for every pass to yield.
pub struct WorkerHandles {
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    scheduled: Vec<WorkerKind>,
}
impl WorkerHandles {
    /// The workers that were scheduled (those with a granted task and a class connection).
    #[must_use]
    pub fn scheduled(&self) -> &[WorkerKind] {
        &self.scheduled
    }
    /// Cooperative stop: cancel, then await each task.
    pub async fn stop(self) {
        self.cancel.cancel();
        for task in self.tasks {
            if let Err(error) = task.await {
                tracing::warn!(error = %error, "bss-orders: worker task ended abnormally");
            }
        }
    }
}

/// Start every worker whose task is granted and whose class connection exists. A worker with
/// a granted task but no connection is reported once and not scheduled (fail closed).
#[must_use]
pub fn start(parts: WorkerParts, parent: &CancellationToken) -> WorkerHandles {
    let cancel = parent.child_token();
    let shared = Arc::new(parts);
    let mut tasks = Vec::new();
    let mut scheduled = Vec::new();
    for kind in WorkerKind::ALL {
        let granted = kind
            .tasks()
            .iter()
            .filter(|task| shared.authority.grant(**task).is_some())
            .copied()
            .collect::<Vec<_>>();
        if granted.is_empty() {
            continue;
        }
        let missing = granted
            .iter()
            .map(|task| RoleClass::for_task(*task))
            .find(|class| shared.connections.get(*class).is_none());
        if let Some(class) = missing {
            tracing::error!(
                worker = kind.key(),
                class = class.config_key(),
                "bss-orders: worker not scheduled: class connection missing"
            );
            shared
                .metrics
                .pass(kind, PassResult::Unavailable, Duration::ZERO);
            continue;
        }
        scheduled.push(kind);
        let child = cancel.child_token();
        let parts = Arc::clone(&shared);
        tasks.push(tokio::spawn(supervise(kind, parts, child)));
    }
    WorkerHandles {
        cancel,
        tasks,
        scheduled,
    }
}

/// An interval that delays rather than bursts after a missed tick; the first tick is immediate.
fn ticking(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

fn period_of(kind: WorkerKind, settings: &WorkerSettings) -> Duration {
    match kind {
        WorkerKind::Expiry | WorkerKind::DraftAutoVoid => settings.sweep_interval,
        WorkerKind::IdempotencyCleanup => settings.cleanup_interval,
        WorkerKind::RetentionPurge => settings.retention_interval,
        WorkerKind::Audit => settings.verification_interval,
    }
}

/// Wait for the next tick; `false` once the lifecycle is cancelled.
async fn next_tick(interval: &mut tokio::time::Interval, cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        _ = interval.tick() => true,
    }
}

/// Replica-local state a worker loop carries between passes: the audit worker's per-namespace
/// cursors and capture holds, the cleanup worker's keyset positions.
#[derive(Debug, Default)]
struct LoopState {
    audit: audit::AuditWorkerState,
    cleanup: cleanup::CleanupCursor,
}

/// Run one worker loop on its own task and watch it end. A loop only returns once `cancel`
/// fires, so any other end (it returned uncancelled, or it panicked) is a lost worker: it is
/// logged at error, recorded as a `Failed` result in [`WorkerStatus`] and counted as a failed
/// pass, and the worker stays down on this replica until the next start. It is not restarted: a
/// panic is a bug, and a loop restarted into the same state would panic again (review finding
/// 262).
async fn supervise(kind: WorkerKind, parts: Arc<WorkerParts>, cancel: CancellationToken) {
    let inner = tokio::spawn(run_loop(kind, Arc::clone(&parts), cancel.clone()));
    match inner.await {
        Ok(()) if cancel.is_cancelled() => return,
        Ok(()) => tracing::error!(
            worker = kind.key(),
            "bss-orders: worker loop ended without cancellation; worker stopped on this replica"
        ),
        Err(error) => tracing::error!(
            worker = kind.key(),
            panicked = error.is_panic(),
            error = &error as &dyn std::error::Error,
            "bss-orders: worker loop aborted; worker stopped on this replica"
        ),
    }
    parts.status.record(kind, PassResult::Failed);
    parts.metrics.pass(kind, PassResult::Failed, Duration::ZERO);
}

async fn run_loop(kind: WorkerKind, parts: Arc<WorkerParts>, cancel: CancellationToken) {
    let period = period_of(kind, &parts.settings);
    let mut interval = ticking(period);
    let mut state = LoopState::default();
    tracing::info!(
        worker = kind.key(),
        period_seconds = period.as_secs(),
        "bss-orders: worker scheduled"
    );
    while next_tick(&mut interval, &cancel).await {
        if kind == WorkerKind::Audit {
            audit::tick(&parts, &mut state.audit, &cancel).await;
        } else {
            let cleanup = &mut state.cleanup;
            guarded_pass(&parts, kind, kind.key(), &cancel, |cancel| {
                Box::pin(single_pass(kind, &parts, cleanup, cancel))
            })
            .await;
        }
    }
    tracing::info!(worker = kind.key(), "bss-orders: worker stopped");
}

type PassFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WorkerError>> + Send + 'a>>;

/// One non-blocking acquisition attempt of the worker's advisory key on the host connection.
async fn acquire(
    parts: &WorkerParts,
    kind: WorkerKind,
    key: &str,
    cancel: &CancellationToken,
) -> Result<toolkit_db::DbLockGuard, PassResult> {
    let config = LockConfig {
        max_retries: Some(0),
        ..LockConfig::default()
    };
    let acquired = tokio::select! {
        () = cancel.cancelled() => return Err(PassResult::Cancelled),
        result = parts.host.try_lock(LOCK_NAMESPACE, key, config) => result,
    };
    match acquired {
        Ok(Some(guard)) => Ok(guard),
        Ok(None) => {
            tracing::debug!(
                worker = kind.key(),
                key,
                "bss-orders: pass contended, skipped"
            );
            Err(PassResult::Contended)
        }
        Err(error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders: advisory lock unavailable; pass abandoned");
            Err(PassResult::CoordinationFailed)
        }
    }
}

fn classify(kind: WorkerKind, key: &str, outcome: Result<(), WorkerError>) -> PassResult {
    match outcome {
        Ok(()) => PassResult::Completed,
        Err(WorkerError::Cancelled) => PassResult::Cancelled,
        Err(WorkerError::NotGranted | WorkerError::NotConfigured(_)) => PassResult::Unavailable,
        Err(error) if is_coordination_loss(&error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders: pass abandoned: database failure");
            PassResult::CoordinationFailed
        }
        Err(error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders: pass failed; retried next tick");
            PassResult::Failed
        }
    }
}

/// Acquire the worker's advisory key with one non-blocking attempt, run the bounded pass under
/// it, then release explicitly. Contention skips; a lock-session or database failure abandons
/// the pass; cancellation interrupts it; a pass that outlives [`WorkerKind::pass_budget`] fails
/// (D-218). A release error is reported, never treated as proof of continued ownership. The
/// cancel-safety contract of the body is [`guarded_pass_within`]'s.
pub async fn guarded_pass<'a, F>(
    parts: &'a WorkerParts,
    kind: WorkerKind,
    key: &str,
    cancel: &'a CancellationToken,
    body: F,
) -> PassResult
where
    F: FnOnce(&'a CancellationToken) -> PassFuture<'a>,
{
    guarded_pass_within(parts, kind, key, kind.pass_budget(), cancel, body).await
}

/// [`guarded_pass`] with an explicit pass deadline (tests shorten it).
///
/// # Cancel safety
///
/// `body` must be cancel-safe: lifecycle cancellation and the deadline drop it at whatever await
/// it is parked on, mid-transaction or mid-call, rather than waiting for its next
/// `is_cancelled` check. Every Orders pass body is safe under that drop because each durable
/// effect is one transaction that either committed whole or rolls back with the dropped future
/// (the toolkit transaction is rolled back when it is dropped uncommitted), and every
/// replica-local cursor is advanced only after the step it covers returned:
///
/// * expiry and auto-void ([`expiry::run_pass`]): the [`expiry::TransitionSweep`] body carries
///   the same contract (one locked-row recheck and engine transition per transaction);
/// * idempotency cleanup ([`cleanup::run_pass`]): read-only discovery, one conditional marker
///   delete per transaction, and the [`ExecutionRecovery`] contract for the continuation;
/// * retention purge ([`retention::run_pass`]): read-only discovery and one bounded delete batch
///   per transaction;
/// * audit ([`audit::namespace_pass`]): the checkpoint capture is one transaction (a dropped
///   capture records nothing) and the verification slice is one read-only snapshot whose cursor
///   is stored only after it returns.
///
/// The advisory key is released by this function once the body has returned or been dropped.
pub async fn guarded_pass_within<'a, F>(
    parts: &'a WorkerParts,
    kind: WorkerKind,
    key: &str,
    budget: Duration,
    cancel: &'a CancellationToken,
    body: F,
) -> PassResult
where
    F: FnOnce(&'a CancellationToken) -> PassFuture<'a>,
{
    let started = Instant::now();
    let guard = match acquire(parts, kind, key, cancel).await {
        Ok(guard) => guard,
        Err(result) => {
            parts.metrics.pass(kind, result, started.elapsed());
            parts.status.record(kind, result);
            return result;
        }
    };
    let outcome = tokio::select! {
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = tokio::time::timeout(budget, body(cancel)) => {
            result.unwrap_or(Err(WorkerError::DeadlineExceeded(budget)))
        }
    };
    let result = classify(kind, key, outcome);
    if let Err(error) = guard.release().await {
        tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders: advisory lock release failed (ownership not assumed)");
    }
    parts.metrics.pass(kind, result, started.elapsed());
    parts.status.record(kind, result);
    result
}

/// A transport-level database failure (the session is gone) rather than a logical one, wherever
/// the pass met it: a store call, candidate discovery, a secure-scope query or an audit read. The
/// engine's SQLSTATE classification ([`errors::is_connection_loss`]) decides, never the error
/// text, which is driver- and locale-dependent.
fn is_coordination_loss(error: &WorkerError) -> bool {
    match error {
        WorkerError::Store(error) | WorkerError::Discovery(DiscoveryError::Store(error)) => {
            errors::is_connection_loss_failure(error)
        }
        WorkerError::Scope(ScopeError::Db(error))
        | WorkerError::Audit(AuditStoreError::Store(ScopeError::Db(error))) => {
            errors::is_connection_loss(error)
        }
        _ => false,
    }
}

fn grant_for(
    parts: &WorkerParts,
    task: MaintenanceTask,
) -> Result<(TaskGrant<'_>, &Db), WorkerError> {
    let grant = parts.authority.grant(task).ok_or(WorkerError::NotGranted)?;
    let class = RoleClass::for_task(task);
    let db = parts
        .connections
        .get(class)
        .ok_or(WorkerError::NotConfigured(class))?;
    Ok((grant, db))
}

async fn single_pass(
    kind: WorkerKind,
    parts: &WorkerParts,
    cleanup_cursor: &mut cleanup::CleanupCursor,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    match kind {
        WorkerKind::Expiry => {
            let (grant, db) = grant_for(parts, MaintenanceTask::StateExpiry)?;
            expiry::run_pass(
                kind,
                parts.expiry.as_ref(),
                db,
                &grant,
                &parts.settings,
                parts.metrics.as_ref(),
                cancel,
            )
            .await
        }
        WorkerKind::DraftAutoVoid => {
            let (grant, db) = grant_for(parts, MaintenanceTask::DraftAutoVoid)?;
            expiry::run_pass(
                kind,
                parts.auto_void.as_ref(),
                db,
                &grant,
                &parts.settings,
                parts.metrics.as_ref(),
                cancel,
            )
            .await
        }
        WorkerKind::IdempotencyCleanup => {
            let (grant, db) = grant_for(parts, MaintenanceTask::IdempotencyCleanup)?;
            cleanup::run_pass(
                db,
                &grant,
                &parts.settings,
                parts.recovery.as_ref(),
                parts.metrics.as_ref(),
                cleanup_cursor,
                cancel,
            )
            .await
            .map(|_| ())
        }
        WorkerKind::RetentionPurge => {
            let (grant, db) = grant_for(parts, MaintenanceTask::RetentionPurge)?;
            retention::run_pass(db, &grant, &parts.settings, parts.metrics.as_ref(), cancel)
                .await
                .map(|_| ())
        }
        WorkerKind::Audit => Err(WorkerError::NotGranted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::engine::errors::tests::driver_error;

    /// Review finding 38: coordination loss is classified from the database cause on every store
    /// path a pass uses (store, discovery, secure scope, audit), never from the error text.
    #[test]
    fn coordination_loss_covers_every_store_path_and_ignores_message_text() {
        let lost = || {
            driver_error(
                "57P01",
                "terminating connection due to administrator command",
            )
        };
        let lost_paths = [
            WorkerError::Store(anyhow::Error::from(toolkit_db::DbError::Sea(lost()))),
            WorkerError::Discovery(DiscoveryError::Store(anyhow::Error::from(lost()))),
            WorkerError::Scope(ScopeError::Db(lost())),
            WorkerError::Audit(AuditStoreError::Store(ScopeError::Db(lost()))),
        ];
        for error in lost_paths {
            assert!(is_coordination_loss(&error), "{error}");
            assert_eq!(
                classify(WorkerKind::Audit, "k", Err(error)),
                PassResult::CoordinationFailed
            );
        }
        let failed_paths = [
            // Text that the old substring match took for a lost connection.
            WorkerError::Store(anyhow::anyhow!("connection reset: closed")),
            WorkerError::Store(anyhow::Error::from(driver_error(
                "42501",
                "permission denied: connection closed for role",
            ))),
            // A conflict on a live session is retried next tick, the session is not lost.
            WorkerError::Scope(ScopeError::Db(driver_error(
                "40001",
                "could not serialize access",
            ))),
        ];
        for error in failed_paths {
            assert!(!is_coordination_loss(&error), "{error}");
            assert_eq!(
                classify(WorkerKind::Audit, "k", Err(error)),
                PassResult::Failed
            );
        }
    }
}
