use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
};

use crate::domain::ports::UsersMetricsPort;

const COUNTER_NAME: &str = "users_info.get_user";
const CARDINALITY_LIMIT: usize = 2000;

fn local_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .with_view(|_: &Instrument| {
            Stream::builder()
                .with_cardinality_limit(CARDINALITY_LIMIT)
                .build()
                .ok()
        })
        .build();
    (provider, exporter)
}

fn extract_counter_value(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
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

#[test]
fn test_record_get_user_increments_counter() {
    let (provider, exporter) = local_provider();
    let metrics = super::UsersMetricsMeter::new(&provider.meter("users-info"));

    metrics.record_get_user("success");
    metrics.record_get_user("success");
    metrics.record_get_user("success");

    provider.force_flush().unwrap();

    let value = extract_counter_value(&exporter, COUNTER_NAME);
    assert_eq!(value, 3, "expected counter == 3, got {value}");
}

#[test]
fn test_record_get_user_single_increment() {
    let (provider, exporter) = local_provider();
    let metrics = super::UsersMetricsMeter::new(&provider.meter("users-info"));

    metrics.record_get_user("success");

    provider.force_flush().unwrap();

    let value = extract_counter_value(&exporter, COUNTER_NAME);
    assert_eq!(value, 1, "expected counter == 1, got {value}");
}
