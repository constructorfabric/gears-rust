// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-read-metrics:p1
//! The reader boundary's read metrics, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram};

use crate::domain::ports::ReadMetrics;

/// The explicit bucket boundaries of `settings_effective_read_duration_seconds`,
/// in seconds: the 2 ms and 15 ms p95 targets are boundaries, so they can be
/// asserted from the histogram rather than interpolated.
pub const READ_DURATION_BUCKETS: [f64; 13] = [
    0.001, 0.002, 0.005, 0.01, 0.015, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
];

/// `settings_effective_read_duration_seconds` by `source` and `scope_class`,
/// `settings_cache_lookups_total` by `result` and `scope_class`, and
/// `settings_resolve_failures_total` by `reason`.
///
/// The hit ratio is a dashboard derivation of the lookup counter, and the
/// read histogram's boundaries are [`READ_DURATION_BUCKETS`]. No label carries
/// a key, a tenant, an actor or a value.
pub struct OtelReadMetrics {
    duration: Histogram<f64>,
    lookups: Counter<u64>,
    failures: Counter<u64>,
}

impl OtelReadMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            duration: meter
                .f64_histogram("settings_effective_read_duration_seconds")
                .with_description("Effective-value read latency at the reader boundary")
                .with_unit("s")
                .with_boundaries(READ_DURATION_BUCKETS.to_vec())
                .build(),
            lookups: meter
                .u64_counter("settings_cache_lookups_total")
                .with_description("Effective-value cache lookups, by result and scope class")
                .build(),
            failures: meter
                .u64_counter("settings_resolve_failures_total")
                .with_description("Reader-boundary resolves that failed, by reason")
                .build(),
        }
    }
}

impl Default for OtelReadMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadMetrics for OtelReadMetrics {
    fn lookup(&self, result: &'static str, scope_class: &'static str) {
        self.lookups.add(
            1,
            &[
                KeyValue::new("result", result),
                KeyValue::new("scope_class", scope_class),
            ],
        );
    }

    fn read(&self, source: &'static str, scope_class: &'static str, seconds: f64) {
        self.duration.record(
            seconds,
            &[
                KeyValue::new("source", source),
                KeyValue::new("scope_class", scope_class),
            ],
        );
    }

    fn resolve_failure(&self, reason: &'static str) {
        self.failures.add(1, &[KeyValue::new("reason", reason)]);
    }
}

#[cfg(test)]
#[path = "read_metrics_tests.rs"]
mod read_metrics_tests;
