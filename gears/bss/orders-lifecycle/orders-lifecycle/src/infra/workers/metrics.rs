//! What the Orders-owned workers report (01 §3.5 item 4, §3.8 observability): pass outcomes,
//! purge/cleanup counts and backlogs, recovery backlog, integrity findings, checkpoint and
//! verification coverage ages. Instruments live on the process meter under
//! `bss-orders-lifecycle`; the trait keeps the passes testable with a recording sink.
use std::sync::Mutex;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use uuid::Uuid;

use super::WorkerKind;
use crate::infra::maintenance::RetentionTable;

/// How a scheduled pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassResult {
    /// The pass ran to completion; its body may still have reported findings.
    Completed,
    /// Another replica holds the key: skipped and rescheduled.
    Contended,
    /// The lock session or the database failed before or during the pass: abandoned.
    CoordinationFailed,
    /// Lifecycle cancellation interrupted the pass.
    Cancelled,
    /// The task's body or class connection is not configured or not delivered.
    Unavailable,
    /// The body failed; the next tick retries.
    Failed,
}
impl PassResult {
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Contended => "contended",
            Self::CoordinationFailed => "coordination_failed",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// An integrity finding: alerted, never repaired (DESIGN §3.8, D-100 item 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityFinding {
    /// The committed chain of one order failed recomputation or continuity.
    Chain {
        order_id: Uuid,
        error: crate::domain::audit::VerifyError,
    },
    /// The aggregate counter is positive but no committed entry exists at it.
    HeadMissing { order_id: Uuid, counter: i64 },
    /// The committed entry at the counter stores a digest of the wrong length.
    HeadMalformed { order_id: Uuid, counter: i64 },
    /// A committed entry exists beyond the aggregate counter.
    Overflow {
        order_id: Uuid,
        sequence: i64,
        counter: i64,
    },
    /// The counter is below a sequence the previous checkpoint recorded.
    CounterRollback {
        order_id: Uuid,
        recorded: i64,
        counter: i64,
    },
    /// The entry at a previously recorded sequence is gone.
    PrefixMissing { order_id: Uuid, sequence: i64 },
    /// The entry at a previously recorded sequence carries another digest.
    PrefixDigest { order_id: Uuid, sequence: i64 },
    /// An order the previous checkpoint recorded is no longer in the namespace.
    OrderMissing { order_id: Uuid, recorded: i64 },
    /// A stored checkpoint digest does not recompute from its stored header and members.
    CheckpointDigest { sequence: i64 },
    /// A stored checkpoint does not link to its predecessor (or genesis).
    CheckpointPredecessor { sequence: i64 },
    /// Checkpoint sequences are not contiguous from 1.
    CheckpointSequence { expected: i64, found: i64 },
    /// A stored checkpoint's members are malformed (count, order, digest length, version).
    CheckpointMembers { sequence: i64, reason: String },
    /// A recorded member position lies beyond the order's current counter.
    MemberBeyondCounter {
        order_id: Uuid,
        sequence: i64,
        counter: i64,
    },
    /// The recomputed chain digest at a recorded member position differs from the record.
    MemberDigest { order_id: Uuid, sequence: i64 },
}
impl IntegrityFinding {
    /// The metric label of the finding class.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Chain { .. } => "chain",
            Self::HeadMissing { .. } => "head_missing",
            Self::HeadMalformed { .. } => "head_malformed",
            Self::Overflow { .. } => "overflow",
            Self::CounterRollback { .. } => "counter_rollback",
            Self::PrefixMissing { .. } => "prefix_missing",
            Self::PrefixDigest { .. } => "prefix_digest",
            Self::OrderMissing { .. } => "order_missing",
            Self::CheckpointDigest { .. } => "checkpoint_digest",
            Self::CheckpointPredecessor { .. } => "checkpoint_predecessor",
            Self::CheckpointSequence { .. } => "checkpoint_sequence",
            Self::CheckpointMembers { .. } => "checkpoint_members",
            Self::MemberBeyondCounter { .. } => "member_beyond_counter",
            Self::MemberDigest { .. } => "member_digest",
        }
    }
}

/// Outcome of one checkpoint-phase visit of a namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointResult {
    /// A new checkpoint was published.
    Appended,
    /// The last checkpoint is younger than the interval: no capture was due (its age is
    /// reported alongside).
    Current,
    /// A discrepancy was found: nothing was recorded.
    Mismatch,
    /// A capture is withheld after a mismatch until the retry interval elapses; the verifier
    /// keeps alerting and the checkpoint age keeps growing.
    Withheld,
    /// A competing append won the sequence: nothing was recorded.
    Conflict,
    Failed,
}
impl CheckpointResult {
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Current => "current",
            Self::Mismatch => "mismatch",
            Self::Withheld => "withheld",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
        }
    }
}

