// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for the OpenTelemetry-backed [`CredStoreMetricsMeter`].

#[cfg(feature = "test-support")]
use super::test_harness::MetricsHarness;
#[cfg(feature = "test-support")]
use crate::domain::ports::metrics::{
    CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome, VerifyOp,
    VerifyOutcome,
};

/// Smoke test that exercises instrument construction and every recording
/// path against the global (no-op) meter — no SDK exporter required, so it
/// runs under the default feature set (unlike the `test-support` tests).
#[test]
fn global_meter_records_all_instruments() {
    use super::CredStoreMetricsMeter;
    use crate::domain::ports::metrics::{
        CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
        VerifyOp, VerifyOutcome,
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
    // The store refusing the plugin's credentials is labelled `credentials_rejected`.
    m.dependency(
        Dep::Plugin,
        DepOp::PluginPut,
        Outcome::CredentialsRejected,
        0.03,
    );
    m.cross_tenant_denied();
    m.write_intents_healed(3);
    m.store_cleanup_recorded(CleanupOp::Purge);
    m.store_cleanup_recorded(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Purge);
    m.store_cleanup_failed(CleanupOp::Destroy);
    m.write_commit_verified(VerifyOp::Write, VerifyOutcome::Committed);
    m.write_commit_verified(VerifyOp::Delete, VerifyOutcome::Failed);
    m.read_retry(ReadRetryOutcome::Recovered);
    m.read_retry(ReadRetryOutcome::SecondMiss);
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
fn write_commit_verified_is_labelled_by_op_and_outcome() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.write_commit_verified(VerifyOp::Write, VerifyOutcome::Committed);
    m.write_commit_verified(VerifyOp::Delete, VerifyOutcome::Failed);
    m.write_commit_verified(VerifyOp::Delete, VerifyOutcome::Failed);
    h.force_flush();
    assert_eq!(
        h.counter_value(
            "credstore_write_commit_verified_total",
            &[("op", "write"), ("outcome", "committed")]
        ),
        1
    );
    assert_eq!(
        h.counter_value(
            "credstore_write_commit_verified_total",
            &[("op", "delete"), ("outcome", "failed")]
        ),
        2
    );
}

#[test]
#[cfg(feature = "test-support")]
fn store_cleanup_counters_are_labelled_by_op() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.store_cleanup_recorded(CleanupOp::Purge);
    m.store_cleanup_recorded(CleanupOp::Destroy);
    m.store_cleanup_recorded(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Destroy);
    m.store_cleanup_failed(CleanupOp::Purge);
    m.store_cleanup_failed(CleanupOp::Purge);
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_store_cleanup_recorded_total", &[("op", "purge")]),
        1
    );
    assert_eq!(
        h.counter_value(
            "credstore_store_cleanup_recorded_total",
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
fn write_intents_healed_accumulates() {
    let h = MetricsHarness::new();
    let m = h.metrics();
    m.write_intents_healed(3);
    m.write_intents_healed(2);
    h.force_flush();
    assert_eq!(
        h.counter_value("credstore_write_intents_healed_total", &[]),
        5
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
