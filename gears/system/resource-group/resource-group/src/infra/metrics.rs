// Created: 2026-08-07 by Constructor Tech
//! OpenTelemetry adapter for [`RgMetricsPort`].
//!
//! Instruments come from the process-global meter provider the host installs
//! (`toolkit::telemetry::init_metrics_provider`). Until an exporter is wired
//! that provider is the built-in no-op, so an uninstrumented deployment --
//! and every test in this crate -- pays nothing.
//!
//! Instrument names are the full literal Prometheus names, with the suffix
//! baked in rather than left to the exporter: counters end in `_total`,
//! duration histograms in `_seconds`. No `.with_unit()` hint is set, matching
//! the platform's `add_metric_suffixes: false` collector posture and the
//! other gears' adapters.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::metrics::{Operation, Outcome, RgMetricsPort};

/// Meter / instrumentation scope name.
pub(crate) const METER_NAME: &str = "resource-group";

// ── Metric names (literal Prometheus form) ───────────────────────────────
const RG_OPERATION_DURATION: &str = "rg_operation_duration_seconds";
const RG_SUBTREE_NODES: &str = "rg_subtree_nodes";
const RG_CLOSURE_ROWS_WRITTEN: &str = "rg_closure_rows_written";
const RG_ISOLATION_ESCALATION: &str = "rg_isolation_escalation_total";
const RG_METADATA_VALIDATION_DURATION: &str = "rg_metadata_validation_duration_seconds";

/// OpenTelemetry-backed metrics handle for resource-group.
pub struct RgMetricsMeter {
    operation_duration: Histogram<f64>,
    subtree_nodes: Histogram<u64>,
    closure_rows_written: Histogram<u64>,
    isolation_escalation: Counter<u64>,
    metadata_validation_duration: Histogram<f64>,
}

impl std::fmt::Debug for RgMetricsMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RgMetricsMeter").finish_non_exhaustive()
    }
}

impl RgMetricsMeter {
    /// Build every instrument against `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            operation_duration: meter
                .f64_histogram(RG_OPERATION_DURATION)
                .with_description(
                    "Wall time of a resource-group write operation, by operation and outcome. \
                     Includes retries, which the caller cannot see individually.",
                )
                .build(),
            subtree_nodes: meter
                .u64_histogram(RG_SUBTREE_NODES)
                .with_description(
                    "Nodes in the subtree an operation touched, for the operations whose cost \
                     is a function of it.",
                )
                .build(),
            closure_rows_written: meter
                .u64_histogram(RG_CLOSURE_ROWS_WRITTEN)
                .with_description(
                    "Closure rows written by an operation, as counted by the database: the \
                     set-based rebuild never materializes them in the process.",
                )
                .build(),
            isolation_escalation: meter
                .u64_counter(RG_ISOLATION_ESCALATION)
                .with_description(
                    "Operations that opened below SERIALIZABLE on a pre-transaction hint, \
                     found they needed it, and ran again.",
                )
                .build(),
            metadata_validation_duration: meter
                .f64_histogram(RG_METADATA_VALIDATION_DURATION)
                .with_description(
                    "Time resolving and compiling the GTS metadata schema, outside the \
                     transaction.",
                )
                .build(),
        }
    }

    /// Build a handle bound to the process-global meter provider.
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }
}

impl RgMetricsPort for RgMetricsMeter {
    fn operation_duration(&self, operation: Operation, outcome: Outcome, seconds: f64) {
        self.operation_duration.record(
            seconds,
            &[
                KeyValue::new("operation", operation.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    fn subtree_nodes(&self, operation: Operation, nodes: u64) {
        self.subtree_nodes
            .record(nodes, &[KeyValue::new("operation", operation.as_str())]);
    }

    fn closure_rows_written(&self, operation: Operation, rows: u64) {
        self.closure_rows_written
            .record(rows, &[KeyValue::new("operation", operation.as_str())]);
    }

    fn isolation_escalation(&self, operation: Operation) {
        self.isolation_escalation
            .add(1, &[KeyValue::new("operation", operation.as_str())]);
    }

    fn metadata_validation_duration(&self, operation: Operation, seconds: f64) {
        self.metadata_validation_duration
            .record(seconds, &[KeyValue::new("operation", operation.as_str())]);
    }
}

/// In-memory meter provider and exporter, for asserting what a recording
/// actually emits.
#[cfg(test)]
pub(crate) mod harness {
    #![allow(clippy::expect_used)]

    use opentelemetry::metrics::{Meter, MeterProvider};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::{METER_NAME, RgMetricsMeter};

    /// One exported data point: its attributes, sorted, and the value a test
    /// asserts on -- a counter's sum, or a histogram's observation count.
    pub type Point = (Vec<(String, String)>, u64);

    pub struct MetricsHarness {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
    }

    impl MetricsHarness {
        pub fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { provider, exporter }
        }

        pub fn meter(&self) -> Meter {
            self.provider.meter(METER_NAME)
        }

        pub fn metrics(&self) -> RgMetricsMeter {
            RgMetricsMeter::new(&self.meter())
        }

        pub fn force_flush(&self) {
            self.provider
                .force_flush()
                .expect("test meter provider should flush");
        }

        /// Every data point recorded for `name`, as
        /// `(attributes, sum-or-count)`. Histograms report their count, so a
        /// test can assert "recorded once, with these attributes" without
        /// pinning the value; counters report their sum.
        pub fn points(&self, name: &str) -> Vec<Point> {
            self.force_flush();
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");

            // Only the most recent export. The exporter keeps every batch it
            // has been handed, and temporality here is cumulative, so each
            // flush re-emits the full state -- reading all batches would
            // report a series once per `points` call made before it.
            let mut out = Vec::new();
            if let Some(rm) = metrics.last() {
                for sm in rm.scope_metrics() {
                    for m in sm.metrics() {
                        if m.name() != name {
                            continue;
                        }
                        match m.data() {
                            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                                for dp in sum.data_points() {
                                    out.push((attrs(dp.attributes()), dp.value()));
                                }
                            }
                            AggregatedMetrics::U64(MetricData::Histogram(h)) => {
                                for dp in h.data_points() {
                                    out.push((attrs(dp.attributes()), dp.count()));
                                }
                            }
                            AggregatedMetrics::F64(MetricData::Histogram(h)) => {
                                for dp in h.data_points() {
                                    out.push((attrs(dp.attributes()), dp.count()));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            out
        }
    }

    fn attrs<'a>(kvs: impl Iterator<Item = &'a opentelemetry::KeyValue>) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = kvs
            .map(|kv| (kv.key.to_string(), kv.value.to_string()))
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