/// Tally of one cleanup pass over discovered expired markers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupTally {
    pub deleted: u64,
    pub live: u64,
    pub unresolved: u64,
    pub replaced: u64,
    pub failed: u64,
}

/// Tally of one pass of the D-188/D-198 recovery continuation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryTally {
    pub progressed: u64,
    pub terminal: u64,
    pub deferred: u64,
    pub unavailable: u64,
}

fn store_label(store: RetentionTable) -> &'static str {
    match store {
        RetentionTable::RefusedAudit => "refused_audit",
        RetentionTable::PreviewDiagnostics => "preview_diagnostics",
        RetentionTable::ReadAccessLog => "read_access_log",
    }
}
fn seconds(age: Option<Duration>) -> u64 {
    age.map_or(0, |d| d.as_secs())
}

/// The worker observability port.
pub trait WorkerMetrics: Send + Sync {
    fn pass(&self, worker: WorkerKind, result: PassResult, duration: Duration);
    fn retention_batch(&self, store: RetentionTable, purged: u64, duration: Duration);
    fn retention_backlog(&self, store: RetentionTable, rows: u64, oldest_overdue: Option<Duration>);
    fn cleanup(&self, tally: CleanupTally);
    fn cleanup_backlog(&self, rows: u64, oldest_expired: Option<Duration>);
    fn recovery(&self, backlog: u64, oldest: Option<Duration>, tally: RecoveryTally);
    fn integrity_finding(&self, namespace: Uuid, finding: &IntegrityFinding);
    fn checkpoint(
        &self,
        namespace: Uuid,
        result: CheckpointResult,
        members: u64,
        age: Option<Duration>,
    );
    fn verification(&self, namespace: Uuid, orders: u64, coverage_age: Option<Duration>);
    fn sweep(&self, worker: WorkerKind, candidates: u64, transitioned: u64, skipped: u64);
}

