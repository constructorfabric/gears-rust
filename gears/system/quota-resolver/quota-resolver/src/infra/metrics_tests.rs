use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::QuotaMetricsMeter;
use crate::domain::{Operation, Outcome, Plane, QuotaMetrics};

#[test]
fn an_operation_is_counted_with_its_three_labels_and_the_overhead_is_observed() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let meter = QuotaMetricsMeter::new(&provider.meter("quota-resolver"));

    meter.record_operation(
        Operation::AcquireLease,
        Plane::Tenant,
        Outcome::PrincipalNotMapped,
    );
    meter.record_overhead_ms(0.4);
    provider.force_flush().unwrap();

    let mut labels = Vec::new();
    let mut observations = 0;
    for resource in exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                match (metric.name(), metric.data()) {
                    ("quota_resolver_operation", AggregatedMetrics::U64(MetricData::Sum(sum))) => {
                        for point in sum.data_points() {
                            labels = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                                .collect();
                            labels.sort();
                        }
                    }
                    (
                        "quota_resolver_overhead_ms",
                        AggregatedMetrics::F64(MetricData::Histogram(h)),
                    ) => {
                        observations += h
                            .data_points()
                            .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                            .sum::<u64>();
                    }
                    _ => {}
                }
            }
        }
    }

    assert_eq!(
        labels,
        vec![
            ("operation".to_owned(), "acquire_lease".to_owned()),
            ("outcome".to_owned(), "principal_not_mapped".to_owned()),
            ("plane".to_owned(), "tenant".to_owned()),
        ]
    );
    assert_eq!(observations, 1);
}
