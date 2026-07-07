//! Typed metrics port for the BSS ledger invoice-posting domain.
//!
//! Mirrors the canonical RBAC pattern: label values come from a closed enum
//! (`as_str()` → `snake_case`) so metric cardinality is bounded at compile
//! time. The infra adapter (`crate::infra::metrics`) implements
//! [`LedgerMetricsPort`]; [`NoopLedgerMetrics`] is the safe default for unit
//! tests and any construction before an exporter is wired.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// Outcome class of one invoice-post attempt (the `result` label on
/// `ledger_invoice_post_total`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostResult {
    /// A fresh balanced entry was written.
    Posted,
    /// An idempotent re-post returned the prior entry (no new ledger effect).
    Replayed,
    /// The attempt was rejected before any ledger effect (validation /
    /// invariant / authz).
    Rejected,
}

impl PostResult {
    /// Stable `snake_case` label value for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Posted => "posted",
            Self::Replayed => "replayed",
            Self::Rejected => "rejected",
        }
    }
}

/// Which write flow drove a post attempt (the `flow` label on
/// `ledger_invoice_post_total` / `_duration_seconds`), so reversals and
/// corrections are not mis-counted as fresh invoice posts.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostFlow {
    /// A fresh invoice post (`POST /journal-entries`).
    InvoicePost,
    /// A strict line-negation reversal.
    Reversal,
    /// A mapping-correction re-post.
    MappingCorrection,
    /// A payment settlement post (`POST …/payments:settle`).
    Settle,
    /// A payment allocation post (`POST …/payments/{id}:allocate`).
    Allocate,
    /// A reusable-credit grant/apply post (`POST …/credit:grant|:apply`).
    CreditApply,
    /// A settlement-return post (`POST …/payments/{id}/returns`).
    SettlementReturn,
    /// A chargeback dispute-phase post (`POST …/disputes/{id}/phases`).
    Chargeback,
}

impl PostFlow {
    /// Stable `snake_case` label value for this flow.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvoicePost => "invoice_post",
            Self::Reversal => "reversal",
            Self::MappingCorrection => "mapping_correction",
            Self::Settle => "settle",
            Self::Allocate => "allocate",
            Self::CreditApply => "credit_apply",
            Self::SettlementReturn => "settlement_return",
            Self::Chargeback => "chargeback",
        }
    }
}

/// Outcome class of one credit-note / debit-note attempt (the `outcome` label on
/// `ledger_credit_note_total` / `ledger_debit_note_total`, Slice 3 §4.2 / §4.3 /
/// Group F). A closed enum so the label cardinality is bounded at compile time
/// (mirrors [`PostResult`]). The `blocked_*` variants name WHY a note was
/// rejected before any books effect (the credit-note split / headroom gates); a
/// debit note only ever reports `Posted` / `Replayed` / `Rejected` (it has no
/// split-ambiguous or headroom-cap rejection — it raises the headroom).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteOutcome {
    /// A fresh note posted (balanced compensating / charge entry written).
    Posted,
    /// An idempotent re-post returned the prior entry (no new ledger effect).
    Replayed,
    /// Rejected before any ledger effect for a reason with no dedicated label
    /// (shape / payer-closed / account-closed / not-found / infra).
    Rejected,
    /// A credit note blocked because its recognized-vs-deferred split was
    /// indeterminable (`CREDIT_NOTE_SPLIT_AMBIGUOUS`, block-on-ambiguous).
    BlockedSplit,
    /// A credit note blocked because it would exceed the invoice's remaining
    /// headroom (`CREDIT_NOTE_EXCEEDS_HEADROOM`).
    BlockedHeadroom,
}

impl NoteOutcome {
    /// Stable `snake_case` label value for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Posted => "posted",
            Self::Replayed => "replayed",
            Self::Rejected => "rejected",
            Self::BlockedSplit => "blocked_split",
            Self::BlockedHeadroom => "blocked_headroom",
        }
    }
}

