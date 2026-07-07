use super::test_harness::MetricsHarness;
use super::*;

#[test]
fn invoice_post_increments_counter_and_records_duration() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.invoice_post(PostResult::Posted, PostFlow::InvoicePost);
    m.invoice_post_duration(0.01, PostFlow::InvoicePost);
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_invoice_post_total",
            &[("result", "posted"), ("flow", "invoice_post")]
        ),
        1
    );
    assert_eq!(
        h.histogram_count(
            "ledger_invoice_post_duration_seconds",
            &[("flow", "invoice_post")]
        ),
        1
    );
}

#[test]
fn reversal_flow_is_counted_separately_from_invoice_post() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.invoice_post(PostResult::Posted, PostFlow::Reversal);
    m.invoice_post_duration(0.01, PostFlow::Reversal);
    h.force_flush();
    // The reversal does NOT count under the invoice_post flow.
    assert_eq!(
        h.counter_value(
            "ledger_invoice_post_total",
            &[("result", "posted"), ("flow", "invoice_post")]
        ),
        0
    );
    assert_eq!(
        h.counter_value(
            "ledger_invoice_post_total",
            &[("result", "posted"), ("flow", "reversal")]
        ),
        1
    );
}

#[test]
fn payment_settle_and_allocation_counters_and_duration_are_observable() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.payment_settle(PostResult::Posted);
    m.allocation(PostResult::Posted);
    m.credit_application(PostResult::Posted);
    m.payment_post_duration(0.01, PostFlow::Settle);
    m.payment_post_duration(0.02, PostFlow::Allocate);
    m.payment_post_duration(0.03, PostFlow::CreditApply);
    h.force_flush();
    assert_eq!(
        h.counter_value("ledger_payment_settle_total", &[("result", "posted")]),
        1
    );
    assert_eq!(
        h.counter_value("ledger_allocation_total", &[("result", "posted")]),
        1
    );
    assert_eq!(
        h.counter_value("ledger_credit_application_total", &[("result", "posted")]),
        1
    );
    assert_eq!(
        h.histogram_count(
            "ledger_payment_post_duration_seconds",
            &[("flow", "settle")]
        ),
        1
    );
    assert_eq!(
        h.histogram_count(
            "ledger_payment_post_duration_seconds",
            &[("flow", "allocate")]
        ),
        1
    );
    assert_eq!(
        h.histogram_count(
            "ledger_payment_post_duration_seconds",
            &[("flow", "credit_apply")]
        ),
        1
    );
}

#[test]
fn invariant_alarm_counter_carries_category_and_severity() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.invariant_alarm("TIE_OUT_VARIANCE", "CRITICAL");
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_alarm_total",
            &[("category", "TIE_OUT_VARIANCE"), ("severity", "CRITICAL")]
        ),
        1
    );
}

#[test]
fn recognition_run_metrics_are_observable() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.recognition_run_duration(0.02);
    m.revenue_recognized_minor(1_200, "subscription");
    m.over_recognition();
    m.recognition_double_credit();
    h.force_flush();
    assert_eq!(
        h.histogram_count("ledger_recognition_run_duration_seconds", &[]),
        1
    );
    // The recognized-minor counter sums the released amount under the stream label.
    assert_eq!(
        h.counter_value(
            "ledger_revenue_recognized_minor",
            &[("stream", "subscription")]
        ),
        1_200
    );
    assert_eq!(h.counter_value("ledger_over_recognition_total", &[]), 1);
    assert_eq!(
        h.counter_value("ledger_recognition_double_credit_total", &[]),
        1
    );
}

#[test]
fn bounded_reason_code_buckets_unknown_to_other() {
    // Known codes pass through (case-insensitive, canonicalized).
    assert_eq!(
        bounded_reason_code("DISPUTE_INVESTIGATION"),
        "DISPUTE_INVESTIGATION"
    );
    assert_eq!(
        bounded_reason_code("dispute_investigation"),
        "DISPUTE_INVESTIGATION"
    );
    assert_eq!(bounded_reason_code("  fraud  "), "FRAUD");
    // Anything else (incl. a caller's high-cardinality junk) buckets to "other".
    assert_eq!(bounded_reason_code("attacker-supplied-uuid-1"), "other");
    assert_eq!(bounded_reason_code(""), "other");
}

