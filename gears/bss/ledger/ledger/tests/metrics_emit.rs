//! §9 feature-metric emission tests (Slice 6 Phase 4 Group 4C).
//!
//! A NON-ignored, NO-Docker integration test that drives the OTel metrics
//! adapter (`LedgerMetricsMeter`) through the in-memory `MetricsHarness` and
//! asserts each §9 instrument records the right metric + labels. This proves the
//! metric LAYER independently of the services that will later emit through it
//! (most of which are wired only via documented seams in this group).
#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::too_many_lines
)]

use bss_ledger::domain::ports::metrics::LedgerMetricsPort;
use bss_ledger::infra::metrics::test_harness::MetricsHarness;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222);

#[test]
fn tamper_verify_run_counts_runs_and_failures() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.tamper_verify_run(TENANT, false);
    m.tamper_verify_run(TENANT, true);
    m.tamper_verify_run(TENANT, false);
    h.force_flush();
    assert_eq!(
        h.counter_value("ledger_tamper_verify_runs_total", &[]),
        3,
        "every run counts under the runs counter"
    );
    assert_eq!(
        h.counter_value("ledger_tamper_verify_failures_total", &[]),
        1,
        "only the break counts under the failures counter"
    );
}

#[test]
fn chain_length_gauge_carries_tenant() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.chain_length(TENANT, 7);
    h.force_flush();
    assert_eq!(
        h.gauge_value(
            "ledger_tamper_chain_length",
            &[("tenant", &TENANT.to_string())]
        ),
        7
    );
}

#[test]
fn scope_freeze_active_gauge_carries_tenant() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.scope_freeze_active(TENANT, 1);
    h.force_flush();
    assert_eq!(
        h.gauge_value(
            "ledger_scope_freeze_active",
            &[("tenant", &TENANT.to_string())]
        ),
        1
    );
}

#[test]
fn cross_tenant_access_counter_carries_reason_code() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.cross_tenant_access("DISPUTE");
    m.cross_tenant_access("DISPUTE");
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "ledger_cross_tenant_access_total",
            &[("reason_code", "DISPUTE")]
        ),
        2
    );
}

#[test]
fn reidentification_and_erasure_counters_increment() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.reidentification();
    m.erasure_applied();
    m.erasure_applied();
    h.force_flush();
    assert_eq!(h.counter_value("ledger_reidentification_total", &[]), 1);
    assert_eq!(h.counter_value("ledger_erasure_applied_total", &[]), 2);
}

#[test]
fn metadata_change_counter_carries_attribute() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.metadata_change("memo");
    h.force_flush();
    assert_eq!(
        h.counter_value("ledger_metadata_change_total", &[("attribute", "memo")]),
        1
    );
}

#[test]
fn audit_pack_export_duration_records_a_sample() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.audit_pack_export_duration(0.5);
    h.force_flush();
    assert_eq!(
        h.histogram_count("ledger_audit_pack_export_duration_seconds", &[]),
        1
    );
}
