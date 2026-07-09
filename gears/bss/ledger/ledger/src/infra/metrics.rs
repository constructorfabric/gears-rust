//! OpenTelemetry adapter implementing [`LedgerMetricsPort`].
//!
//! Instruments are pulled from the process-global meter provider installed by
//! the host; a no-op until an exporter is wired, so emitting is always cheap
//! and safe. Instruments use full, literal Prometheus names: counters end in
//! `_total` and duration histograms in `_seconds`, with the suffix baked into
//! the instrument name (no `.with_unit()`); gauges carry the unit in the name
//! (`_seconds`) or stay bare. This matches the platform's
//! `add_metric_suffixes: false` collector posture, so the exporter renders the
//! names verbatim — consistent with RBAC / RMS / openbao.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::domain::ports::metrics::{LedgerMetricsPort, NoteOutcome, PostFlow, PostResult};

/// Meter / instrumentation scope name (matches the gear / toolkit module name).
pub(crate) const METER_NAME: &str = "bss-ledger";

// ─── Metric names (literal Prometheus form; `add_metric_suffixes: false`) ─────
// Full names with suffixes baked in: counters end `_total`, duration
// histograms `_seconds`. No `.with_unit()` — the collector renders verbatim.
const LEDGER_INVOICE_POST: &str = "ledger_invoice_post_total";
const LEDGER_INVOICE_POST_DURATION: &str = "ledger_invoice_post_duration_seconds";
const LEDGER_PAYMENT_SETTLE: &str = "ledger_payment_settle_total";
const LEDGER_SETTLEMENT_RETURN: &str = "ledger_settlement_return_total";
const LEDGER_CHARGEBACK: &str = "ledger_chargeback_total";
const LEDGER_ALLOCATION: &str = "ledger_allocation_total";
// Deferred-apply queue depth (rows still `QUEUED` for `PAYMENT_ALLOCATE`),
// observed by the sweep job. A bare gauge (no unit suffix — it is a count).
const LEDGER_ALLOCATION_QUEUE_DEPTH: &str = "ledger_allocation_queue_depth";
const LEDGER_CREDIT_APPLICATION: &str = "ledger_credit_application_total";
const LEDGER_PAYMENT_POST_DURATION: &str = "ledger_payment_post_duration_seconds";
// §9 catalog-wide alarm rollup `ledger_alarm_total{category,severity}` — the
// single alarm counter (the §4.7 tamper-failure count is this counter filtered
// to `category="TAMPER_VERIFY_FAILED"`).
const LEDGER_ALARM: &str = "ledger_alarm_total";
// Suspense-backlog gauges: a bare line count and an age in seconds.
const LEDGER_SUSPENSE_PENDING_LINES: &str = "ledger_suspense_pending_lines";
const LEDGER_SUSPENSE_PENDING_AGE: &str = "ledger_suspense_pending_age_seconds";
// ASC 606 recognition (Slice 4, design §9): the release-pass duration histogram,
// the recognized-revenue counter (by stream), the over-recognition +
// double-credit counters (paired with their alarms), and the parked-segment
// queue-depth gauge.
const LEDGER_RECOGNITION_RUN_DURATION: &str = "ledger_recognition_run_duration_seconds";
const LEDGER_REVENUE_RECOGNIZED_MINOR: &str = "ledger_revenue_recognized_minor";
const LEDGER_OVER_RECOGNITION: &str = "ledger_over_recognition_total";
const LEDGER_RECOGNITION_DOUBLE_CREDIT: &str = "ledger_recognition_double_credit_total";
const LEDGER_RECOGNITION_PERIOD_QUEUE_DEPTH: &str = "ledger_recognition_period_queue_depth";
// Dual-control governance (VHP-1852): pending created, decisions by outcome, and
// self-approval-denied attempts.
const LEDGER_DUAL_CONTROL_PENDING: &str = "ledger_dual_control_pending_total";
const LEDGER_DUAL_CONTROL_DECIDED: &str = "ledger_dual_control_decided_total";
const LEDGER_DUAL_CONTROL_SELF_APPROVAL_DENIED: &str =
    "ledger_dual_control_self_approval_denied_total";