/// OpenTelemetry instruments on the process meter.
pub struct OtelWorkerMetrics {
    passes: Counter<u64>,
    pass_duration: Histogram<u64>,
    last_success: Gauge<u64>,
    purged: Counter<u64>,
    batch_duration: Histogram<u64>,
    retention_backlog: Gauge<u64>,
    retention_oldest: Gauge<u64>,
    markers: Counter<u64>,
    cleanup_backlog: Gauge<u64>,
    cleanup_oldest: Gauge<u64>,
    recovery_backlog: Gauge<u64>,
    recovery_oldest: Gauge<u64>,
    recovery_outcomes: Counter<u64>,
    findings: Counter<u64>,
    checkpoints: Counter<u64>,
    checkpoint_age: Gauge<u64>,
    verified: Counter<u64>,
    coverage_age: Gauge<u64>,
    sweeps: Counter<u64>,
}
impl OtelWorkerMetrics {
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("bss-orders-lifecycle");
        Self {
            passes: meter
                .u64_counter("orders_worker_passes_total")
                .with_description("Worker passes by worker and result")
                .build(),
            pass_duration: meter
                .u64_histogram("orders_worker_pass_duration_ms")
                .with_description("Worker pass duration")
                .build(),
            last_success: meter
                .u64_gauge("orders_worker_last_success_unix_seconds")
                .with_description("Unix time of the last completed pass per worker (a missed run is an aging value)")
                .build(),
            purged: meter
                .u64_counter("orders_retention_rows_purged_total")
                .with_description("Rows purged per retention store")
                .build(),
            batch_duration: meter
                .u64_histogram("orders_retention_batch_duration_ms")
                .with_description("Retention batch duration per store")
                .build(),
            retention_backlog: meter
                .u64_gauge("orders_retention_backlog_rows")
                .with_description("Eligible rows still present after the pass per store")
                .build(),
            retention_oldest: meter
                .u64_gauge("orders_retention_oldest_overdue_seconds")
                .with_description("Age beyond the window of the oldest overdue row per store")
                .build(),
            markers: meter
                .u64_counter("orders_idempotency_markers_total")
                .with_description("Discovered expired markers by sweep outcome")
                .build(),
            cleanup_backlog: meter
                .u64_gauge("orders_idempotency_expired_backlog_rows")
                .with_description("Expired markers still present after the pass")
                .build(),
            cleanup_oldest: meter
                .u64_gauge("orders_idempotency_expired_oldest_seconds")
                .with_description("Age past expiry of the oldest expired marker")
                .build(),
            recovery_backlog: meter
                .u64_gauge("orders_execution_recovery_backlog")
                .with_description("Unresolved D-188/D-198 executions whose lease lapsed")
                .build(),
            recovery_oldest: meter
                .u64_gauge("orders_execution_recovery_oldest_seconds")
                .with_description("Age of the oldest lapsed unresolved execution")
                .build(),
            recovery_outcomes: meter
                .u64_counter("orders_execution_recovery_total")
                .with_description("Recovery continuation outcomes")
                .build(),
            findings: meter
                .u64_counter("orders_audit_integrity_findings_total")
                .with_description("Audit integrity findings by kind (alert on any)")
                .build(),
            checkpoints: meter
                .u64_counter("orders_audit_checkpoints_total")
                .with_description("Checkpoint capture attempts by result")
                .build(),
            checkpoint_age: meter
                .u64_gauge("orders_audit_checkpoint_age_seconds")
                .with_description("Age of the last successful checkpoint per namespace")
                .build(),
            verified: meter
                .u64_counter("orders_audit_orders_verified_total")
                .with_description("Orders whose committed chain was fully recomputed")
                .build(),
            coverage_age: meter
                .u64_gauge("orders_audit_verification_coverage_age_seconds")
                .with_description("Age of the last completed full verification pass per namespace")
                .build(),
            sweeps: meter
                .u64_counter("orders_transition_sweeps_total")
                .with_description("Expiry/auto-void sweep candidates by outcome")
                .build(),
        }
    }
}
impl Default for OtelWorkerMetrics {
    fn default() -> Self {
        Self::new()
    }
}
fn namespace_label(namespace: Uuid) -> KeyValue {
    KeyValue::new("namespace", namespace.to_string())
}
impl WorkerMetrics for OtelWorkerMetrics {
    fn pass(&self, worker: WorkerKind, result: PassResult, duration: Duration) {
        let labels = [
            KeyValue::new("worker", worker.key()),
            KeyValue::new("result", result.token()),
        ];
        self.passes.add(1, &labels);
        self.pass_duration.record(
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            &[KeyValue::new("worker", worker.key())],
        );
        if result == PassResult::Completed {
            let now = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0);
            self.last_success
                .record(now, &[KeyValue::new("worker", worker.key())]);
        }
    }
    fn retention_batch(&self, store: RetentionTable, purged: u64, duration: Duration) {
        let labels = [KeyValue::new("store", store_label(store))];
        self.purged.add(purged, &labels);
        self.batch_duration.record(
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            &labels,
        );
    }
    fn retention_backlog(
        &self,
        store: RetentionTable,
        rows: u64,
        oldest_overdue: Option<Duration>,
    ) {
        let labels = [KeyValue::new("store", store_label(store))];
        self.retention_backlog.record(rows, &labels);
        self.retention_oldest
            .record(seconds(oldest_overdue), &labels);
    }
    fn cleanup(&self, tally: CleanupTally) {
        for (outcome, count) in [
            ("deleted", tally.deleted),
            ("live", tally.live),
            ("unresolved", tally.unresolved),
            ("replaced", tally.replaced),
            ("failed", tally.failed),
        ] {
            self.markers
                .add(count, &[KeyValue::new("outcome", outcome)]);
        }
    }
    fn cleanup_backlog(&self, rows: u64, oldest_expired: Option<Duration>) {
        self.cleanup_backlog.record(rows, &[]);
        self.cleanup_oldest.record(seconds(oldest_expired), &[]);
    }
    fn recovery(&self, backlog: u64, oldest: Option<Duration>, tally: RecoveryTally) {
        self.recovery_backlog.record(backlog, &[]);
        self.recovery_oldest.record(seconds(oldest), &[]);
        for (outcome, count) in [
            ("progressed", tally.progressed),
            ("terminal", tally.terminal),
            ("deferred", tally.deferred),
            ("unavailable", tally.unavailable),
        ] {
            self.recovery_outcomes
                .add(count, &[KeyValue::new("outcome", outcome)]);
        }
    }
    fn integrity_finding(&self, namespace: Uuid, finding: &IntegrityFinding) {
        self.findings.add(
            1,
            &[
                KeyValue::new("kind", finding.kind()),
                namespace_label(namespace),
            ],
        );
    }
    fn checkpoint(
        &self,
        namespace: Uuid,
        result: CheckpointResult,
        _members: u64,
        age: Option<Duration>,
    ) {
        self.checkpoints.add(
            1,
            &[
                KeyValue::new("result", result.token()),
                namespace_label(namespace),
            ],
        );
        if let Some(age) = age {
            self.checkpoint_age
                .record(age.as_secs(), &[namespace_label(namespace)]);
        }
    }
    fn verification(&self, namespace: Uuid, orders: u64, coverage_age: Option<Duration>) {
        self.verified.add(orders, &[namespace_label(namespace)]);
        if let Some(age) = coverage_age {
            self.coverage_age
                .record(age.as_secs(), &[namespace_label(namespace)]);
        }
    }
    fn sweep(&self, worker: WorkerKind, candidates: u64, transitioned: u64, skipped: u64) {
        for (outcome, count) in [
            ("candidate", candidates),
            ("transitioned", transitioned),
            ("skipped", skipped),
        ] {
            self.sweeps.add(
                count,
                &[
                    KeyValue::new("worker", worker.key()),
                    KeyValue::new("outcome", outcome),
                ],
            );
        }
    }
}

