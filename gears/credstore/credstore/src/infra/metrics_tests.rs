//! Unit tests for the OpenTelemetry-backed [`CredStoreMetricsMeter`].

#[cfg(feature = "test-support")]
use super::test_harness::MetricsHarness;
#[cfg(feature = "test-support")]
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};

/// Smoke test that exercises instrument construction and every recording
/// path against the global (no-op) meter — no SDK exporter required, so it
/// runs under the default feature set (unlike the `test-support` tests).
#[test]
fn global_meter_records_all_instruments() {
    use super::CredStoreMetricsMeter;
    use crate::domain::ports::metrics::{
        CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
    };

    let m = CredStoreMetricsMeter::from_global();
    assert!(!format!("{m:?}").is_empty());

    for outcome in [
        ReadOutcome::HitOwn,
        ReadOutcome::HitInherited,
        ReadOutcome::Miss,
        ReadOutcome::Expired,
    ] {
        m.read_outcome(outcome);
    }
    m.walkup_depth(2);
    m.dependency(Dep::Plugin, DepOp::PluginGet, Outcome::Success, 0.01);
    m.dependency(Dep::Pdp, DepOp::Evaluate, Outcome::Error, 0.02);
    m.cross_tenant_denied();
    m.destroy_failed();
    m.outbox_purge_failed();
    m.read_retry(ReadRetryOutcome::Recovered);
    m.read_retry(ReadRetryOutcome::SecondMiss);
    m.list_type_invariant_violation();
    m.audit_publish_failed();
}

#[test]
#[cfg(feature = "test-support")]
fn read_outcome_emits_with_label() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.read_outcome(ReadOutcome::HitInherited);
    m.read_outcome(ReadOutcome::Miss);
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "credstore_read_outcome_total",
            &[("outcome", "hit_inherited")]
        ),
        1
    );
    assert_eq!(
        h.counter_value("credstore_read_outcome_total", &[("outcome", "miss")]),
        1
    );
}

#[test]
#[cfg(feature = "test-support")]
fn dependency_emits_duration_and_health() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.dependency(Dep::Plugin, DepOp::PluginGet, Outcome::Success, 0.005);
    h.force_flush();
    assert_eq!(
        h.histogram_count(
            "credstore_dependency_query_duration_seconds",
            &[("dependency", "plugin"), ("operation", "plugin_get")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "credstore_dependency_health_total",
            &[
                ("dependency", "plugin"),
                ("operation", "plugin_get"),
                ("outcome", "success")
            ]
        ),
        1
    );
}

#[test]
#[cfg(feature = "test-support")]
fn read_retry_emits_with_outcome_labels() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.read_retry(ReadRetryOutcome::Recovered);
    m.read_retry(ReadRetryOutcome::SecondMiss);
    m.read_retry(ReadRetryOutcome::SecondMiss);
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_read_retry_total", &[("outcome", "recovered")]),
        1
    );
    assert_eq!(
        h.counter_value("credstore_read_retry_total", &[("outcome", "second_miss")]),
        2
    );
}

#[test]
#[cfg(feature = "test-support")]
fn destroy_and_purge_failure_counters_accumulate_independently() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.destroy_failed();
    m.destroy_failed();
    m.outbox_purge_failed();
    h.force_flush();
    assert_eq!(h.counter_value("credstore_destroy_failed_total", &[]), 2);
    assert_eq!(
        h.counter_value("credstore_outbox_purge_failed_total", &[]),
        1
    );
}

#[test]
#[cfg(feature = "test-support")]
fn list_type_invariant_violation_accumulates() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.list_type_invariant_violation();
    m.list_type_invariant_violation();
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_list_type_invariant_violation_total", &[]),
        2
    );
}

#[test]
#[cfg(feature = "test-support")]
fn audit_publish_failed_accumulates() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.audit_publish_failed();
    m.audit_publish_failed();
    m.audit_publish_failed();
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_audit_publish_failed_total", &[]),
        3
    );
}
