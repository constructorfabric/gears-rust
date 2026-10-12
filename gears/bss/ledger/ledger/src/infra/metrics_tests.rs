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
    m.revenue_recognized("subscription");
    m.revenue_recognized("subscription");
    m.revenue_recognized("usage");
    m.over_recognition();
    m.recognition_double_credit();
    h.force_flush();
    assert_eq!(
        h.histogram_count("ledger_recognition_run_duration_seconds", &[]),
        1
    );
    // Each call counts one release; streams remain separate.
    assert_eq!(
        h.counter_value(
            "ledger_revenue_recognized_total",
            &[("stream", "subscription")]
        ),
        2
    );
    assert_eq!(
        h.counter_value("ledger_revenue_recognized_total", &[("stream", "usage")]),
        1
    );
    assert_eq!(h.counter_value("ledger_revenue_recognized_total", &[]), 0);
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
    // clearing grain count/age are per-tenant gauges.
    m.refund_unknown_final();
    m.refund_unknown_final();
    m.stage1_refund_orphan();
    m.refund_clearing_open_grains(tenant, 3);
    m.refund_clearing_aged_seconds(tenant, 700_000.0);
    h.force_flush();
    assert_eq!(h.counter_value("ledger_refund_unknown_final_total", &[]), 2);
    assert_eq!(h.counter_value("ledger_stage1_refund_orphan_total", &[]), 1);
    let tenant_text = tenant.to_string();
    assert_eq!(
        h.count_gauge_value(
            "ledger_refund_clearing_open_grains",
            &[("tenant", &tenant_text)]
        ),
        Some(3)
    );
    assert_eq!(
        h.age_gauge_value(
            "ledger_refund_clearing_aged_seconds",
            &[("tenant", &tenant_text)]
        ),
        Some(700_000.0)
    );
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
    // Two runs of the same check type; one breaches tolerance. Money variance
    // counts nonzero currency buckets, separately from missing invoices.
    m.reconciliation_run("ar_subledger_vs_gl");
    m.reconciliation_run("ar_subledger_vs_gl");
    m.reconciliation_out_of_tolerance("ar_subledger_vs_gl");
    m.reconciliation_money_variance_currencies("ar_subledger_vs_gl", 2);
    m.reconciliation_missing_invoices(7);
    // A blocked close (by reason) + an exception-queue depth (by type) gauge.
    m.period_close_blocked("open_exceptions");
    m.exception_queue_depth("unmatched_settlement", 4);
    // The tenant-lifecycle gate: the skipped-tenant gauge by state (last write
    // wins), the purged-row counter (accumulates across adds), and the two
    // failure counters.
    m.reconciliation_retired_tenants("deleted", 9);
    m.reconciliation_retired_tenants("deleted", 7);
    m.reconciliation_retired_tenants("unregistered", 2);
    m.reconciliation_runs_purged(5_000);
    m.reconciliation_runs_purged(1_234);
    m.reconciliation_lifecycle_unavailable("none_recognised");
    m.reconciliation_lifecycle_unavailable("none_recognised");
    m.reconciliation_lifecycle_unavailable("read_failed");
    m.reconciliation_purge_failed(3);
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
        h.count_gauge_value(
            "ledger_reconciliation_money_variance_currencies",
            &[("check_type", "ar_subledger_vs_gl")]
        ),
        Some(2)
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
    assert_eq!(
        h.gauge_value(
            "ledger_reconciliation_retired_tenants",
            &[("state", "deleted")]
        ),
        7
    );
    assert_eq!(
        h.gauge_value(
            "ledger_reconciliation_retired_tenants",
            &[("state", "unregistered")]
        ),
        2
    );
    assert_eq!(
        h.counter_value("ledger_reconciliation_runs_purged_total", &[]),
        6_234
    );
    assert_eq!(
        h.counter_value(
            "ledger_reconciliation_lifecycle_unavailable_total",
            &[("reason", "none_recognised")]
        ),
        2
    );
    assert_eq!(
        h.counter_value(
            "ledger_reconciliation_lifecycle_unavailable_total",
            &[("reason", "read_failed")]
        ),
        1
    );
    assert_eq!(
        h.counter_value("ledger_reconciliation_purge_failed_total", &[]),
        3
    );
}

#[test]
fn fx_rate_sync_heartbeat_is_observable_and_unlabelled() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    // The job-liveness heartbeat: one increment per scheduler tick started. It is
    // the only signal that catches a stalled ticker — no fetch is attempted, so no
    // fetch error is produced and FX_SNAPSHOT_MISSING never fires. An alert reads
    // it as "no increase over 3 x fx.rate_sync_tick_secs", which only works if the
    // series exists and carries no attributes to split it.
    m.fx_rate_sync_ticked();
    m.fx_rate_sync_ticked();
    m.fx_rate_sync_ticked();
    h.force_flush();
    assert_eq!(h.counter_value("ledger_fx_rate_sync_ticks_total", &[]), 3);
}