// Count of `ledger_approval` rows currently in the transient `APPROVING` latch
// (Z8-1): a healthy approve clears it within one txn, so a value that stays > 0
// across maintenance ticks is a crash-stranded approve (excluded from the TTL
// sweep, holding the active-uniqueness slot) needing a manual re-approve.
const LEDGER_DUAL_CONTROL_APPROVING: &str = "ledger_dual_control_approving";
// ── §9 feature metrics ───────────────────────────────────────────────────────
// Two counters per §9: total runs + the failures subset (failures/runs = the
// break rate), rather than one counter with an `outcome` label.
const LEDGER_TAMPER_VERIFY_RUNS: &str = "ledger_tamper_verify_runs_total";
const LEDGER_TAMPER_VERIFY_FAILURES: &str = "ledger_tamper_verify_failures_total";
const LEDGER_TAMPER_CHAIN_LENGTH: &str = "ledger_tamper_chain_length";
const LEDGER_SCOPE_FREEZE_ACTIVE: &str = "ledger_scope_freeze_active";
const LEDGER_CROSS_TENANT_ACCESS: &str = "ledger_cross_tenant_access_total";
const LEDGER_REIDENTIFICATION: &str = "ledger_reidentification_total";
const LEDGER_ERASURE_APPLIED: &str = "ledger_erasure_applied_total";
const LEDGER_METADATA_CHANGE: &str = "ledger_metadata_change_total";
const LEDGER_AUDIT_PACK_EXPORT_DURATION: &str = "ledger_audit_pack_export_duration_seconds";
// Slice-3 adjustments (Group F): credit-note + debit-note attempts by outcome
// (posted / replayed / rejected, plus credit-note's blocked_split /
// blocked_headroom). One counter per note type, labelled by `outcome`.
const LEDGER_CREDIT_NOTE: &str = "ledger_credit_note_total";
const LEDGER_DEBIT_NOTE: &str = "ledger_debit_note_total";
// Slice-3 Phase-2 refunds (Group F, design §9): the `unknown_final` disposition
// counter, the open-`REFUND_CLEARING` balance + oldest-age gauges the aged-alarm
// job observes, and the stage-1-orphan counter.
// Group G adds the per-(phase, pattern) refund-post counter and the
// refund-quarantine queue-depth gauge.
const LEDGER_REFUND: &str = "ledger_refund_total";
const LEDGER_REFUND_QUARANTINE_DEPTH: &str = "ledger_refund_quarantine_depth";
const LEDGER_REFUND_UNKNOWN_FINAL: &str = "ledger_refund_unknown_final_total";
const LEDGER_REFUND_CLEARING_BALANCE: &str = "ledger_refund_clearing_balance_minor";
const LEDGER_REFUND_CLEARING_AGED: &str = "ledger_refund_clearing_aged_seconds";
const LEDGER_STAGE1_REFUND_ORPHAN: &str = "ledger_stage1_refund_orphan_total";
// FX & multi-currency (Slice 5 Phase 3, design §9): the unrealized-revaluation
// run-pass duration histogram + the provider-fallback counter.
const LEDGER_FX_REVALUATION_DURATION: &str = "ledger_fx_revaluation_duration_seconds";
const LEDGER_FX_PROVIDER_FALLBACK: &str = "ledger_fx_provider_fallback_total";
// Realized-FX amount counter (Slice 5 Phase 2, design §9): the functional
// magnitude moved through `FX_GAIN_LOSS` on a cross-currency allocation close,
// labelled by functional currency + gain/loss direction.
const LEDGER_FX_REALIZED_MINOR: &str = "ledger_fx_realized_minor";
// ── Slice 7 Phase 3 reconciliation (design §9 / spec §3.5 J4) ─────────────────
// The per-check signed-variance gauge, the run + out-of-tolerance counters (both
// by check_type; out_of_tolerance/runs = the breach rate), the period-close-
// blocked counter (by reason), and the exception-queue depth gauge (by type).
const LEDGER_RECONCILIATION_VARIANCE_MINOR: &str = "ledger_reconciliation_variance_minor";
const LEDGER_RECONCILIATION_RUNS: &str = "ledger_reconciliation_runs_total";
const LEDGER_RECONCILIATION_OUT_OF_TOLERANCE: &str = "ledger_reconciliation_out_of_tolerance_total";
const LEDGER_PERIOD_CLOSE_BLOCKED: &str = "ledger_period_close_blocked_total";
const LEDGER_EXCEPTION_QUEUE_DEPTH: &str = "ledger_exception_queue_depth";