/// Metrics sink for the invoice-posting domain. Implemented by the infra `OTel`
/// adapter; [`NoopLedgerMetrics`] is the default for tests / pre-init.
pub trait LedgerMetricsPort: Send + Sync + 'static {
    /// Increment the invoice-post counter for one post attempt, labelled by
    /// outcome + flow (`ledger_invoice_post_total{result,flow}`).
    fn invoice_post(&self, result: PostResult, flow: PostFlow);
    /// Record one end-to-end post latency sample, labelled by flow
    /// (`ledger_invoice_post_duration_seconds{flow}`).
    fn invoice_post_duration(&self, secs: f64, flow: PostFlow);
    /// Increment the payment-settle counter for one settle attempt, labelled by
    /// outcome (`ledger_payment_settle_total{result}`).
    fn payment_settle(&self, result: PostResult);
    /// Increment the settlement-return counter for one return attempt, labelled
    /// by outcome (`ledger_settlement_return_total{result}`).
    fn settlement_return(&self, result: PostResult);
    /// Increment the chargeback counter for one dispute-phase attempt, labelled
    /// by outcome (`ledger_chargeback_total{result}`).
    fn chargeback(&self, result: PostResult);
    /// Increment the allocation counter for one allocate attempt, labelled by
    /// outcome (`ledger_allocation_total{result}`).
    fn allocation(&self, result: PostResult);
    /// Record the current deferred-apply queue depth for the `PAYMENT_ALLOCATE`
    /// flow (`ledger_allocation_queue_depth`) — the count of rows still `QUEUED`,
    /// observed by the sweep job each tick. A gauge: it reflects the live backlog
    /// (rises as unsettled allocations queue, falls as the drain applies them).
    fn allocation_queue_depth(&self, depth: i64);
    /// Increment the reusable-credit counter for one grant/apply attempt,
    /// labelled by outcome (`ledger_credit_application_total{result}`).
    fn credit_application(&self, result: PostResult);
    /// Record one end-to-end payment-post latency sample, labelled by flow
    /// (`ledger_payment_post_duration_seconds{flow}`).
    fn payment_post_duration(&self, secs: f64, flow: PostFlow);
    /// Record the live suspense backlog for a tenant: the pending-line count
    /// (`ledger_suspense_pending_lines{tenant}`) and the age of the oldest
    /// pending line in seconds (`ledger_suspense_pending_age_seconds{tenant}`).
    fn suspense_pending(&self, tenant: Uuid, lines: i64, oldest_age_secs: f64);
    /// Increment the §9 catalog-wide alarm rollup
    /// (`ledger_alarm_total{category,severity}`). Mirrors the durable alarm event
    /// so the alarm is observable even when the broker is absent; the §4.7
    /// tamper-failure count is this counter filtered to
    /// `category="TAMPER_VERIFY_FAILED"`.
    fn invariant_alarm(&self, category: &str, severity: &str);
    /// Record one recognition-run pass latency sample
    /// (`ledger_recognition_run_duration_seconds`) — the wall time one
    /// `RecognitionRunService::trigger` pass (release the period's due segments)
    /// took (design §9). Unlabelled: a run covers one `(tenant, period)`.
    fn recognition_run_duration(&self, secs: f64);
    /// Increment the recognized-revenue counter by `amount_minor` for one
    /// released segment, labelled by `revenue_stream`
    /// (`ledger_revenue_recognized_minor{stream}`, design §9) — the minor-unit
    /// amount moved `CONTRACT_LIABILITY → REVENUE` this release. The stream label
    /// is the schedule's revenue stream (bounded cardinality — a closed set per
    /// deployment).
    fn revenue_recognized_minor(&self, amount_minor: i64, revenue_stream: &str);
    /// Increment the over-recognition counter
    /// (`ledger_over_recognition_total`, design §9) — one release rejected by the
    /// per-schedule `recognized_minor <= total_deferred_minor` cap CHECK. Paired
    /// with the `OVER_RECOGNITION` invariant alarm.
    fn over_recognition(&self);
    /// Increment the recognition-double-credit counter
    /// (`ledger_recognition_double_credit_total`, design §9) — one detected
    /// attempt to re-credit an already-released segment (the at-most-once stamp
    /// guard tripped). Paired with the `RECOGNITION_DOUBLE_CREDIT` alarm.
    fn recognition_double_credit(&self);
    /// Record the current recognition-period queue depth
    /// (`ledger_recognition_period_queue_depth`, design §9) — the count of
    /// segments parked `QUEUED` (a predecessor period not yet `DONE`) observed by
    /// a run pass. A gauge: rises as out-of-order segments park, falls as later
    /// runs drain them.
    fn recognition_period_queue_depth(&self, depth: i64);
    /// Increment the dual-control pending-created counter, labelled by `kind`
    /// (`ledger_dual_control_pending_total{kind}`) — an over-threshold mutation
    /// routed to the preparer→approver queue (VHP-1852).
    fn dual_control_pending(&self, kind: &str);
    /// Increment the dual-control decision counter, labelled by `kind` +
    /// `decision` (approved / rejected / `needs_rework` / cancelled / expired)
    /// (`ledger_dual_control_decided_total{kind,decision}`).
    fn dual_control_decided(&self, kind: &str, decision: &str);
    /// Increment the self-approval-denied counter, labelled by `kind`
    /// (`ledger_dual_control_self_approval_denied_total{kind}`) — a
    /// `preparer == approver` attempt (a fraud / mis-config signal).
    fn dual_control_self_approval_denied(&self, kind: &str);
    /// Record the count of approvals currently in the transient `APPROVING` latch
    /// (`ledger_dual_control_approving`, Z8-1). A healthy approve clears the latch
    /// within one txn; a value that stays `> 0` across maintenance ticks is a
    /// crash-stranded approve (excluded from the TTL sweep, still holding the
    /// active-uniqueness slot) that needs a manual re-approve. Recorded by the
    /// dual-control sweep on its maintenance cadence.
    fn dual_control_approving(&self, count: i64);

    // ── §9 feature metrics ───────────────────────────────────────────────────
    /// Record one chain-Verifier run for a tenant (§9): increments
    /// `ledger_tamper_verify_runs_total` always, and
    /// `ledger_tamper_verify_failures_total` when `failed` (the walk found a
    /// break). `failures/runs` is the break rate.
    fn tamper_verify_run(&self, tenant: Uuid, failed: bool);
    /// Set the observed tamper-chain length for a tenant (§9):
    /// `ledger_tamper_chain_length{tenant}` (gauge).
    fn chain_length(&self, tenant: Uuid, length: i64);
    /// Set the count of active scope freezes for a tenant (§9):
    /// `ledger_scope_freeze_active{tenant}` (gauge).
    fn scope_freeze_active(&self, tenant: Uuid, active: i64);
    /// Increment the cross-tenant audit-access counter (§9):
    /// `ledger_cross_tenant_access_total{reason_code}`. `reason_code` is a
    /// bounded forensic token (mirrors `invariant_alarm`'s `&str` params).
    fn cross_tenant_access(&self, reason_code: &str);
    /// Increment the forensic re-identification counter (§9):
    /// `ledger_reidentification_total`.
    fn reidentification(&self);
    /// Increment the GDPR right-to-erasure counter (§9):
    /// `ledger_erasure_applied_total`.
    fn erasure_applied(&self);
    /// Increment the controlled-metadata-change counter (§9):
    /// `ledger_metadata_change_total{attribute}`. `attribute` is a closed
    /// allow-list token (mirrors `invariant_alarm`'s `&str` params).
    fn metadata_change(&self, attribute: &str);
    /// Record one audit-pack CSV export latency sample in seconds (§9):
    /// `ledger_audit_pack_export_duration_seconds` (histogram).
    fn audit_pack_export_duration(&self, secs: f64);
    /// Increment the credit-note counter for one post attempt, labelled by
    /// outcome (`ledger_credit_note_total{outcome}`, Slice 3 §4.2 / Group F) —
    /// `posted` / `replayed` on success, or `blocked_split` / `blocked_headroom`
    /// / `rejected` on a rejection.
    fn credit_note(&self, outcome: NoteOutcome);
    /// Increment the debit-note counter for one post attempt, labelled by outcome
    /// (`ledger_debit_note_total{outcome}`, Slice 3 §4.3 / Group F) — `posted` /
    /// `replayed` on success, or `rejected` on a rejection (a debit note has no
    /// split / headroom block).
    fn debit_note(&self, outcome: NoteOutcome);
    /// Increment the refund counter for one successfully-POSTED refund phase,
    /// labelled by `phase` + `pattern` (`ledger_refund_total{phase,pattern}`,
    /// Slice 3 §4.4 / §9 / Group G). One increment per FRESH refund post (never on
    /// replay) — every advanced phase (`initiated` / `confirmed` / `rejected` /
    /// `voided` / `unknown_final`). `phase` is the [`RefundPhase`] wire literal;
    /// `pattern` is the [`RefundPattern`] wire literal (`A_UNALLOCATED` /
    /// `B_RESTORE_AR`). Bounded cardinality (5 × 2).
    ///
    /// [`RefundPhase`]: crate::domain::adjustment::refund::RefundPhase
    /// [`RefundPattern`]: crate::domain::adjustment::refund::RefundPattern
    fn refund(&self, phase: &str, pattern: &str);
    /// Record the current refund-quarantine queue depth
    /// (`ledger_refund_quarantine_depth`, Slice 3 §4.4 / §9 / Group G) — the count
    /// of refund-before-payment rows still `QUEUED` on the `REFUND_QUARANTINE`
    /// flow, observed by the sweep job each tick. A gauge: rises as
    /// refund-before-payment requests quarantine, falls as the de-quarantine drain
    /// resolves them (post / approval / give-up). Unlabelled (cross-tenant total,
    /// mirroring `allocation_queue_depth`).
    fn refund_quarantine_depth(&self, depth: i64);
    /// Increment the refund `unknown_final` disposition counter
    /// (`ledger_refund_unknown_final_total`, Slice 3 §4.4 / §9 / K-1) — one
    /// terminal ledger-side dual-control disposition that cleared a stuck
    /// `REFUND_CLEARING` to a documented loss line + wrote a secured-audit record.
    /// Unlabelled (a rare governed event). Paired with the secured-audit append.
    fn refund_unknown_final(&self);
    /// Record the current open `REFUND_CLEARING` balance for a tenant
    /// (`ledger_refund_clearing_balance_minor{tenant}` gauge, Slice 3 §9) — the
    /// summed unsettled clearing the `AgedAlarmJob` observes each tick. Rises as
    /// stage-1 refunds initiate, falls as they settle / reverse / are disposed.
    fn refund_clearing_balance_minor(&self, tenant: Uuid, balance_minor: i64);
    /// Record the age (seconds) of the oldest open `REFUND_CLEARING` balance for a
    /// tenant (`ledger_refund_clearing_aged_seconds{tenant}` gauge, Slice 3 §9) —
    /// the worst-case clearing latency the aging alarm thresholds (7d/14d) gate.
    fn refund_clearing_aged_seconds(&self, tenant: Uuid, age_secs: f64);
    /// Increment the stage-1-refund-orphan counter
    /// (`ledger_stage1_refund_orphan_total`, Slice 3 §4.4 / §9) — one stage-1
    /// refund with no matching stage-2 / reversal beyond the aging threshold,
    /// paged to Revenue Assurance. Unlabelled; re-counted each tick the orphan
    /// persists (a latency signal, like the aged-allocation count).
    fn stage1_refund_orphan(&self);
    /// Record one unrealized-revaluation run-pass latency sample in seconds
    /// (Slice 5 Phase 3 / §9): `ledger_fx_revaluation_duration_seconds`
    /// (histogram) — the wall time one `RevaluationRunJob` tick took across the
    /// in-scope grains (the close-window cost, NFR ≤ 30 min).
    fn fx_revaluation_duration(&self, secs: f64);
    /// Increment the FX provider-fallback counter (Slice 5 Phase 3 / §9):
    /// `ledger_fx_provider_fallback_total{provider}` — one lock-time rate resolve
    /// that fell to a lower-priority provider. `provider` is the resolved provider
    /// id (bounded cardinality — the configured `provider_order`).
    fn fx_provider_fallback(&self, provider: &str);
    /// Increment the realized-FX counter by `amount_minor` (Slice 5 Phase 2 / §9):
    /// `ledger_fx_realized_minor{functional_currency,direction}` — the functional
    /// magnitude of a net `FX_GAIN_LOSS` line posted on a cross-currency allocation
    /// close. `amount_minor` is the non-negative magnitude; `direction` (`"gain"` /
    /// `"loss"`) carries the sign-by-role (the spec names `{functional_currency}`;
    /// the `direction` label makes a realized gain distinguishable from a loss in
    /// the one monotonic counter). Both labels are bounded cardinality (the
    /// functional currency is per-tenant; direction is a two-value closed set).
    fn fx_realized_minor(&self, amount_minor: i64, functional_currency: &str, direction: &str);

    // ── Slice 7 Phase 3 reconciliation ─────────────────────────────────────────
    /// Record the signed variance (minor units) observed by one reconciliation
    /// check run, labelled by `check_type`
    /// (`ledger_reconciliation_variance_minor{check_type}`, design §9 / spec §3.5
    /// J4). A gauge: each run records its own signed observed value (a tie-out can
    /// be positive or negative). `check_type` is the closed reconciliation-check
    /// kind (bounded cardinality — a fixed set of control feeds).
    fn reconciliation_variance_minor(&self, check_type: &str, variance_minor: i64);
    /// Increment the reconciliation-run counter for one check pass, labelled by
    /// `check_type` (`ledger_reconciliation_runs_total{check_type}`, design §9 /
    /// spec §3.5 J4) — one increment per reconciliation check executed.
    fn reconciliation_run(&self, check_type: &str);
    /// Increment the out-of-tolerance counter for one reconciliation check whose
    /// variance breached its configured tolerance, labelled by `check_type`
    /// (`ledger_reconciliation_out_of_tolerance_total{check_type}`, design §9 /
    /// spec §3.5 J4). `out_of_tolerance/runs` is the breach rate per check type.
    fn reconciliation_out_of_tolerance(&self, check_type: &str);
    /// Increment the period-close-blocked counter for one close attempt rejected
    /// by a pre-close gate, labelled by `reason`
    /// (`ledger_period_close_blocked_total{reason}`, design §9 / spec §3.5 J4) —
    /// `reason` is the close-gate rejection token (bounded cardinality).
    fn period_close_blocked(&self, reason: &str);
    /// Record the current exception-queue depth, labelled by `type`
    /// (`ledger_exception_queue_depth{type}`, design §9 / spec §3.5 J4) — the count
    /// of rows still open on the exception queue for the given exception type,
    /// observed by the sweep each tick. A gauge: rises as exceptions route in,
    /// falls as they are resolved.
    fn exception_queue_depth(&self, exception_type: &str, depth: i64);
}