#[test]
fn fx_rate_sync_duration_is_observable_and_unlabelled() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    // The whole-pass latency: the only series that can be compared against
    // `fx.rate_sync_tick_secs`, since the adapter's per-source histogram times one
    // HTTP round-trip while the composite tries its sources one after another.
    // Unlabelled for the same reason as the heartbeat it pairs with — the pass
    // spans discovery, so it belongs to no single provider.
    m.fx_rate_sync_duration(0.4);
    m.fx_rate_sync_duration(2.5);
    h.force_flush();
    assert_eq!(
        h.histogram_count("ledger_fx_rate_sync_duration_seconds", &[]),
        2
    );
}

#[test]
fn realized_fx_counts_postings_separately_by_currency_and_direction() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.fx_realized("USD", "gain");
    m.fx_realized("USD", "gain");
    m.fx_realized("USD", "loss");
    m.fx_realized("EUR", "gain");
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_fx_realized_total",
            &[("functional_currency", "USD"), ("direction", "gain")]
        ),
        2
    );
    assert_eq!(
        h.counter_value(
            "ledger_fx_realized_total",
            &[("functional_currency", "USD"), ("direction", "loss")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "ledger_fx_realized_total",
            &[("functional_currency", "EUR"), ("direction", "gain")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "ledger_fx_realized_total",
            &[("functional_currency", "EUR"), ("direction", "loss")]
        ),
        0
    );
}

#[test]
fn refund_open_grains_replace_and_reset_per_tenant() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    let first = uuid::Uuid::now_v7();
    let second = uuid::Uuid::now_v7();
    let first_text = first.to_string();
    let second_text = second.to_string();
    m.refund_clearing_open_grains(first, 3);
    m.refund_clearing_open_grains(first, 2);
    m.refund_clearing_open_grains(second, 4);
    h.force_flush();
    assert_eq!(
        h.count_gauge_value(
            "ledger_refund_clearing_open_grains",
            &[("tenant", &first_text)]
        ),
        Some(2)
    );
    assert_eq!(
        h.count_gauge_value(
            "ledger_refund_clearing_open_grains",
            &[("tenant", &second_text)]
        ),
        Some(4)
    );
    assert_eq!(
        h.count_gauge_value("ledger_refund_clearing_open_grains", &[]),
        None
    );
    m.refund_clearing_open_grains(first, 0);
    h.force_flush();
    assert_eq!(
        h.count_gauge_value(
            "ledger_refund_clearing_open_grains",
            &[("tenant", &first_text)]
        ),
        Some(0)
    );
    assert_eq!(
        h.count_gauge_value(
            "ledger_refund_clearing_open_grains",
            &[("tenant", &second_text)]
        ),
        Some(4)
    );
}

#[test]
fn reconciliation_currency_buckets_and_missing_invoices_replace_and_reset_independently() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    let name = "ledger_reconciliation_money_variance_currencies";
    let first = [("check_type", "ar_subledger_vs_gl")];
    let second = [("check_type", "psp_vs_clearing")];
    m.reconciliation_money_variance_currencies("ar_subledger_vs_gl", 3);
    m.reconciliation_money_variance_currencies("ar_subledger_vs_gl", 2);
    m.reconciliation_money_variance_currencies("psp_vs_clearing", 1);
    m.reconciliation_missing_invoices(8);
    m.reconciliation_missing_invoices(7);
    h.force_flush();
    assert_eq!(h.count_gauge_value(name, &first), Some(2));
    assert_eq!(h.count_gauge_value(name, &second), Some(1));
    assert_eq!(h.count_gauge_value(name, &[]), None);
    assert_eq!(
        h.count_gauge_value("ledger_reconciliation_missing_invoices", &[]),
        Some(7)
    );
    assert_eq!(
        h.count_gauge_value("ledger_reconciliation_missing_invoices", &first),
        None
    );
    m.reconciliation_money_variance_currencies("ar_subledger_vs_gl", 0);
    h.force_flush();
    assert_eq!(h.count_gauge_value(name, &first), Some(0));
    assert_eq!(h.count_gauge_value(name, &second), Some(1));
    assert_eq!(
        h.count_gauge_value("ledger_reconciliation_missing_invoices", &[]),
        Some(7)
    );
    m.reconciliation_missing_invoices(0);
    h.force_flush();
    assert_eq!(
        h.count_gauge_value("ledger_reconciliation_missing_invoices", &[]),
        Some(0)
    );
    assert_eq!(h.count_gauge_value(name, &second), Some(1));
}