// ─── Bounded `reason_code` label ─────────────────────────────────────────────
// The cross-tenant `reason_code` is a caller-supplied header. Used verbatim as a
// Prometheus label it is an unbounded-cardinality DoS (a caller mints a new
// series per distinct value). The closed allow-list below bounds the
// `ledger_cross_tenant_access_total{reason_code}` label to a fixed size: a code
// is matched case-insensitively, anything else buckets to `"other"`. The raw,
// unbounded `reason_code` is still recorded verbatim in the `cross-tenant-access`
// forensic audit record — only the metric label is bounded. Extend the list as
// product ratifies new investigation reason codes (it stays a closed set).
const KNOWN_REASON_CODES: &[&str] = &[
    "DISPUTE",
    "DISPUTE_INVESTIGATION",
    "FRAUD",
    "FRAUD_INVESTIGATION",
    "LEGAL_HOLD",
    "GDPR_REQUEST",
    "SECURITY_INCIDENT",
    "RECONCILIATION",
    "AUDIT",
    "SUPPORT",
];

/// Bucket a caller-supplied `reason_code` to a bounded metric label: a known
/// code (case-insensitive) maps to its canonical token, everything else to
/// `"other"`. Keeps `ledger_cross_tenant_access_total{reason_code}` cardinality
/// bounded to `KNOWN_REASON_CODES.len() + 1`.
fn bounded_reason_code(reason_code: &str) -> &'static str {
    let upper = reason_code.trim().to_ascii_uppercase();
    KNOWN_REASON_CODES
        .iter()
        .copied()
        .find(|known| *known == upper)
        .unwrap_or("other")
}

/// OpenTelemetry-backed metrics handle for the ledger invoice-posting domain.
pub struct LedgerMetricsMeter {
    invoice_post: Counter<u64>,
    invoice_post_duration: Histogram<f64>,
    payment_settle: Counter<u64>,
    settlement_return: Counter<u64>,
    chargeback: Counter<u64>,
    allocation: Counter<u64>,
    allocation_queue_depth: Gauge<i64>,
    credit_application: Counter<u64>,
    payment_post_duration: Histogram<f64>,
    suspense_pending_lines: Gauge<i64>,
    suspense_pending_age: Gauge<f64>,
    invariant_alarm: Counter<u64>,
    tamper_verify_runs: Counter<u64>,
    tamper_verify_failures: Counter<u64>,
    tamper_chain_length: Gauge<i64>,
    scope_freeze_active: Gauge<i64>,
    cross_tenant_access: Counter<u64>,
    reidentification: Counter<u64>,
    erasure_applied: Counter<u64>,
    metadata_change: Counter<u64>,
    audit_pack_export_duration: Histogram<f64>,
    recognition_run_duration: Histogram<f64>,
    revenue_recognized_minor: Counter<u64>,
    over_recognition: Counter<u64>,
    recognition_double_credit: Counter<u64>,
    recognition_period_queue_depth: Gauge<i64>,
    dual_control_pending: Counter<u64>,
    dual_control_decided: Counter<u64>,
    dual_control_self_approval_denied: Counter<u64>,
    dual_control_approving: Gauge<i64>,
    credit_note: Counter<u64>,
    debit_note: Counter<u64>,
    refund: Counter<u64>,
    refund_quarantine_depth: Gauge<i64>,
    refund_unknown_final: Counter<u64>,
    refund_clearing_balance: Gauge<i64>,
    refund_clearing_aged: Gauge<f64>,
    stage1_refund_orphan: Counter<u64>,
    fx_revaluation_duration: Histogram<f64>,
    fx_provider_fallback: Counter<u64>,
    fx_realized_minor: Counter<u64>,
    reconciliation_variance_minor: Gauge<i64>,
    reconciliation_runs: Counter<u64>,
    reconciliation_out_of_tolerance: Counter<u64>,
    period_close_blocked: Counter<u64>,
    exception_queue_depth: Gauge<i64>,
}

impl std::fmt::Debug for LedgerMetricsMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerMetricsMeter").finish_non_exhaustive()
    }
}

