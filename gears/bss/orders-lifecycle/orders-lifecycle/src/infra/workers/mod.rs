//! The five Orders-owned workers (DESIGN Foundation contract §3.8; 01 §3.4 item 5): per-state
//! TTL expiry, draft auto-void, idempotency-window cleanup, retention purge and audit
//! verification/checkpointing.
//!
//! Coordination is the toolkit's session advisory lock on the host connection,
//! `Db::try_lock("bss-orders-lifecycle", <key>)` with exactly one non-blocking attempt: a
//! contended pass skips and reschedules, the guard is held for the bounded pass and released
//! explicitly, and lifecycle cancellation stops scheduling and interrupts a pass between its
//! batches. Session loss is not fencing: every write stays protected by its own transactional
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
pub const LOCK_NAMESPACE: &str = "bss-orders-lifecycle";

/// The authoritative roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerKind {
    Expiry,
    DraftAutoVoid,
    IdempotencyCleanup,
    RetentionPurge,
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
    #[error("maintenance task is not granted")]
    NotGranted,
    #[error("maintenance connection class `{}` is not configured", .0.config_key())]
    NotConfigured(RoleClass),
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error(transparent)]
    Audit(#[from] AuditStoreError),
    #[error(transparent)]
    Encoding(#[from] crate::domain::audit::AuditError),
    #[error("worker store failure: {0}")]
    Store(#[from] anyhow::Error),
    #[error("pass cancelled")]
    Cancelled,
}
impl From<toolkit_db::DbError> for WorkerError {
    fn from(error: toolkit_db::DbError) -> Self {
        Self::Store(error.into())
    }
}

/// Validated cadences and batch bounds (baselines from 01 §3.1 item 6, §3.5, D-100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSettings {
    pub cleanup_interval: Duration,
    pub cleanup_batch: u64,
    pub retention_interval: Duration,
    pub retention_batch: u64,
    pub retention_batches_per_pass: u32,
    pub checkpoint_interval: Duration,
    pub verification_interval: Duration,
    pub verification_orders_per_pass: u64,
    pub sweep_interval: Duration,
    pub sweep_batch: u64,
}
impl WorkerSettings {
    /// The design baselines: cleanup every 60 s / 500 rows, daily purge batches of 5,000,
    /// daily checkpoints, a verification slice of 500 orders per namespace per minute, and
    /// expiry/auto-void sweeps every 60 s over 500 candidates.
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
    last: std::sync::Mutex<std::collections::HashMap<WorkerKind, PassResult>>,
}
impl WorkerStatus {
    fn record(&self, kind: WorkerKind, result: PassResult) {
        self.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind, result);
    }
    /// The last result of one worker, if it has run.
    #[must_use]
    pub fn last(&self, kind: WorkerKind) -> Option<PassResult> {
        self.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&kind)
            .copied()
    }
}

/// What the scheduler is built from.
pub struct WorkerParts {
    /// The host connection: owner of the attested advisory-lock session.
    pub host: Db,
    pub connections: Arc<WorkerConnections>,
    pub authority: Arc<MaintenanceAuthority>,
    pub settings: WorkerSettings,
    pub metrics: Arc<dyn WorkerMetrics>,
    pub expiry: Arc<dyn expiry::TransitionSweep>,
    pub auto_void: Arc<dyn expiry::TransitionSweep>,
    pub recovery: Arc<dyn ExecutionRecovery>,
    pub capture_hooks: audit::CaptureHooks,
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
                tracing::warn!(error = %error, "bss-orders-lifecycle: worker task ended abnormally");
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
                "bss-orders-lifecycle: worker not scheduled: class connection missing"
            );
            shared
                .metrics
                .pass(kind, PassResult::Unavailable, Duration::ZERO);
            continue;
        }
        scheduled.push(kind);
        let child = cancel.child_token();
        let parts = Arc::clone(&shared);
        tasks.push(tokio::spawn(async move {
            run_loop(kind, parts, child).await;
        }));
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

async fn run_loop(kind: WorkerKind, parts: Arc<WorkerParts>, cancel: CancellationToken) {
    let period = period_of(kind, &parts.settings);
    let mut interval = ticking(period);
    let mut state = LoopState::default();
    tracing::info!(
        worker = kind.key(),
        period_seconds = period.as_secs(),
        "bss-orders-lifecycle: worker scheduled"
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
    tracing::info!(worker = kind.key(), "bss-orders-lifecycle: worker stopped");
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
                "bss-orders-lifecycle: pass contended, skipped"
            );
            Err(PassResult::Contended)
        }
        Err(error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders-lifecycle: advisory lock unavailable; pass abandoned");
            Err(PassResult::CoordinationFailed)
        }
    }
}

fn classify(kind: WorkerKind, key: &str, outcome: Result<(), WorkerError>) -> PassResult {
    match outcome {
        Ok(()) => PassResult::Completed,
        Err(WorkerError::Cancelled) => PassResult::Cancelled,
        Err(WorkerError::NotGranted | WorkerError::NotConfigured(_)) => PassResult::Unavailable,
        Err(WorkerError::Store(error)) if is_coordination_loss(&error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders-lifecycle: pass abandoned: database failure");
            PassResult::CoordinationFailed
        }
        Err(error) => {
            tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders-lifecycle: pass failed; retried next tick");
            PassResult::Failed
        }
    }
}

/// Acquire the worker's advisory key with one non-blocking attempt, run the bounded pass under
/// it, then release explicitly. Contention skips; a lock-session or database failure abandons
/// the pass; cancellation interrupts it. A release error is reported, never treated as proof
/// of continued ownership.
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
        result = body(cancel) => result,
    };
    let result = classify(kind, key, outcome);
    if let Err(error) = guard.release().await {
        tracing::warn!(worker = kind.key(), key, error = %error, "bss-orders-lifecycle: advisory lock release failed (ownership not assumed)");
    }
    parts.metrics.pass(kind, result, started.elapsed());
    parts.status.record(kind, result);
    result
}

/// A transport-level database failure (the connection is gone) rather than a logical one.
fn is_coordination_loss(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("connection")
        && (text.contains("closed") || text.contains("reset") || text.contains("terminat"))
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
