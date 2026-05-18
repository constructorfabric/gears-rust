//! End-to-end tests for [`AmMetricsMeter`] against an in-memory
//! OpenTelemetry exporter. Exercises both the typed port methods and
//! the transitional [`MetricsFacadeBridge`] stringly-typed path.

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use crate::domain::metrics::{
    AM_BOOTSTRAP_LIFECYCLE, AM_HIERARCHY_INTEGRITY_DURATION, AM_HIERARCHY_INTEGRITY_VIOLATIONS,
    MetricKind, MetricsFacadeBridge,
};
use crate::domain::ports::metrics::{
    BootstrapClassification, BootstrapMetricsPort, BootstrapOutcome, BootstrapPhase,
    DependencyMetricsPort, DependencyOp, DependencyOutcome, DependencyTarget, HierarchyDepthMode,
    HierarchyDepthOutcome, IntegrityMetricsPort, IntegrityPhase, IntegrityRunOutcome,
    TenantMetricsPort, TenantRetentionJob, TenantRetentionOutcome,
};
use crate::domain::tenant::integrity::IntegrityCategory;
use crate::infra::metrics::AmMetricsMeter;

use account_management_sdk::idp::IdpProvisionFailure;

const TEST_PREFIX: &str = "test_am";

fn local_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    (provider, exporter)
}

fn meter(provider: &SdkMeterProvider) -> AmMetricsMeter {
    AmMetricsMeter::new(&provider.meter("account-management"), TEST_PREFIX)
}

fn extract_u64_counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn count_data_points_with_label(
    exporter: &InMemoryMetricExporter,
    name: &str,
    label_key: &str,
    label_value: &str,
) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .filter(|dp| {
                            dp.attributes().any(|kv| {
                                kv.key.as_str() == label_key && kv.value.as_str() == label_value
                            })
                        })
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn extract_i64_gauge_last(exporter: &InMemoryMetricExporter, name: &str) -> Option<i64> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::I64(MetricData::Gauge(g)) = metric.data()
                {
                    return g
                        .data_points()
                        .next()
                        .map(opentelemetry_sdk::metrics::data::GaugeDataPoint::value);
                }
            }
        }
    }
    None
}

fn extract_f64_histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                        .sum();
                }
            }
        }
    }
    0
}

// ════════════════════════════════════════════════════════════════════
//  Typed port methods
// ════════════════════════════════════════════════════════════════════

#[test]
fn dependency_health_counter_with_and_without_outcome() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.dependency_health(
        DependencyOp::ProvisionTenant,
        DependencyTarget::Idp,
        Some(DependencyOutcome::SUCCESS),
    );
    m.dependency_health(
        DependencyOp::DeprovisionTenant,
        DependencyTarget::Idp,
        Some(DependencyOutcome::TIMEOUT),
    );
    m.dependency_health(DependencyOp::GetType, DependencyTarget::TypesRegistry, None);

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.dependency_health"),
        3,
    );
    assert_eq!(
        count_data_points_with_label(&exporter, "test_am.dependency_health", "outcome", "success",),
        1,
    );
    assert_eq!(
        count_data_points_with_label(&exporter, "test_am.dependency_health", "outcome", "timeout",),
        1,
    );
}

#[test]
fn bootstrap_lifecycle_counter_routes_classification_label() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.bootstrap_lifecycle(BootstrapPhase::IdpPrecheck, BootstrapOutcome::Timeout, None);
    m.bootstrap_lifecycle(
        BootstrapPhase::RootCreating,
        BootstrapOutcome::Failure,
        Some(BootstrapClassification::IDP_TIMEOUT),
    );
    let pf = IdpProvisionFailure::Ambiguous {
        detail: String::new(),
    };
    m.bootstrap_lifecycle(
        BootstrapPhase::RootCreating,
        BootstrapOutcome::Failure,
        Some(BootstrapClassification::from(&pf)),
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.bootstrap_lifecycle"),
        3,
    );
    assert_eq!(
        count_data_points_with_label(
            &exporter,
            "test_am.bootstrap_lifecycle",
            "classification",
            "ambiguous",
        ),
        1,
    );
    assert_eq!(
        count_data_points_with_label(
            &exporter,
            "test_am.bootstrap_lifecycle",
            "classification",
            "idp_timeout",
        ),
        1,
    );
}

