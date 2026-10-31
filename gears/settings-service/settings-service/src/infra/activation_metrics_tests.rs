//! The activation instruments on a local meter: their names, and the labels
//! they carry — and nothing else.

use std::time::Duration;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::OtelNotifyMetrics;
use crate::domain::activation::NotifyMetrics;

#[test]
fn every_notification_instrument_is_exported_under_its_name_with_its_labels() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = OtelNotifyMetrics::on(&provider.meter("test"));

    metrics.publish_to_deliver("change_notification", Duration::from_millis(40));
    metrics.publish_to_deliver("cache_invalidate", Duration::from_millis(10));
    metrics.response_latency(Duration::from_secs(3));
    metrics.redelivery();
    metrics.awaiting(7);
    metrics.subscriptions(9);
    metrics.reaction("succeeded");
    metrics.report("applied");
    metrics.report("ignored");
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
    assert!(reports.contains("applied") && reports.contains("ignored"));
}