/// No-op metrics. Used by unit tests and any construction before an exporter is
/// wired.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopLedgerMetrics;

impl LedgerMetricsPort for NoopLedgerMetrics {
    fn invoice_post(&self, _: PostResult, _: PostFlow) {}
    fn invoice_post_duration(&self, _: f64, _: PostFlow) {}
    fn payment_settle(&self, _: PostResult) {}
    fn settlement_return(&self, _: PostResult) {}
    fn chargeback(&self, _: PostResult) {}
    fn allocation(&self, _: PostResult) {}
    fn allocation_queue_depth(&self, _: i64) {}
    fn credit_application(&self, _: PostResult) {}
    fn payment_post_duration(&self, _: f64, _: PostFlow) {}
    fn suspense_pending(&self, _: Uuid, _: i64, _: f64) {}
    fn invariant_alarm(&self, _: &str, _: &str) {}
    fn recognition_run_duration(&self, _: f64) {}
    fn revenue_recognized_minor(&self, _: i64, _: &str) {}
    fn over_recognition(&self) {}
    fn recognition_double_credit(&self) {}
    fn recognition_period_queue_depth(&self, _: i64) {}
    fn dual_control_pending(&self, _: &str) {}
    fn dual_control_decided(&self, _: &str, _: &str) {}
    fn dual_control_self_approval_denied(&self, _: &str) {}
    fn dual_control_approving(&self, _: i64) {}
    fn tamper_verify_run(&self, _: Uuid, _: bool) {}
    fn chain_length(&self, _: Uuid, _: i64) {}
    fn scope_freeze_active(&self, _: Uuid, _: i64) {}
    fn cross_tenant_access(&self, _: &str) {}
    fn reidentification(&self) {}
    fn erasure_applied(&self) {}
    fn metadata_change(&self, _: &str) {}
    fn audit_pack_export_duration(&self, _: f64) {}
    fn credit_note(&self, _: NoteOutcome) {}
    fn debit_note(&self, _: NoteOutcome) {}
    fn refund(&self, _: &str, _: &str) {}
    fn refund_quarantine_depth(&self, _: i64) {}
    fn refund_unknown_final(&self) {}
    fn refund_clearing_balance_minor(&self, _: Uuid, _: i64) {}
    fn refund_clearing_aged_seconds(&self, _: Uuid, _: f64) {}
    fn stage1_refund_orphan(&self) {}
    fn fx_revaluation_duration(&self, _: f64) {}
    fn fx_provider_fallback(&self, _: &str) {}
    fn fx_realized_minor(&self, _: i64, _: &str, _: &str) {}
    fn reconciliation_variance_minor(&self, _: &str, _: i64) {}
    fn reconciliation_run(&self, _: &str) {}
    fn reconciliation_out_of_tolerance(&self, _: &str) {}
    fn period_close_blocked(&self, _: &str) {}
    fn exception_queue_depth(&self, _: &str, _: i64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_result_label_strings_are_snake_case() {
        assert_eq!(PostResult::Posted.as_str(), "posted");
        assert_eq!(PostResult::Replayed.as_str(), "replayed");
        assert_eq!(PostResult::Rejected.as_str(), "rejected");
    }

    #[test]
    fn post_flow_label_strings_are_snake_case() {
        assert_eq!(PostFlow::InvoicePost.as_str(), "invoice_post");
        assert_eq!(PostFlow::Reversal.as_str(), "reversal");
        assert_eq!(PostFlow::MappingCorrection.as_str(), "mapping_correction");
    }

    #[test]
    fn payment_post_flow_label_strings_are_snake_case() {
        assert_eq!(PostFlow::Settle.as_str(), "settle");
        assert_eq!(PostFlow::Allocate.as_str(), "allocate");
        assert_eq!(PostFlow::CreditApply.as_str(), "credit_apply");
        assert_eq!(PostFlow::SettlementReturn.as_str(), "settlement_return");
        assert_eq!(PostFlow::Chargeback.as_str(), "chargeback");
    }

    #[test]
    fn note_outcome_label_strings_are_snake_case() {
        assert_eq!(NoteOutcome::Posted.as_str(), "posted");
        assert_eq!(NoteOutcome::Replayed.as_str(), "replayed");
        assert_eq!(NoteOutcome::Rejected.as_str(), "rejected");
        assert_eq!(NoteOutcome::BlockedSplit.as_str(), "blocked_split");
        assert_eq!(NoteOutcome::BlockedHeadroom.as_str(), "blocked_headroom");
    }

    // Exercise every `NoopLedgerMetrics` sink method: the no-op bodies are the
    // safe default wired in unit tests / pre-exporter construction, so they must
    // accept every signal without panicking. Also pins the `LedgerMetricsPort`
    // contract surface (a new method forces this call site to compile).
    #[test]
    fn noop_metrics_accepts_every_signal_without_panicking() {
        let m = NoopLedgerMetrics;
        let t = Uuid::now_v7();
        m.invoice_post(PostResult::Posted, PostFlow::InvoicePost);
        m.invoice_post_duration(1.0, PostFlow::Reversal);
        m.payment_settle(PostResult::Replayed);
        m.settlement_return(PostResult::Rejected);
        m.chargeback(PostResult::Posted);
        m.allocation(PostResult::Posted);
        m.allocation_queue_depth(5);
        m.credit_application(PostResult::Posted);
        m.payment_post_duration(0.5, PostFlow::Settle);
        m.suspense_pending(t, 3, 10.0);
        m.invariant_alarm("TAMPER_VERIFY_FAILED", "critical");
        m.recognition_run_duration(0.1);
        m.revenue_recognized_minor(100, "subscription");
        m.over_recognition();
        m.recognition_double_credit();
        m.recognition_period_queue_depth(2);
        m.dual_control_pending("reverse");
        m.dual_control_decided("reverse", "approved");
        m.dual_control_self_approval_denied("reverse");
        m.dual_control_approving(1);
        m.tamper_verify_run(t, false);
        m.tamper_verify_run(t, true);
        m.chain_length(t, 10);
        m.scope_freeze_active(t, 0);
        m.cross_tenant_access("INVESTIGATION");
        m.reidentification();
        m.erasure_applied();
        m.metadata_change("payer_phone");
        m.audit_pack_export_duration(2.0);
        m.credit_note(NoteOutcome::BlockedSplit);
        m.debit_note(NoteOutcome::Rejected);
        m.refund("initiated", "A_UNALLOCATED");
        m.refund_quarantine_depth(0);
        m.refund_unknown_final();
        m.refund_clearing_balance_minor(t, 1_000);
        m.refund_clearing_aged_seconds(t, 3_600.0);
        m.stage1_refund_orphan();
        m.fx_revaluation_duration(1.5);
        m.fx_provider_fallback("ecb");
        m.fx_realized_minor(240, "USD", "loss");
    }

    // `NoopLedgerMetrics` is the trait's safe default — usable behind the
    // `Arc<dyn LedgerMetricsPort>` the services hold. Covers the dyn-dispatch path.
    #[test]
    fn noop_metrics_is_usable_as_dyn_port() {
        let m: std::sync::Arc<dyn LedgerMetricsPort> = std::sync::Arc::new(NoopLedgerMetrics);
        m.invoice_post(PostResult::Rejected, PostFlow::Chargeback);
        m.fx_realized_minor(0, "EUR", "gain");
    }
}