impl LedgerMetricsMeter {
    /// Build the instrument set from the supplied meter.
    #[must_use]
    #[allow(clippy::too_many_lines)] // a flat instrument-builder list, one block per metric; no branching to factor out
    pub fn new(meter: &Meter) -> Self {
        Self {
            invoice_post: meter
                .u64_counter(LEDGER_INVOICE_POST)
                .with_description("Invoice-post attempts by outcome (posted/replayed/rejected)")
                .build(),
            invoice_post_duration: meter
                .f64_histogram(LEDGER_INVOICE_POST_DURATION)
                .with_description("End-to-end invoice-post latency, seconds")
                .with_boundaries(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5])
                .build(),
            payment_settle: meter
                .u64_counter(LEDGER_PAYMENT_SETTLE)
                .with_description("Payment-settle attempts by outcome (posted/replayed/rejected)")
                .build(),
            settlement_return: meter
                .u64_counter(LEDGER_SETTLEMENT_RETURN)
                .with_description(
                    "Settlement-return attempts by outcome (posted/replayed/rejected)",
                )
                .build(),
            chargeback: meter
                .u64_counter(LEDGER_CHARGEBACK)
                .with_description(
                    "Chargeback dispute-phase attempts by outcome (posted/replayed/rejected)",
                )
                .build(),
            allocation: meter
                .u64_counter(LEDGER_ALLOCATION)
                .with_description(
                    "Payment-allocation attempts by outcome (posted/replayed/rejected)",
                )
                .build(),
            allocation_queue_depth: meter
                .i64_gauge(LEDGER_ALLOCATION_QUEUE_DEPTH)
                .with_description(
                    "Deferred-apply queue depth: allocations still QUEUED awaiting drain",
                )
                .build(),
            credit_application: meter
                .u64_counter(LEDGER_CREDIT_APPLICATION)
                .with_description(
                    "Reusable-credit grant/apply attempts by outcome (posted/replayed/rejected)",
                )
                .build(),
            payment_post_duration: meter
                .f64_histogram(LEDGER_PAYMENT_POST_DURATION)
                .with_description("End-to-end payment-post latency, seconds")
                .with_boundaries(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5])
                .build(),
            suspense_pending_lines: meter
                .i64_gauge(LEDGER_SUSPENSE_PENDING_LINES)
                .with_description("Live count of pending (suspense) lines, by tenant")
                .build(),
            suspense_pending_age: meter
                .f64_gauge(LEDGER_SUSPENSE_PENDING_AGE)
                .with_description("Age of the oldest pending (suspense) line in seconds, by tenant")
                .build(),
            invariant_alarm: meter
                .u64_counter(LEDGER_ALARM)
                .with_description("Ledger invariant/catalog alarms by category + severity")
                .build(),
            tamper_verify_runs: meter
                .u64_counter(LEDGER_TAMPER_VERIFY_RUNS)
                .with_description("Chain-Verifier runs (total)")
                .build(),
            tamper_verify_failures: meter
                .u64_counter(LEDGER_TAMPER_VERIFY_FAILURES)
                .with_description("Chain-Verifier runs that found a break")
                .build(),
            tamper_chain_length: meter
                .i64_gauge(LEDGER_TAMPER_CHAIN_LENGTH)
                .with_description("Observed tamper-evidence chain length, by tenant")
                .build(),
            scope_freeze_active: meter
                .i64_gauge(LEDGER_SCOPE_FREEZE_ACTIVE)
                .with_description("Count of active scope freezes, by tenant")
                .build(),
            cross_tenant_access: meter
                .u64_counter(LEDGER_CROSS_TENANT_ACCESS)
                .with_description("Cross-tenant audit-access elevations by reason_code")
                .build(),
            reidentification: meter
                .u64_counter(LEDGER_REIDENTIFICATION)
                .with_description("Forensic payer re-identification events")
                .build(),
            erasure_applied: meter
                .u64_counter(LEDGER_ERASURE_APPLIED)
                .with_description("GDPR right-to-erasure events applied")
                .build(),
            metadata_change: meter
                .u64_counter(LEDGER_METADATA_CHANGE)
                .with_description("Controlled-metadata changes by attribute")
                .build(),
            audit_pack_export_duration: meter
                .f64_histogram(LEDGER_AUDIT_PACK_EXPORT_DURATION)
                .with_description("Audit-pack CSV export latency, seconds")
                .with_boundaries(vec![0.05, 0.1, 0.5, 1.0, 5.0, 15.0, 60.0])
                .build(),
            recognition_run_duration: meter
                .f64_histogram(LEDGER_RECOGNITION_RUN_DURATION)
                .with_description("End-to-end recognition-run pass latency, seconds")
                .with_boundaries(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
                ])
                .build(),
            revenue_recognized_minor: meter
                .u64_counter(LEDGER_REVENUE_RECOGNIZED_MINOR)
                .with_description("Revenue recognized (minor units) on release, by stream")
                .build(),
            over_recognition: meter
                .u64_counter(LEDGER_OVER_RECOGNITION)
                .with_description("Releases rejected by the per-schedule over-recognition cap")
                .build(),
            recognition_double_credit: meter
                .u64_counter(LEDGER_RECOGNITION_DOUBLE_CREDIT)
                .with_description("Detected attempts to re-credit an already-released segment")
                .build(),
            recognition_period_queue_depth: meter
                .i64_gauge(LEDGER_RECOGNITION_PERIOD_QUEUE_DEPTH)
                .with_description(
                    "Recognition segments parked QUEUED (a predecessor period not yet DONE)",
                )
                .build(),
            dual_control_pending: meter
                .u64_counter(LEDGER_DUAL_CONTROL_PENDING)
                .with_description("Dual-control approvals created (over-threshold), by kind")
                .build(),
            dual_control_decided: meter
                .u64_counter(LEDGER_DUAL_CONTROL_DECIDED)
                .with_description("Dual-control approval decisions, by kind + decision")
                .build(),
            dual_control_self_approval_denied: meter
                .u64_counter(LEDGER_DUAL_CONTROL_SELF_APPROVAL_DENIED)
                .with_description("Dual-control self-approval attempts denied, by kind")
                .build(),
            dual_control_approving: meter
                .i64_gauge(LEDGER_DUAL_CONTROL_APPROVING)
                .with_description(
                    "Approvals in the transient APPROVING latch; sustained > 0 = a \
                     crash-stranded approve needing manual re-approve (Z8-1)",
                )
                .build(),
            credit_note: meter
                .u64_counter(LEDGER_CREDIT_NOTE)
                .with_description(
                    "Credit-note attempts by outcome \
                     (posted/replayed/rejected/blocked_split/blocked_headroom)",
                )
                .build(),
            debit_note: meter
                .u64_counter(LEDGER_DEBIT_NOTE)
                .with_description("Debit-note attempts by outcome (posted/replayed/rejected)")
                .build(),
            refund: meter
                .u64_counter(LEDGER_REFUND)
                .with_description("Refund phases posted, by phase + pattern (fresh posts only)")
                .build(),
            refund_quarantine_depth: meter
                .i64_gauge(LEDGER_REFUND_QUARANTINE_DEPTH)
                .with_description(
                    "Refund-before-payment rows still QUEUED on the REFUND_QUARANTINE flow",
                )
                .build(),
            refund_unknown_final: meter
                .u64_counter(LEDGER_REFUND_UNKNOWN_FINAL)
                .with_description(
                    "Refund unknown_final dispositions (REFUND_CLEARING cleared to a loss line + \
                     secured-audit record)",
                )
                .build(),
            refund_clearing_balance: meter
                .i64_gauge(LEDGER_REFUND_CLEARING_BALANCE)
                .with_description("Open (unsettled) REFUND_CLEARING balance, minor units, by tenant")
                .build(),
            refund_clearing_aged: meter
                .f64_gauge(LEDGER_REFUND_CLEARING_AGED)
                .with_description(
                    "Age of the oldest open REFUND_CLEARING balance in seconds, by tenant",
                )
                .build(),
            stage1_refund_orphan: meter
                .u64_counter(LEDGER_STAGE1_REFUND_ORPHAN)
                .with_description(
                    "Stage-1 refunds with no matching stage-2 / reversal beyond the aging threshold",
                )
                .build(),
            fx_revaluation_duration: meter
                .f64_histogram(LEDGER_FX_REVALUATION_DURATION)
                .with_description("Unrealized-revaluation run-pass latency, seconds")
                .with_boundaries(vec![0.05, 0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 1800.0])
                .build(),
            fx_provider_fallback: meter
                .u64_counter(LEDGER_FX_PROVIDER_FALLBACK)
                .with_description(
                    "FX rate resolves that fell to a lower-priority provider, by provider",
                )
                .build(),
            fx_realized_minor: meter
                .u64_counter(LEDGER_FX_REALIZED_MINOR)
                .with_description(
                    "Realized FX magnitude (minor units) on a cross-currency close, by \
                     functional currency + gain/loss direction",
                )
                .build(),
            reconciliation_variance_minor: meter
                .i64_gauge(LEDGER_RECONCILIATION_VARIANCE_MINOR)
                .with_description(
                    "Signed variance (minor units) observed by a reconciliation check, by \
                     check_type",
                )
                .build(),
            reconciliation_runs: meter
                .u64_counter(LEDGER_RECONCILIATION_RUNS)
                .with_description("Reconciliation check runs (total), by check_type")
                .build(),
            reconciliation_out_of_tolerance: meter
                .u64_counter(LEDGER_RECONCILIATION_OUT_OF_TOLERANCE)
                .with_description(
                    "Reconciliation checks whose variance breached tolerance, by check_type",
                )
                .build(),
            period_close_blocked: meter
                .u64_counter(LEDGER_PERIOD_CLOSE_BLOCKED)
                .with_description("Period-close attempts rejected by a pre-close gate, by reason")
                .build(),
            exception_queue_depth: meter
                .i64_gauge(LEDGER_EXCEPTION_QUEUE_DEPTH)
                .with_description("Open exception-queue depth, by exception type")
                .build(),
        }
    }

    /// Build a handle bound to the process-global meter provider.
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }
}

