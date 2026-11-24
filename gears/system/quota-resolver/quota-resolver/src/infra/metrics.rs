//! `OpenTelemetry` adapter behind the [`QuotaMetrics`] port.
//!
//! Counters omit a `_total` suffix; the Prometheus exporter appends it. Every
//! label value comes from a closed set.

use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::{Operation, Outcome, Plane, QuotaMetrics};

const OVERHEAD_BUCKETS_MS: [f64; 10] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 100.0];

/// Records the resolver's operation counter and overhead histogram.
pub struct QuotaMetricsMeter {
    operation: Counter<u64>,
    overhead_ms: Histogram<f64>,
}

impl QuotaMetricsMeter {
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            operation: meter
                .u64_counter("quota_resolver_operation")
                .with_description("Quota resolver calls finished (operation, plane, outcome)")
                .build(),
            overhead_ms: meter
                .f64_histogram("quota_resolver_overhead_ms")
                .with_description(
                    "Time the resolver spent before calling the backend plugin, in milliseconds",
                )
                .with_unit("ms")
                .with_boundaries(OVERHEAD_BUCKETS_MS.to_vec())
                .build(),
        }
    }
}

impl QuotaMetrics for QuotaMetricsMeter {
    fn record_operation(&self, operation: Operation, plane: Plane, outcome: Outcome) {
        self.operation.add(
            1,
            &[
                KeyValue::new("operation", operation.as_label()),
                KeyValue::new("plane", plane.as_label()),
                KeyValue::new("outcome", outcome.as_label()),
            ],
        );
    }

    fn record_overhead_ms(&self, millis: f64) {
        self.overhead_ms.record(millis, &[]);
    }
}

/// Build the adapter against the process-global meter provider; with metrics
/// disabled the global provider is a no-op.
#[must_use]
pub fn build_default_adapter() -> Arc<QuotaMetricsMeter> {
    let scope = opentelemetry::InstrumentationScope::builder("quota-resolver").build();
    let meter = opentelemetry::global::meter_with_scope(scope);
    Arc::new(QuotaMetricsMeter::new(&meter))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod metrics_tests;