/// One recorded observation (tests and the readiness snapshot).
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    Pass {
        worker: WorkerKind,
        result: PassResult,
    },
    RetentionBatch {
        store: RetentionTable,
        purged: u64,
    },
    RetentionBacklog {
        store: RetentionTable,
        rows: u64,
        oldest_overdue: Option<Duration>,
    },
    Cleanup(CleanupTally),
    CleanupBacklog {
        rows: u64,
        oldest_expired: Option<Duration>,
    },
    Recovery {
        backlog: u64,
        oldest: Option<Duration>,
        tally: RecoveryTally,
    },
    Finding {
        namespace: Uuid,
        finding: IntegrityFinding,
    },
    Checkpoint {
        namespace: Uuid,
        result: CheckpointResult,
        members: u64,
        age: Option<Duration>,
    },
    Verification {
        namespace: Uuid,
        orders: u64,
        coverage_age: Option<Duration>,
    },
    Sweep {
        worker: WorkerKind,
        candidates: u64,
        transitioned: u64,
        skipped: u64,
    },
}

/// Records every observation in memory; the tests assert on it.
#[derive(Debug, Default)]
pub struct RecordingMetrics {
    observations: Mutex<Vec<Observation>>,
}
impl RecordingMetrics {
    /// Everything observed so far.
    ///
    /// # Panics
    /// Never: the lock is only poisoned by a panicking recorder.
    #[must_use]
    pub fn observations(&self) -> Vec<Observation> {
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn record(&self, observation: Observation) {
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(observation);
    }
}
impl WorkerMetrics for RecordingMetrics {
    fn pass(&self, worker: WorkerKind, result: PassResult, _duration: Duration) {
        self.record(Observation::Pass { worker, result });
    }
    fn retention_batch(&self, store: RetentionTable, purged: u64, _duration: Duration) {
        self.record(Observation::RetentionBatch { store, purged });
    }
    fn retention_backlog(
        &self,
        store: RetentionTable,
        rows: u64,
        oldest_overdue: Option<Duration>,
    ) {
        self.record(Observation::RetentionBacklog {
            store,
            rows,
            oldest_overdue,
        });
    }
    fn cleanup(&self, tally: CleanupTally) {
        self.record(Observation::Cleanup(tally));
    }
    fn cleanup_backlog(&self, rows: u64, oldest_expired: Option<Duration>) {
        self.record(Observation::CleanupBacklog {
            rows,
            oldest_expired,
        });
    }
    fn recovery(&self, backlog: u64, oldest: Option<Duration>, tally: RecoveryTally) {
        self.record(Observation::Recovery {
            backlog,
            oldest,
            tally,
        });
    }
    fn integrity_finding(&self, namespace: Uuid, finding: &IntegrityFinding) {
        self.record(Observation::Finding {
            namespace,
            finding: finding.clone(),
        });
    }
    fn checkpoint(
        &self,
        namespace: Uuid,
        result: CheckpointResult,
        members: u64,
        age: Option<Duration>,
    ) {
        self.record(Observation::Checkpoint {
            namespace,
            result,
            members,
            age,
        });
    }
    fn verification(&self, namespace: Uuid, orders: u64, coverage_age: Option<Duration>) {
        self.record(Observation::Verification {
            namespace,
            orders,
            coverage_age,
        });
    }
    fn sweep(&self, worker: WorkerKind, candidates: u64, transitioned: u64, skipped: u64) {
        self.record(Observation::Sweep {
            worker,
            candidates,
            transitioned,
            skipped,
        });
    }
}