#[test]
fn cross_tenant_access_label_is_bounded() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.cross_tenant_access("totally-unbounded-12345");
    m.cross_tenant_access("DISPUTE");
    h.force_flush();
    // The junk code is bucketed; only "other" + the known "DISPUTE" appear.
    assert_eq!(
        h.counter_value(
            "ledger_cross_tenant_access_total",
            &[("reason_code", "other")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "ledger_cross_tenant_access_total",
            &[("reason_code", "DISPUTE")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "ledger_cross_tenant_access_total",
            &[("reason_code", "totally-unbounded-12345")]
        ),
        0
    );
}

#[test]
fn credit_and_debit_note_counters_are_observable_by_outcome() {
    use crate::domain::ports::metrics::NoteOutcome;
    let h = MetricsHarness::new();
    let m = h.metrics();
    // Credit-note: a posted + the two block reasons each get their own outcome.
    m.credit_note(NoteOutcome::Posted);
    m.credit_note(NoteOutcome::BlockedSplit);
    m.credit_note(NoteOutcome::BlockedHeadroom);
    // Debit-note: a posted + a rejected (a debit note has no block reasons).
    m.debit_note(NoteOutcome::Posted);
    m.debit_note(NoteOutcome::Rejected);
    h.force_flush();
    assert_eq!(
        h.counter_value("ledger_credit_note_total", &[("outcome", "posted")]),
        1
    );
    assert_eq!(
        h.counter_value("ledger_credit_note_total", &[("outcome", "blocked_split")]),
        1
    );
    assert_eq!(
        h.counter_value(
            "ledger_credit_note_total",
            &[("outcome", "blocked_headroom")]
        ),
        1
    );
    assert_eq!(
        h.counter_value("ledger_debit_note_total", &[("outcome", "posted")]),
        1
    );
    assert_eq!(
        h.counter_value("ledger_debit_note_total", &[("outcome", "rejected")]),
        1
    );
}

#[test]
fn refund_group_f_counters_are_observable() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    let tenant = uuid::Uuid::now_v7();
    // The unknown_final disposition + a stage-1 orphan are bare counters; the
    // clearing balance/age are per-tenant gauges (recorded, not summed here).
    m.refund_unknown_final();
    m.refund_unknown_final();
    m.stage1_refund_orphan();
    m.refund_clearing_balance_minor(tenant, 500);
    m.refund_clearing_aged_seconds(tenant, 700_000.0);
    h.force_flush();
    assert_eq!(h.counter_value("ledger_refund_unknown_final_total", &[]), 2);
    assert_eq!(h.counter_value("ledger_stage1_refund_orphan_total", &[]), 1);
}

#[test]
fn refund_group_g_counter_is_labelled_by_phase_and_pattern() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    // `ledger_refund_total{phase,pattern}` — one increment per fresh refund post.
    m.refund("initiated", "A_UNALLOCATED");
    m.refund("initiated", "A_UNALLOCATED");
    m.refund("confirmed", "B_RESTORE_AR");
    // The quarantine-depth gauge (recorded, not summed).
    m.refund_quarantine_depth(3);
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_refund_total",
            &[("phase", "initiated"), ("pattern", "A_UNALLOCATED")],
        ),
        2,
    );
    assert_eq!(
        h.counter_value(
            "ledger_refund_total",
            &[("phase", "confirmed"), ("pattern", "B_RESTORE_AR")],
        ),
        1,
    );
}

#[test]
fn reconciliation_slice7_metrics_are_observable() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    // Two runs of the same check type; one breaches tolerance. The variance gauge
    // records the latest signed observed value (last write wins per attribute set).
    m.reconciliation_run("ar_subledger_vs_gl");
    m.reconciliation_run("ar_subledger_vs_gl");
    m.reconciliation_out_of_tolerance("ar_subledger_vs_gl");
    m.reconciliation_variance_minor("ar_subledger_vs_gl", -1_500);
    // A blocked close (by reason) + an exception-queue depth (by type) gauge.
    m.period_close_blocked("open_exceptions");
    m.exception_queue_depth("unmatched_settlement", 4);
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_reconciliation_runs_total",
            &[("check_type", "ar_subledger_vs_gl")]
        ),
        2
    );
    assert_eq!(
        h.counter_value(
            "ledger_reconciliation_out_of_tolerance_total",
            &[("check_type", "ar_subledger_vs_gl")]
        ),
        1
    );
    assert_eq!(
        h.gauge_value(
            "ledger_reconciliation_variance_minor",
            &[("check_type", "ar_subledger_vs_gl")]
        ),
        -1_500
    );
    assert_eq!(
        h.counter_value(
            "ledger_period_close_blocked_total",
            &[("reason", "open_exceptions")]
        ),
        1
    );
    assert_eq!(
        h.gauge_value(
            "ledger_exception_queue_depth",
            &[("type", "unmatched_settlement")]
        ),
        4
    );
}
