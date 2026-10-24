//! What the Orders-owned workers report (01 §3.5 item 4, §3.8 observability): pass outcomes,
//! purge/cleanup counts and backlogs, recovery backlog, integrity findings, checkpoint and
//! verification coverage ages. Instruments live on the process meter under
//! `bss-orders`; the trait keeps the passes testable with a recording sink.
//!
//! No instrument carries the audit namespace (a tenant UUID) as an attribute: the series count
//! would grow with tenants (D-223). The audit counters are totals across namespaces, and the two
//! audit age gauges report the oldest age across the namespaces one audit cycle visited
//! ([`WorkerMetrics::audit_cycle`]). The namespace stays in the `orders.audit` logs.
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use uuid::Uuid;

use super::WorkerKind;
use super::expiry::SweepReport;
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
    /// The `result` label value of the pass metric.
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
        /// The order whose chain failed.
        order_id: Uuid,
        /// Why recomputation or continuity failed.
        error: crate::domain::audit::VerifyError,
    },
    /// The aggregate counter is positive but no committed entry exists at it.
    HeadMissing {
        /// The affected order.
        order_id: Uuid,
        /// The aggregate audit counter.
        counter: i64,
    },
    /// The committed entry at the counter stores a digest of the wrong length.
    HeadMalformed {
        /// The affected order.
        order_id: Uuid,
        /// The aggregate audit counter.
        counter: i64,
    },
    /// A committed entry exists beyond the aggregate counter.
    Overflow {
        /// The affected order.
        order_id: Uuid,
        /// The sequence of the committed entry beyond the counter.
        sequence: i64,
        /// The aggregate audit counter.
        counter: i64,
    },
    /// The counter is below a sequence the previous checkpoint recorded.
    CounterRollback {
        /// The affected order.
        order_id: Uuid,
        /// The sequence the previous checkpoint recorded.
        recorded: i64,
        /// The current aggregate counter, below `recorded`.
        counter: i64,
    },
    /// The entry at a previously recorded sequence is gone.
    PrefixMissing {
        /// The affected order.
        order_id: Uuid,
        /// The previously recorded sequence.
        sequence: i64,
    },
    /// The entry at a previously recorded sequence carries another digest.
    PrefixDigest {
        /// The affected order.
        order_id: Uuid,
        /// The previously recorded sequence.
        sequence: i64,
    },
    /// An order the previous checkpoint recorded is no longer in the namespace.
    OrderMissing {
        /// The missing order.
        order_id: Uuid,
        /// The sequence the previous checkpoint recorded for it.
        recorded: i64,
    },
    /// A stored checkpoint digest does not recompute from its stored header and members.
    CheckpointDigest {
        /// The checkpoint sequence.
        sequence: i64,
    },
    /// A stored checkpoint does not link to its predecessor (or genesis).
    CheckpointPredecessor {
        /// The checkpoint sequence.
        sequence: i64,
    },
    /// Checkpoint sequences are not contiguous from 1.
    CheckpointSequence {
        /// The next contiguous sequence.
        expected: i64,
        /// The sequence actually stored.
        found: i64,
    },
    /// A stored checkpoint's members are malformed (count, order, digest length, version).
    CheckpointMembers {
        /// The checkpoint sequence.
        sequence: i64,
        /// What is malformed.
        reason: String,
    },
    /// A recorded member position lies beyond the order's current counter.
    MemberBeyondCounter {
        /// The affected order.
        order_id: Uuid,
        /// The recorded member position.
        sequence: i64,
        /// The order's current aggregate counter.
        counter: i64,
    },
    /// The recomputed chain digest at a recorded member position differs from the record.
    MemberDigest {
        /// The affected order.
        order_id: Uuid,
        /// The recorded member position.
        sequence: i64,
    },
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
    /// The capture failed with an error: nothing was recorded and the pass returns the error.
    Failed,
}
impl CheckpointResult {
    /// The `result` label value of the checkpoint metric.
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
    /// Expired markers deleted.
    pub deleted: u64,
    /// Markers kept because their window or lease was live again on recheck.
    pub live: u64,
    /// Markers kept because they link an unresolved execution.
    pub unresolved: u64,
    /// Markers not deleted because the key now carries another generation.
    pub replaced: u64,
    /// Marker sweeps that failed; retried next pass.
    pub failed: u64,
}