#[test]
fn tenant_retention_counter() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.tenant_retention(
        TenantRetentionJob::ProvisioningReaper,
        TenantRetentionOutcome::TERMINAL,
    );
    m.tenant_retention(
        TenantRetentionJob::HardDelete,
        TenantRetentionOutcome::RETRYABLE,
    );
    m.tenant_retention(
        TenantRetentionJob::HardDelete,
        TenantRetentionOutcome::RETRYABLE,
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.tenant_retention"),
        3,
    );
    assert_eq!(
        count_data_points_with_label(
            &exporter,
            "test_am.tenant_retention",
            "outcome",
            "retryable",
        ),
        2,
    );
}

#[test]
fn hierarchy_depth_exceedance_renders_threshold() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.hierarchy_depth_exceedance(
        HierarchyDepthMode::Strict,
        HierarchyDepthOutcome::Reject,
        16,
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.hierarchy_depth_exceedance"),
        1,
    );
    assert_eq!(
        count_data_points_with_label(
            &exporter,
            "test_am.hierarchy_depth_exceedance",
            "threshold",
            "16",
        ),
        1,
    );
}

#[test]
fn integrity_run_counters_separate_by_family() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.hierarchy_integrity_runs(IntegrityRunOutcome::Completed);
    m.hierarchy_integrity_runs(IntegrityRunOutcome::Failed);
    m.hierarchy_integrity_repair_runs(IntegrityRunOutcome::Completed);

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.hierarchy_integrity_runs"),
        2,
    );
    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.hierarchy_integrity_repair_runs"),
        1,
    );
}

#[test]
fn integrity_gauge_records_value() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.hierarchy_integrity_violations(IntegrityCategory::OrphanedChild, 7);

    provider.force_flush().unwrap();

    assert_eq!(
        extract_i64_gauge_last(&exporter, "test_am.hierarchy_integrity_violations"),
        Some(7),
    );
}

#[test]
fn integrity_duration_histogram_records_observation() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    m.hierarchy_integrity_duration_ms(IntegrityPhase::Check, 123.4);
    m.hierarchy_integrity_duration_ms(IntegrityPhase::Repair, 56.7);

    provider.force_flush().unwrap();

    assert_eq!(
        extract_f64_histogram_count(&exporter, "test_am.hierarchy_integrity_duration"),
        2,
    );
}

// ════════════════════════════════════════════════════════════════════
//  Stringly facade bridge
// ════════════════════════════════════════════════════════════════════

#[test]
fn facade_bridge_routes_counter_emission() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    MetricsFacadeBridge::emit(
        &m,
        AM_BOOTSTRAP_LIFECYCLE,
        MetricKind::Counter,
        &[("phase", "completed"), ("outcome", "success")],
    );
    MetricsFacadeBridge::emit(
        &m,
        AM_BOOTSTRAP_LIFECYCLE,
        MetricKind::Counter,
        &[("phase", "failed"), ("outcome", "failure")],
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_u64_counter_sum(&exporter, "test_am.bootstrap_lifecycle"),
        2,
    );
    assert_eq!(
        count_data_points_with_label(
            &exporter,
            "test_am.bootstrap_lifecycle",
            "outcome",
            "success",
        ),
        1,
    );
}

#[test]
fn facade_bridge_routes_gauge_emission() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    MetricsFacadeBridge::emit_gauge(
        &m,
        AM_HIERARCHY_INTEGRITY_VIOLATIONS,
        42,
        &[("category", "orphaned_child")],
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_i64_gauge_last(&exporter, "test_am.hierarchy_integrity_violations"),
        Some(42),
    );
}

#[test]
fn facade_bridge_routes_histogram_emission() {
    let (provider, exporter) = local_provider();
    let m = meter(&provider);

    MetricsFacadeBridge::emit_histogram(
        &m,
        AM_HIERARCHY_INTEGRITY_DURATION,
        88.0,
        &[("phase", "check")],
    );

    provider.force_flush().unwrap();

    assert_eq!(
        extract_f64_histogram_count(&exporter, "test_am.hierarchy_integrity_duration"),
        1,
    );
}
