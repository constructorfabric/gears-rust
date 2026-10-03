//! Unit tests for the OpenTelemetry-backed [`CredStoreMetricsMeter`].

#[cfg(feature = "test-support")]
use super::test_harness::MetricsHarness;
#[cfg(feature = "test-support")]
use crate::domain::ports::metrics::{
    CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};

/// Smoke test that exercises instrument construction and every recording
/// path against the global (no-op) meter — no SDK exporter required, so it
/// runs under the default feature set (unlike the `test-support` tests).
#[test]
fn global_meter_records_all_instruments() {
    use super::CredStoreMetricsMeter;
    use crate::domain::ports::metrics::{
        CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
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
    m.write_intents_reclaimed(3);
    m.write_intent_lost();
    m.write_intent_reclaim_failed();
    m.store_cleanup_enqueued(CleanupOp::Purge);
    m.store_cleanup_enqueued(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Purge);
    m.store_cleanup_failed(CleanupOp::Destroy);
    m.read_retry(ReadRetryOutcome::Recovered);
    m.read_retry(ReadRetryOutcome::SecondMiss);
    m.list_type_invariant_violation();
    m.audit_publish_failed();
    m.secret_unreadable();
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
fn store_cleanup_counters_are_labelled_by_op() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.store_cleanup_enqueued(CleanupOp::Purge);
    m.store_cleanup_enqueued(CleanupOp::Destroy);
    m.store_cleanup_enqueued(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Purge);
    m.store_cleanup_failed(CleanupOp::Purge);
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_store_cleanup_enqueued_total", &[("op", "purge")]),
        1
    );
    assert_eq!(
        h.counter_value(
            "credstore_store_cleanup_enqueued_total",
            &[("op", "destroy")]
        ),
        2
    );
    assert_eq!(
        h.counter_value("credstore_store_cleanup_failed_total", &[("op", "destroy")]),
        1
    );
    assert_eq!(
        h.counter_value("credstore_store_cleanup_failed_total", &[("op", "purge")]),
        2
    );
}

#[test]
#[cfg(feature = "test-support")]
fn write_intent_counters_accumulate_independently() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.write_intents_reclaimed(3);
    m.write_intents_reclaimed(2);
    m.write_intent_lost();
    m.write_intent_reclaim_failed();
    m.write_intent_reclaim_failed();
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_write_intents_reclaimed_total", &[]),
        5
    );
    assert_eq!(h.counter_value("credstore_write_intent_lost_total", &[]), 1);
    assert_eq!(
        h.counter_value("credstore_write_intent_reclaim_failed_total", &[]),
        2
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

#[test]
#[cfg(feature = "test-support")]
fn secret_unreadable_accumulates() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.secret_unreadable();
    m.secret_unreadable();
    h.force_flush();
    assert_eq!(h.counter_value("credstore_secret_unreadable_total", &[]), 2);
}
