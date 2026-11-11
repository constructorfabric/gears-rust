//! The activation instruments on a local meter: their names, and the labels
//! they carry — and nothing else.

use std::time::Duration;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::{OtelLockMetrics, OtelNotifyMetrics, SECONDS_BUCKETS};
use crate::domain::activation::{LockMetrics, NotifyMetrics};

#[test]
fn every_notification_instrument_is_exported_under_its_name_with_its_labels() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = OtelNotifyMetrics::on(&provider.meter("test"));

    metrics.publish_to_deliver("change_notification", Duration::from_millis(40));
    metrics.publish_to_deliver("cache_invalidate", Duration::from_millis(10));
    metrics.response_latency(Duration::from_secs(3), false);
    metrics.response_latency(Duration::from_mins(15), true);
    metrics.redelivery();
    metrics.awaiting(7);
    metrics.subscriptions(9);
    metrics.silent(2);
    metrics.silent_subscriptions(3);
    metrics.silenced();
    metrics.incomplete();
    metrics.forgotten();
    metrics.reaction("succeeded");
    for result in [
        "applied",
        "not_subscribed",
        "ignored_no_row",
        "ignored_stale",
        "ignored_repeat",
    ] {
        metrics.report(result);
    }
    metrics.eviction("evicted");
    metrics.eviction("refused");
    provider.force_flush().expect("flushed");

    let finished = exporter.get_finished_metrics().expect("exported");
    let mut seen = std::collections::BTreeMap::new();
    for resource in &finished {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                seen.insert(metric.name().to_owned(), format!("{:?}", metric.data()));
            }
        }
    }
    let names: Vec<&str> = seen.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "settings_activation_awaiting",
            "settings_activation_evictions_total",
            "settings_activation_forgotten_total",
            "settings_activation_incomplete_total",
            "settings_activation_silenced_total",
            "settings_activation_silent",
            "settings_activation_silent_subscriptions",
            "settings_notify_publish_to_deliver_seconds",
            "settings_notify_reactions_total",
            "settings_notify_redeliveries_total",
            "settings_notify_reports_total",
            "settings_notify_response_latency_seconds",
            "settings_notify_subscriptions",
        ]
    );
    let by_event = &seen["settings_notify_publish_to_deliver_seconds"];
    assert!(by_event.contains("change_notification") && by_event.contains("cache_invalidate"));
    assert!(seen["settings_notify_reactions_total"].contains("succeeded"));
    let reports = &seen["settings_notify_reports_total"];
    for result in [
        "applied",
        "not_subscribed",
        "ignored_no_row",
        "ignored_stale",
        "ignored_repeat",
    ] {
        assert!(reports.contains(result), "{result}: {reports}");
    }
    let latency = &seen["settings_notify_response_latency_seconds"];
    assert!(
        latency.contains("late") && latency.contains("true") && latency.contains("false"),
        "{latency}"
    );
    let evictions = &seen["settings_activation_evictions_total"];
    assert!(evictions.contains("evicted") && evictions.contains("refused"));
    // The silence and forgetting instruments are unlabelled.
    for name in [
        "settings_activation_silent",
        "settings_activation_silent_subscriptions",
        "settings_activation_silenced_total",
        "settings_activation_incomplete_total",
        "settings_activation_forgotten_total",
    ] {
        assert!(
            !seen[name].contains("KeyValue"),
            "{name} carries no label: {}",
            seen[name]
        );
    }
}

#[test]
fn every_seconds_histogram_exports_the_second_scale_boundaries() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let meter = provider.meter("test");
    let notify = OtelNotifyMetrics::on(&meter);
    let lock = OtelLockMetrics::on(&meter);
    notify.publish_to_deliver("change_notification", Duration::from_millis(40));
    notify.response_latency(Duration::from_secs(3), false);
    lock.acquired(Duration::from_millis(20));
    provider.force_flush().expect("flushed");

    let finished = exporter.get_finished_metrics().expect("exported");
    let expected = format!("bounds: {SECONDS_BUCKETS:?}");
    let mut checked = Vec::new();
    for resource in &finished {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name().ends_with("_seconds") {
                    let data = format!("{:?}", metric.data());
                    assert!(data.contains(&expected), "{}: {data}", metric.name());
                    checked.push(metric.name().to_owned());
                }
            }
        }
    }
    checked.sort();
    assert_eq!(
        checked,
        [
            "settings_mutation_lock_wait_seconds",
            "settings_notify_publish_to_deliver_seconds",
            "settings_notify_response_latency_seconds",
        ]
    );
}
