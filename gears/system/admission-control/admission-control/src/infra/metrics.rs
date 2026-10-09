//! OpenTelemetry instruments of the gear, on the meter scope
//! `admission-control` of the platform's global provider: verdicts by cause,
//! engine call latency and dropped refusal events. No tenant, subject or
//! resource identifier is ever a label.

use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue};

use crate::domain::service::AdmissionMetrics;

/// Meter scope of every instrument.
pub const METER_SCOPE: &str = "admission-control";

/// Buckets (seconds) of the engine-call histogram.
const ENGINE_BUCKETS_SECONDS: [f64; 10] =
    [0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0];

/// Every OpenTelemetry instrument of the gear.
pub struct AdmissionControlMetrics {
    verdicts: Counter<u64>,
    engine_call: Histogram<f64>,
    events_dropped: Counter<u64>,
}

impl std::fmt::Debug for AdmissionControlMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionControlMetrics")
            .finish_non_exhaustive()
    }
}

impl AdmissionControlMetrics {
    /// Instruments on the platform's global meter provider.
    #[must_use]
    pub fn global() -> Self {
        let scope = InstrumentationScope::builder(METER_SCOPE).build();
        Self::new(&opentelemetry::global::meter_with_scope(scope))
    }

    /// Instruments on `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            verdicts: meter
                .u64_counter("admission_control_verdicts_total")
                .with_description("Decisions by cause (admitted or the refusal cause)")
                .build(),
            engine_call: meter
                .f64_histogram("admission_control_engine_call_seconds")
                .with_description("Wall time of engine calls actually made")
                .with_boundaries(ENGINE_BUCKETS_SECONDS.to_vec())
                .build(),
            events_dropped: meter
                .u64_counter("admission_control_events_dropped_total")
                .with_description("Refusal events dropped (queue full or broker failing)")
                .build(),
        }
    }

    /// One decision, labelled `admitted` or by refusal cause.
    pub fn verdict(&self, cause: &'static str) {
        self.verdicts.add(1, &[KeyValue::new("cause", cause)]);
    }

    /// Wall time of one engine call.
    pub fn engine_latency(&self, elapsed: Duration) {
        self.engine_call.record(elapsed.as_secs_f64(), &[]);
    }

    /// One refusal event was dropped.
    pub fn event_dropped(&self) {
        self.events_dropped.add(1, &[]);
    }
}

impl AdmissionMetrics for AdmissionControlMetrics {
    fn verdict(&self, cause: &'static str) {
        Self::verdict(self, cause);
    }

    fn engine_latency(&self, elapsed: Duration) {
        Self::engine_latency(self, elapsed);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::expect_used)]

    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{
        AggregatedMetrics, HistogramDataPoint, MetricData, ResourceMetrics, ScopeMetrics,
        SumDataPoint,
    };
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::*;

    /// What the instruments export after recording through `record`.
    fn exported(record: impl FnOnce(&AdmissionControlMetrics)) -> Vec<ResourceMetrics> {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        record(&AdmissionControlMetrics::new(&provider.meter(METER_SCOPE)));
        provider.force_flush().expect("the reader flushes");
        exporter.get_finished_metrics().expect("metrics exported")
    }

    #[test]
    fn every_instrument_exports_under_its_name_and_labels() {
        let metrics = exported(|metrics| {
            metrics.verdict("policy");
            metrics.verdict("policy");
            metrics.verdict("admitted");
            metrics.engine_latency(Duration::from_millis(3));
            metrics.event_dropped();
        });
        let named = |name: &str| {
            metrics
                .iter()
                .flat_map(ResourceMetrics::scope_metrics)
                .flat_map(ScopeMetrics::metrics)
                .find(|metric| metric.name() == name)
                .unwrap_or_else(|| panic!("`{name}` not exported"))
                .data()
        };

        let AggregatedMetrics::U64(MetricData::Sum(verdicts)) =
            named("admission_control_verdicts_total")
        else {
            panic!("verdicts is not a u64 sum");
        };
        let mut by_cause: Vec<(String, u64)> = verdicts
            .data_points()
            .map(|point| {
                let labels: Vec<_> = point.attributes().collect();
                assert_eq!(labels.len(), 1, "cause is the only label");
                assert_eq!(labels[0].key.as_str(), "cause");
                (labels[0].value.to_string(), point.value())
            })
            .collect();
        by_cause.sort();
        assert_eq!(
            by_cause,
            [("admitted".to_owned(), 1), ("policy".to_owned(), 2)]
        );

        let AggregatedMetrics::F64(MetricData::Histogram(latency)) =
            named("admission_control_engine_call_seconds")
        else {
            panic!("engine call is not an f64 histogram");
        };
        let samples: u64 = latency.data_points().map(HistogramDataPoint::count).sum();
        assert_eq!(samples, 1);

        let AggregatedMetrics::U64(MetricData::Sum(dropped)) =
            named("admission_control_events_dropped_total")
        else {
            panic!("dropped events is not a u64 sum");
        };
        assert_eq!(
            dropped.data_points().map(SumDataPoint::value).sum::<u64>(),
            1
        );
    }
}
