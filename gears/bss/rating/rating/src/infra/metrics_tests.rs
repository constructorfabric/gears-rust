#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::{EXCEPTION_SWEEP_DURATION_BUCKETS_SECONDS, METER_NAME, OtelRatingMetrics};
use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};

/// One data point: its value (a histogram's count) and its labels.
type Point = (u64, Vec<(String, String)>);

/// Every data point of `name`.
fn points(exporter: &InMemoryMetricExporter, name: &str) -> Vec<Point> {
    let mut found = Vec::new();
    for resource in &exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics().filter(|m| m.name() == name) {
                match metric.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        for point in sum.data_points() {
                            let labels = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                                .collect();
                            found.push((point.value(), labels));
                        }
                    }
                    AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                        for point in histogram.data_points() {
                            found.push((point.count(), Vec::new()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    found
}

#[test]
fn the_alerts_read_these_names_and_labels() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = OtelRatingMetrics::new(&provider.meter(METER_NAME));
    metrics.inbox_quarantined(InboxSource::Fact);
    metrics.dependency_error(Dependency::Pdp, FailureCode::EvaluationFailed);
    let report = SweepReport {
        tenants: 1,
        retried: 3,
        failed: 1,
    };
    metrics.exception_sweep(SweepOutcome::Completed, &report, Duration::from_millis(40));
    provider.force_flush().unwrap();

    let label = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    assert_eq!(
        points(&exporter, "rating_inbox_quarantined_total"),
        vec![(1, vec![label("source", "fact")])]
    );
    let mut dependency = points(&exporter, "rating_dependency_errors_total");
    dependency[0].1.sort();
    assert_eq!(
        dependency,
        vec![(
            1,
            vec![
                label("code", "evaluation_failed"),
                label("dependency", "pdp")
            ]
        )]
    );
    assert_eq!(
        points(&exporter, "rating_exception_sweeps_total"),
        vec![(1, vec![label("outcome", "completed")])]
    );
    let mut retries = points(&exporter, "rating_exception_sweep_retries_total");
    retries.sort();
    assert_eq!(
        retries,
        vec![
            (1, vec![label("result", "failed")]),
            (3, vec![label("result", "retried")])
        ]
    );
    assert_eq!(
        points(&exporter, "rating_exception_sweep_duration_seconds"),
        vec![(1, Vec::new())]
    );
    assert_eq!(
        bounds(&exporter, "rating_exception_sweep_duration_seconds"),
        EXCEPTION_SWEEP_DURATION_BUCKETS_SECONDS.to_vec()
    );
}

/// The bucket boundaries of histogram `name`.
fn bounds(exporter: &InMemoryMetricExporter, name: &str) -> Vec<f64> {
    for resource in &exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics().filter(|m| m.name() == name) {
                if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data()
                    && let Some(point) = histogram.data_points().next()
                {
                    return point.bounds().collect();
                }
            }
        }
    }
    Vec::new()
}
