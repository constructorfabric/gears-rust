#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::{EXCEPTION_SWEEP_DURATION_BUCKETS_SECONDS, METER_NAME, OtelRatingMetrics};
use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};
use crate::domain::usage::intake::PageReport;
use crate::domain::usage::loss::LossKind;
use crate::domain::usage::metrics::UsageMetricsPort;
use crate::domain::usage::normalize::RejectReason;
use crate::domain::usage::reader::{FeedPosition, PauseReason};

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
                    AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                        for point in gauge.data_points() {
                            found.push((point.value(), labels_of(point.attributes())));
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

fn labels_of<'a>(attributes: impl Iterator<Item = &'a KeyValue>) -> Vec<(String, String)> {
    let mut labels: Vec<(String, String)> = attributes
        .map(|kv| (kv.key.to_string(), kv.value.to_string()))
        .collect();
    labels.sort();
    labels
}

/// One `f64` gauge data point: its value and its labels.
type GaugePoint = (f64, Vec<(String, String)>);

/// Every data point of the `f64` gauge `name`.
fn gauge_points(exporter: &InMemoryMetricExporter, name: &str) -> Vec<GaugePoint> {
    let mut found = Vec::new();
    for resource in &exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics().filter(|m| m.name() == name) {
                if let AggregatedMetrics::F64(MetricData::Gauge(gauge)) = metric.data() {
                    for point in gauge.data_points() {
                        found.push((point.value(), labels_of(point.attributes())));
                    }
                }
            }
        }
    }
    found
}

#[test]
fn the_feed_alerts_read_these_names_and_labels() {
    use crate::domain::usage::feed::{FeedSubscription, MeterId};
    use crate::domain::usage::source::{SourceDefinition, TenantScope};

    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = OtelRatingMetrics::new(&provider.meter(METER_NAME));
    let source = SourceDefinition::new(
        FeedSubscription::new([
            MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.test.hours.v1~").unwrap(),
        ])
        .unwrap(),
        TenantScope::new([uuid::Uuid::from_u128(0x10)]).unwrap(),
    )
    .source_id();
    let mut report = PageReport {
        entries: 9,
        captured: 2,
        duplicates: 3,
        content_conflicts: 1,
        unidentifiable: 1,
        outside_subscription: 2,
        ..PageReport::default()
    };
    report.rejected.insert(RejectReason::UsageMalformed, 2);
    report.rejected.insert(RejectReason::NaturalKeyConflict, 1);
    metrics.feed_page(source, &report);
    metrics.feed_position(
        source,
        FeedPosition {
            lag: Duration::from_secs(7),
            cursor_age: Duration::from_secs(3),
            oldest_unread_age: Duration::from_secs(60),
        },
    );
    metrics.feed_restart(source);
    metrics.feed_paused(source, PauseReason::Backpressure, true);
    metrics.source_losses(source, LossKind::CursorBeyondRetention, 2);
    provider.force_flush().unwrap();

    let label = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    let source_label = label("source", &source.to_string());
    assert_eq!(
        points(&exporter, "rating_usage_duplicate_total"),
        vec![(3, vec![])]
    );
    assert_eq!(
        points(&exporter, "rating_usage_content_conflict_total"),
        vec![(1, vec![])]
    );
    assert_eq!(
        points(&exporter, "rating_usage_natural_key_conflict_total"),
        vec![(1, vec![])],
        "a natural-key conflict is counted on its own and among the rejections"
    );
    let mut rejected = points(&exporter, "rating_usage_rejected_total");
    rejected.sort();
    assert_eq!(
        rejected,
        vec![
            (1, vec![label("reason", "natural_key_conflict")]),
            (2, vec![label("reason", "malformed")]),
        ]
    );
    let mut set_aside = points(&exporter, "rating_usage_capture_rejected_total");
    set_aside.sort();
    assert_eq!(
        set_aside,
        vec![
            (1, vec![label("reason", "unidentifiable")]),
            (2, vec![label("reason", "outside_subscription")]),
        ]
    );
    for (name, seconds) in [
        ("rating_feed_lag_seconds", 7.0),
        ("rating_feed_cursor_age_seconds", 3.0),
        ("rating_feed_oldest_unread_age_seconds", 60.0),
    ] {
        assert_eq!(
            gauge_points(&exporter, name),
            vec![(seconds, vec![source_label.clone()])],
            "{name}"
        );
    }
    assert_eq!(
        points(&exporter, "rating_feed_restart_total"),
        vec![(1, vec![source_label.clone()])]
    );
    let mut paused = points(&exporter, "rating_feed_paused");
    paused[0].1.sort();
    assert_eq!(
        paused,
        vec![(
            1,
            vec![label("reason", "backpressure"), source_label.clone()]
        )]
    );
    let mut losses = points(&exporter, "rating_source_loss_open");
    losses[0].1.sort();
    assert_eq!(
        losses,
        vec![(
            2,
            vec![label("kind", "cursor_beyond_retention"), source_label]
        )]
    );
}