impl LedgerMetricsPort for LedgerMetricsMeter {
    fn invoice_post(&self, result: PostResult, flow: PostFlow) {
        self.invoice_post.add(
            1,
            &[
                KeyValue::new("result", result.as_str()),
                KeyValue::new("flow", flow.as_str()),
            ],
        );
    }

    fn invoice_post_duration(&self, secs: f64, flow: PostFlow) {
        self.invoice_post_duration
            .record(secs, &[KeyValue::new("flow", flow.as_str())]);
    }

    fn payment_settle(&self, result: PostResult) {
        self.payment_settle
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn settlement_return(&self, result: PostResult) {
        self.settlement_return
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn chargeback(&self, result: PostResult) {
        self.chargeback
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn allocation(&self, result: PostResult) {
        self.allocation
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn allocation_queue_depth(&self, depth: i64) {
        // Unlabelled: the sweep reads the cross-tenant total per tick. A
        // per-tenant breakdown would need a `tenant` label (bounded cardinality
        // concern) — deferred until a tenant-scoped depth is actually needed.
        self.allocation_queue_depth.record(depth, &[]);
    }

    fn credit_application(&self, result: PostResult) {
        self.credit_application
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn payment_post_duration(&self, secs: f64, flow: PostFlow) {
        self.payment_post_duration
            .record(secs, &[KeyValue::new("flow", flow.as_str())]);
    }

    fn suspense_pending(&self, tenant: uuid::Uuid, lines: i64, oldest_age_secs: f64) {
        let tenant_label = KeyValue::new("tenant", tenant.to_string());
        self.suspense_pending_lines
            .record(lines, std::slice::from_ref(&tenant_label));
        self.suspense_pending_age
            .record(oldest_age_secs, std::slice::from_ref(&tenant_label));
    }

    fn invariant_alarm(&self, category: &str, severity: &str) {
        self.invariant_alarm.add(
            1,
            &[
                KeyValue::new("category", category.to_owned()),
                KeyValue::new("severity", severity.to_owned()),
            ],
        );
    }

    fn recognition_run_duration(&self, secs: f64) {
        self.recognition_run_duration.record(secs, &[]);
    }

    fn revenue_recognized_minor(&self, amount_minor: i64, revenue_stream: &str) {
        // A released segment amount is `>= 0` (the `recognition_segment`
        // `amount_minor >= 0` CHECK), so the conversion never clamps in practice;
        // a defensively-negative value contributes 0 rather than panicking.
        let amount = u64::try_from(amount_minor).unwrap_or(0);
        self.revenue_recognized_minor.add(
            amount,
            &[KeyValue::new("stream", revenue_stream.to_owned())],
        );
    }

    fn over_recognition(&self) {
        self.over_recognition.add(1, &[]);
    }

    fn recognition_double_credit(&self) {
        self.recognition_double_credit.add(1, &[]);
    }

    fn recognition_period_queue_depth(&self, depth: i64) {
        // Unlabelled: a run observes the count it parked this pass (a per-tenant
        // breakdown would need a `tenant` label — bounded-cardinality concern,
        // deferred, mirroring `allocation_queue_depth`).
        self.recognition_period_queue_depth.record(depth, &[]);
    }

    fn dual_control_pending(&self, kind: &str) {
        self.dual_control_pending
            .add(1, &[KeyValue::new("kind", kind.to_owned())]);
    }

    fn dual_control_decided(&self, kind: &str, decision: &str) {
        self.dual_control_decided.add(
            1,
            &[
                KeyValue::new("kind", kind.to_owned()),
                KeyValue::new("decision", decision.to_owned()),
            ],
        );
    }

    fn dual_control_self_approval_denied(&self, kind: &str) {
        self.dual_control_self_approval_denied
            .add(1, &[KeyValue::new("kind", kind.to_owned())]);
    }

    fn dual_control_approving(&self, count: i64) {
        self.dual_control_approving.record(count, &[]);
    }

    fn tamper_verify_run(&self, _tenant: uuid::Uuid, failed: bool) {
        self.tamper_verify_runs.add(1, &[]);
        if failed {
            self.tamper_verify_failures.add(1, &[]);
        }
    }

    fn chain_length(&self, tenant: uuid::Uuid, length: i64) {
        self.tamper_chain_length
            .record(length, &[KeyValue::new("tenant", tenant.to_string())]);
    }

    fn scope_freeze_active(&self, tenant: uuid::Uuid, active: i64) {
        self.scope_freeze_active
            .record(active, &[KeyValue::new("tenant", tenant.to_string())]);
    }

    fn cross_tenant_access(&self, reason_code: &str) {
        // Bound the label cardinality: unknown codes bucket to
        // "other". The raw value is kept in the forensic audit record, not here.
        self.cross_tenant_access.add(
            1,
            &[KeyValue::new(
                "reason_code",
                bounded_reason_code(reason_code),
            )],
        );
    }

    fn reidentification(&self) {
        self.reidentification.add(1, &[]);
    }

    fn erasure_applied(&self) {
        self.erasure_applied.add(1, &[]);
    }

    fn metadata_change(&self, attribute: &str) {
        self.metadata_change
            .add(1, &[KeyValue::new("attribute", attribute.to_owned())]);
    }

    fn audit_pack_export_duration(&self, secs: f64) {
        self.audit_pack_export_duration.record(secs, &[]);
    }

    fn credit_note(&self, outcome: NoteOutcome) {
        self.credit_note
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn debit_note(&self, outcome: NoteOutcome) {
        self.debit_note
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn refund(&self, phase: &str, pattern: &str) {
        self.refund.add(
            1,
            &[
                KeyValue::new("phase", phase.to_owned()),
                KeyValue::new("pattern", pattern.to_owned()),
            ],
        );
    }

    fn refund_quarantine_depth(&self, depth: i64) {
        // Unlabelled: the sweep reads the cross-tenant total per tick (a per-tenant
        // breakdown would need a `tenant` label — bounded-cardinality concern,
        // deferred, mirroring `allocation_queue_depth`).
        self.refund_quarantine_depth.record(depth, &[]);
    }

    fn refund_unknown_final(&self) {
        self.refund_unknown_final.add(1, &[]);
    }

    fn refund_clearing_balance_minor(&self, tenant: uuid::Uuid, balance_minor: i64) {
        self.refund_clearing_balance.record(
            balance_minor,
            &[KeyValue::new("tenant", tenant.to_string())],
        );
    }

    fn refund_clearing_aged_seconds(&self, tenant: uuid::Uuid, age_secs: f64) {
        self.refund_clearing_aged
            .record(age_secs, &[KeyValue::new("tenant", tenant.to_string())]);
    }

    fn stage1_refund_orphan(&self) {
        self.stage1_refund_orphan.add(1, &[]);
    }
    fn fx_revaluation_duration(&self, secs: f64) {
        self.fx_revaluation_duration.record(secs, &[]);
    }
    fn fx_provider_fallback(&self, provider: &str) {
        self.fx_provider_fallback
            .add(1, &[KeyValue::new("provider", provider.to_owned())]);
    }
    fn fx_realized_minor(&self, amount_minor: i64, functional_currency: &str, direction: &str) {
        // The magnitude is the non-negative functional amount on the FX_GAIN_LOSS
        // line (the domain guarantees it `>= 0`); a defensively-negative value
        // contributes 0 rather than panicking (mirrors `revenue_recognized_minor`).
        let amount = u64::try_from(amount_minor).unwrap_or(0);
        self.fx_realized_minor.add(
            amount,
            &[
                KeyValue::new("functional_currency", functional_currency.to_owned()),
                KeyValue::new("direction", direction.to_owned()),
            ],
        );
    }

    fn reconciliation_variance_minor(&self, check_type: &str, variance_minor: i64) {
        self.reconciliation_variance_minor.record(
            variance_minor,
            &[KeyValue::new("check_type", check_type.to_owned())],
        );
    }

    fn reconciliation_run(&self, check_type: &str) {
        self.reconciliation_runs
            .add(1, &[KeyValue::new("check_type", check_type.to_owned())]);
    }

    fn reconciliation_out_of_tolerance(&self, check_type: &str) {
        self.reconciliation_out_of_tolerance
            .add(1, &[KeyValue::new("check_type", check_type.to_owned())]);
    }

    fn period_close_blocked(&self, reason: &str) {
        self.period_close_blocked
            .add(1, &[KeyValue::new("reason", reason.to_owned())]);
    }

    fn exception_queue_depth(&self, exception_type: &str, depth: i64) {
        self.exception_queue_depth
            .record(depth, &[KeyValue::new("type", exception_type.to_owned())]);
    }
}

#[cfg(feature = "test-support")]
pub mod test_harness {
    //! In-memory OpenTelemetry harness for asserting emitted ledger metrics.
    #![allow(clippy::expect_used, clippy::missing_panics_doc, dead_code)]

    use opentelemetry::metrics::{Meter, MeterProvider};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::{LedgerMetricsMeter, METER_NAME};

    /// In-memory meter provider + exporter for unit and integration tests.
    pub struct MetricsHarness {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
    }

    impl MetricsHarness {
        #[must_use]
        pub fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { provider, exporter }
        }

        #[must_use]
        pub fn meter(&self) -> Meter {
            self.provider.meter(METER_NAME)
        }

        /// A metrics handle bound to this harness's provider.
        #[must_use]
        pub fn metrics(&self) -> LedgerMetricsMeter {
            LedgerMetricsMeter::new(&self.meter())
        }

        /// Flush aggregated data into the in-memory exporter.
        pub fn force_flush(&self) {
            self.provider
                .force_flush()
                .expect("test meter provider should flush");
        }

        /// Sum all matching `u64` counter data points.
        #[must_use]
        pub fn counter_value(&self, name: &str, expected_attrs: &[(&str, &str)]) -> u64 {
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");
            let mut total = 0u64;
            for rm in &metrics {
                for sm in rm.scope_metrics() {
                    for metric in sm.metrics() {
                        if metric.name() == name
                            && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                        {
                            for dp in sum.data_points() {
                                if attributes_match(dp.attributes(), expected_attrs) {
                                    total += dp.value();
                                }
                            }
                        }
                    }
                }
            }
            total
        }

        /// Read the latest matching `i64` gauge value (last write wins per
        /// attribute set). Returns `0` when no matching data point exists.
        #[must_use]
        pub fn gauge_value(&self, name: &str, expected_attrs: &[(&str, &str)]) -> i64 {
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");
            let mut value = 0i64;
            for rm in &metrics {
                for sm in rm.scope_metrics() {
                    for metric in sm.metrics() {
                        if metric.name() == name
                            && let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data()
                        {
                            for dp in gauge.data_points() {
                                if attributes_match(dp.attributes(), expected_attrs) {
                                    value = dp.value();
                                }
                            }
                        }
                    }
                }
            }
            value
        }

        /// Sum matching histogram sample counts.
        #[must_use]
        pub fn histogram_count(&self, name: &str, expected_attrs: &[(&str, &str)]) -> u64 {
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");
            let mut total = 0u64;
            for rm in &metrics {
                for sm in rm.scope_metrics() {
                    for metric in sm.metrics() {
                        if metric.name() == name
                            && let AggregatedMetrics::F64(MetricData::Histogram(hist)) =
                                metric.data()
                        {
                            for dp in hist.data_points() {
                                if attributes_match(dp.attributes(), expected_attrs) {
                                    total += dp.count();
                                }
                            }
                        }
                    }
                }
            }
            total
        }
    }

    impl Default for MetricsHarness {
        fn default() -> Self {
            Self::new()
        }
    }

    fn attributes_match<'a>(
        actual_attrs: impl Iterator<Item = &'a opentelemetry::KeyValue>,
        expected: &[(&str, &str)],
    ) -> bool {
        let actual = actual_attrs.collect::<Vec<_>>();
        expected.iter().all(|(k, v)| {
            actual
                .iter()
                .any(|kv| kv.key.as_str() == *k && kv.value.as_str() == *v)
        }) && actual.len() == expected.len()
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