/// Tally of one pass of the D-188/D-198 recovery continuation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryTally {
    /// Executions that made bounded progress and stay unresolved.
    pub progressed: u64,
    /// Executions that reached a terminal status.
    pub terminal: u64,
    /// Executions deferred because their inputs are unavailable right now.
    pub deferred: u64,
    /// Executions with no recovery body delivered for their kind.
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
    /// Record one pass of `worker`: how it ended and its wall-clock duration.
    fn pass(&self, worker: WorkerKind, result: PassResult, duration: Duration);
    /// Record one retention batch: rows purged from `store` and the batch duration.
    fn retention_batch(&self, store: RetentionTable, purged: u64, duration: Duration);
    /// Record the eligible rows left in `store` after a pass and how far past its window the oldest is.
    fn retention_backlog(&self, store: RetentionTable, rows: u64, oldest_overdue: Option<Duration>);
    /// Record the marker sweep outcomes of one cleanup pass.
    fn cleanup(&self, tally: CleanupTally);
    /// Record the expired markers left after a cleanup pass and the age past expiry of the oldest.
    fn cleanup_backlog(&self, rows: u64, oldest_expired: Option<Duration>);
    /// Record the lapsed-execution backlog, the age of the oldest lapsed lease and the
    /// continuation outcomes of one pass.
    fn recovery(&self, backlog: u64, oldest: Option<Duration>, tally: RecoveryTally);
    /// Record one audit integrity finding in `namespace` (the namespace is not a metric
    /// attribute).
    fn integrity_finding(&self, namespace: Uuid, finding: &IntegrityFinding);
    /// Record one checkpoint-phase visit of `namespace`: its result, the members published and the
    /// age of the last successful checkpoint (reported to the age gauge through
    /// [`audit_cycle`](Self::audit_cycle)).
    fn checkpoint(
        &self,
        namespace: Uuid,
        result: CheckpointResult,
        members: u64,
        age: Option<Duration>,
    );
    /// Record the orders whose chain was fully recomputed in `namespace` and the verification
    /// coverage age (reported to the age gauge through [`audit_cycle`](Self::audit_cycle)).
    fn verification(&self, namespace: Uuid, orders: u64, coverage_age: Option<Duration>);
    /// Record the end of one audit cycle over the roster: the oldest checkpoint age and the
    /// oldest verification coverage age among the namespaces this replica visited, each `None`
    /// when no visited namespace reported one.
    fn audit_cycle(&self, oldest_checkpoint: Option<Duration>, oldest_coverage: Option<Duration>);
    /// Record one expiry/auto-void sweep: candidates found, transitioned and skipped.
    fn sweep(&self, worker: WorkerKind, report: &SweepReport);
    /// Record how many workers this replica scheduled (zero without a maintenance block, and
    /// after stop).
    fn scheduled(&self, workers: u64);
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
    scheduled: Gauge<u64>,
}
impl OtelWorkerMetrics {
    /// Build the instruments on the `bss-orders` process meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("bss-orders");
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
                .with_description(
                    "Oldest age of the last successful checkpoint across the namespaces of one audit cycle",
                )
                .build(),
            verified: meter
                .u64_counter("orders_audit_orders_verified_total")
                .with_description("Orders whose committed chain was fully recomputed")
                .build(),
            coverage_age: meter
                .u64_gauge("orders_audit_verification_coverage_age_seconds")
                .with_description(
                    "Oldest age of the last completed full verification pass across the namespaces of one audit cycle",
                )
                .build(),
            sweeps: meter
                .u64_counter("orders_transition_sweeps_total")
                .with_description("Expiry/auto-void sweep candidates by outcome")
                .build(),
            scheduled: meter
                .u64_gauge("orders_workers_scheduled")
                .with_description(
                    "Workers this replica scheduled (0: no maintenance block, every worker off)",
                )
                .build(),
        }
    }
}
impl Default for OtelWorkerMetrics {
    fn default() -> Self {
        Self::new()
    }
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
    fn integrity_finding(&self, _namespace: Uuid, finding: &IntegrityFinding) {
        self.findings
            .add(1, &[KeyValue::new("kind", finding.kind())]);
    }
    fn checkpoint(
        &self,
        _namespace: Uuid,
        result: CheckpointResult,
        _members: u64,
        _age: Option<Duration>,
    ) {
        self.checkpoints
            .add(1, &[KeyValue::new("result", result.token())]);
    }
    fn verification(&self, _namespace: Uuid, orders: u64, _coverage_age: Option<Duration>) {
        self.verified.add(orders, &[]);
    }
    fn audit_cycle(&self, oldest_checkpoint: Option<Duration>, oldest_coverage: Option<Duration>) {
        if let Some(age) = oldest_checkpoint {
            self.checkpoint_age.record(age.as_secs(), &[]);
        }
        if let Some(age) = oldest_coverage {
            self.coverage_age.record(age.as_secs(), &[]);
        }
    }
    fn sweep(&self, worker: WorkerKind, report: &SweepReport) {
        for (outcome, count) in [
            ("candidate", report.candidates),
            ("transitioned", report.transitioned),
            ("skipped", report.skipped),
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
    fn scheduled(&self, workers: u64) {
        self.scheduled.record(workers, &[]);
    }
}

/// One recorded observation of [`RecordingMetrics`] (tests only).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// A [`WorkerMetrics::pass`] call.
    Pass {
        /// The worker that ran.
        worker: WorkerKind,
        /// How the pass ended.
        result: PassResult,
    },
    /// A [`WorkerMetrics::retention_batch`] call.
    RetentionBatch {
        /// The retention store.
        store: RetentionTable,
        /// Rows purged by the batch.
        purged: u64,
    },
    /// A [`WorkerMetrics::retention_backlog`] call.
    RetentionBacklog {
        /// The retention store.
        store: RetentionTable,
        /// Eligible rows still present after the pass.
        rows: u64,
        /// How far past its window the oldest overdue row is.
        oldest_overdue: Option<Duration>,
    },
    /// A [`WorkerMetrics::cleanup`] call.
    Cleanup(CleanupTally),
    /// A [`WorkerMetrics::cleanup_backlog`] call.
    CleanupBacklog {
        /// Expired markers still present after the pass.
        rows: u64,
        /// Age past expiry of the oldest expired marker.
        oldest_expired: Option<Duration>,
    },
    /// A [`WorkerMetrics::recovery`] call.
    Recovery {
        /// Unresolved executions whose lease lapsed.
        backlog: u64,
        /// Age of the oldest lapsed lease.
        oldest: Option<Duration>,
        /// Continuation outcomes of the pass.
        tally: RecoveryTally,
    },
    /// A [`WorkerMetrics::integrity_finding`] call.
    Finding {
        /// The audit namespace.
        namespace: Uuid,
        /// What was found.
        finding: IntegrityFinding,
    },
    /// A [`WorkerMetrics::checkpoint`] call.
    Checkpoint {
        /// The audit namespace.
        namespace: Uuid,
        /// How the visit ended.
        result: CheckpointResult,
        /// Members published (zero unless a checkpoint was appended).
        members: u64,
        /// Age of the last successful checkpoint, if known.
        age: Option<Duration>,
    },
    /// A [`WorkerMetrics::verification`] call.
    Verification {
        /// The audit namespace.
        namespace: Uuid,
        /// Orders whose committed chain was fully recomputed.
        orders: u64,
        /// Age of the last completed full verification pass, if any.
        coverage_age: Option<Duration>,
    },
    /// A [`WorkerMetrics::sweep`] call.
    Sweep {
        /// The sweeping worker.
        worker: WorkerKind,
        /// Candidates the sweep found.
        candidates: u64,
        /// Candidates transitioned.
        transitioned: u64,
        /// Candidates skipped by the locked-row recheck.
        skipped: u64,
    },
    /// A [`WorkerMetrics::scheduled`] call.
    Scheduled(u64),
    /// A [`WorkerMetrics::audit_cycle`] call.
    AuditCycle {
        /// The oldest checkpoint age among the visited namespaces.
        oldest_checkpoint: Option<Duration>,
        /// The oldest verification coverage age among the visited namespaces.
        oldest_coverage: Option<Duration>,
    },
}

/// Records every observation in memory; the tests assert on it. Compiled into test builds only,
/// so production carries no recorder (the process meter is the only `WorkerMetrics`).
#[cfg(test)]
#[derive(Debug, Default)]
pub struct RecordingMetrics {
    observations: parking_lot::Mutex<Vec<Observation>>,
}
#[cfg(test)]
impl RecordingMetrics {
    /// Everything observed so far.
    ///
    /// # Panics
    /// Never: the lock is only poisoned by a panicking recorder.
    #[must_use]
    pub fn observations(&self) -> Vec<Observation> {
        self.observations.lock().clone()
    }
    fn record(&self, observation: Observation) {
        self.observations.lock().push(observation);
    }
}
#[cfg(test)]
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
    fn sweep(&self, worker: WorkerKind, report: &SweepReport) {
        let SweepReport {
            candidates,
            transitioned,
            skipped,
        } = *report;
        self.record(Observation::Sweep {
            worker,
            candidates,
            transitioned,
            skipped,
        });
    }
    fn scheduled(&self, workers: u64) {
        self.record(Observation::Scheduled(workers));
    }
    fn audit_cycle(&self, oldest_checkpoint: Option<Duration>, oldest_coverage: Option<Duration>) {
        self.record(Observation::AuditCycle {
            oldest_checkpoint,
            oldest_coverage,
        });
    }
}
