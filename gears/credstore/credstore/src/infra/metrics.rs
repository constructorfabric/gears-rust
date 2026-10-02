//! OpenTelemetry adapter implementing [`CredStoreMetricsPort`].
//!
//! Instruments are pulled from the process-global meter provider installed by
//! the host; a no-op until an exporter is wired. Instrument names are full
//! literal Prometheus names: counters end in `_total`, duration histograms in
//! `_seconds`, with suffixes baked in (no `.with_unit()`). Matches the
//! platform's `add_metric_suffixes: false` collector posture.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};

/// Meter / instrumentation scope name.
pub(crate) const METER_NAME: &str = "credstore";

// ─── Metric names (literal Prometheus form; `add_metric_suffixes: false`) ─────
const CREDSTORE_READ_OUTCOME: &str = "credstore_read_outcome_total";
const CREDSTORE_WALKUP_DEPTH: &str = "credstore_walkup_depth";
const CREDSTORE_DEPENDENCY_QUERY_DURATION: &str = "credstore_dependency_query_duration_seconds";
const CREDSTORE_DEPENDENCY_HEALTH: &str = "credstore_dependency_health_total";
const CREDSTORE_CROSS_TENANT_DENIED: &str = "credstore_cross_tenant_denied_total";
const CREDSTORE_DESTROY_FAILED: &str = "credstore_destroy_failed_total";
const CREDSTORE_OUTBOX_PURGE_FAILED: &str = "credstore_outbox_purge_failed_total";
const CREDSTORE_READ_RETRY: &str = "credstore_read_retry_total";
const CREDSTORE_LIST_TYPE_INVARIANT_VIOLATION: &str =
    "credstore_list_type_invariant_violation_total";
const CREDSTORE_AUDIT_PUBLISH_FAILED: &str = "credstore_audit_publish_failed_total";
const CREDSTORE_SECRET_UNREADABLE: &str = "credstore_secret_unreadable_total";

/// OpenTelemetry-backed metrics handle for the credstore module.
pub struct CredStoreMetricsMeter {
    read_outcome: Counter<u64>,
    walkup_depth: Histogram<u64>,
    dependency_query_duration: Histogram<f64>,
    dependency_health: Counter<u64>,
    cross_tenant_denied: Counter<u64>,
    destroy_failed: Counter<u64>,
    outbox_purge_failed: Counter<u64>,
    read_retry: Counter<u64>,
    list_type_invariant_violation: Counter<u64>,
    audit_publish_failed: Counter<u64>,
    secret_unreadable: Counter<u64>,
}

impl std::fmt::Debug for CredStoreMetricsMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredStoreMetricsMeter")
            .finish_non_exhaustive()
    }
}

impl CredStoreMetricsMeter {
    /// Build the instrument set from the supplied meter.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            read_outcome: meter
                .u64_counter(CREDSTORE_READ_OUTCOME)
                .with_description("Secret read results by outcome")
                .build(),
            walkup_depth: meter
                .u64_histogram(CREDSTORE_WALKUP_DEPTH)
                .with_description("Tenant walk-up depth when resolving inherited secrets")
                .build(),
            dependency_query_duration: meter
                .f64_histogram(CREDSTORE_DEPENDENCY_QUERY_DURATION)
                .with_description("Upstream dependency query latency, by dependency + operation")
                .with_boundaries(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25])
                .build(),
            dependency_health: meter
                .u64_counter(CREDSTORE_DEPENDENCY_HEALTH)
                .with_description(
                    "Upstream dependency call outcomes, by dependency + operation + outcome",
                )
                .build(),
            cross_tenant_denied: meter
                .u64_counter(CREDSTORE_CROSS_TENANT_DENIED)
                .with_description("Cross-tenant secret access attempts that were denied")
                .build(),
            destroy_failed: meter
                .u64_counter(CREDSTORE_DESTROY_FAILED)
                .with_description(
                    "Best-effort destroy of store versions (after a write, a lost CAS or a \
                     secret removal) that failed; the next successful write retries implicitly",
                )
                .build(),
            outbox_purge_failed: meter
                .u64_counter(CREDSTORE_OUTBOX_PURGE_FAILED)
                .with_description(
                    "Outbox delete_key delivery attempts that failed and will be retried (a \
                     persistently rising value means a key purge is stuck)",
                )
                .build(),
            read_retry: meter
                .u64_counter(CREDSTORE_READ_RETRY)
                .with_description(
                    "Secret reads that found their version gone and re-read the row once, by \
                     outcome (second_miss = 503)",
                )
                .build(),
            list_type_invariant_violation: meter
                .u64_counter(CREDSTORE_LIST_TYPE_INVARIANT_VIOLATION)
                .with_description(
                    "Collection read: a reduced reference's winner named a type outside the \
                     authorized set (override-type-consistency violated); the reference was \
                     dropped from the page",
                )
                .build(),
            audit_publish_failed: meter
                .u64_counter(CREDSTORE_AUDIT_PUBLISH_FAILED)
                .with_description(
                    "Audit events for secret reads and writes that the event broker could not \
                     accept (absent, unavailable, slow or rejecting); the operation itself was \
                     unaffected",
                )
                .build(),
            secret_unreadable: meter
                .u64_counter(CREDSTORE_SECRET_UNREADABLE)
                .with_description(
                    "Secret reads whose stored version can never be read (the plugin reported \
                     it unreadable, or it was gone although the record's pointer did not \
                     move); a rising value means records need a rewrite or delete",
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

impl CredStoreMetricsPort for CredStoreMetricsMeter {
    fn read_outcome(&self, outcome: ReadOutcome) {
        self.read_outcome
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn walkup_depth(&self, depth: u64) {
        self.walkup_depth.record(depth, &[]);
    }

    fn dependency(&self, dep: Dep, op: DepOp, outcome: Outcome, secs: f64) {
        self.dependency_query_duration.record(
            secs,
            &[
                KeyValue::new("dependency", dep.as_str()),
                KeyValue::new("operation", op.as_str()),
            ],
        );
        self.dependency_health.add(
            1,
            &[
                KeyValue::new("dependency", dep.as_str()),
                KeyValue::new("operation", op.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    fn cross_tenant_denied(&self) {
        self.cross_tenant_denied.add(1, &[]);
    }

    fn destroy_failed(&self) {
        self.destroy_failed.add(1, &[]);
    }

    fn outbox_purge_failed(&self) {
        self.outbox_purge_failed.add(1, &[]);
    }

    fn read_retry(&self, outcome: ReadRetryOutcome) {
        self.read_retry
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn list_type_invariant_violation(&self) {
        self.list_type_invariant_violation.add(1, &[]);
    }

    fn audit_publish_failed(&self) {
        self.audit_publish_failed.add(1, &[]);
    }

    fn secret_unreadable(&self) {
        self.secret_unreadable.add(1, &[]);
    }
}

#[cfg(feature = "test-support")]
pub mod test_harness {
    //! In-memory OpenTelemetry harness for asserting emitted credstore metrics.
    #![allow(clippy::expect_used, clippy::missing_panics_doc, dead_code)]

    use opentelemetry::metrics::{Meter, MeterProvider};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::{CredStoreMetricsMeter, METER_NAME};

    /// In-memory meter provider + exporter for unit and integration tests.
    pub struct MetricsHarness {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
    }

    impl MetricsHarness {
        #[must_use]
        pub fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { provider, exporter }
        }

        #[must_use]
        pub fn meter(&self) -> Meter {
            self.provider.meter(METER_NAME)
        }

        /// A metrics handle bound to this harness's provider.
        #[must_use]
        pub fn metrics(&self) -> CredStoreMetricsMeter {
            CredStoreMetricsMeter::new(&self.meter())
        }

        /// Flush aggregated data into the in-memory exporter.
        pub fn force_flush(&self) {
            self.provider
                .force_flush()
                .expect("test meter provider should flush");
        }

        /// Sum all matching `u64` counter data points.
        #[must_use]
        pub fn counter_value(&self, name: &str, expected_attrs: &[(&str, &str)]) -> u64 {
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");
            let mut total = 0u64;
            for rm in &metrics {
                for sm in rm.scope_metrics() {
                    for metric in sm.metrics() {
                        if metric.name() == name
                            && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                        {
                            for dp in sum.data_points() {
                                if attributes_match(dp.attributes(), expected_attrs) {
                                    total += dp.value();
                                }
                            }
                        }
                    }
                }
            }
            total
        }

        /// Sum matching histogram sample counts.
        #[must_use]
        pub fn histogram_count(&self, name: &str, expected_attrs: &[(&str, &str)]) -> u64 {
            let metrics = self
                .exporter
                .get_finished_metrics()
                .expect("in-memory exporter should be readable");
            let mut total = 0u64;
            for rm in &metrics {
                for sm in rm.scope_metrics() {
                    for metric in sm.metrics() {
                        if metric.name() == name
                            && let AggregatedMetrics::F64(MetricData::Histogram(hist)) =
                                metric.data()
                        {
                            for dp in hist.data_points() {
                                if attributes_match(dp.attributes(), expected_attrs) {
                                    total += dp.count();
                                }
                            }
                        }
                    }
                }
            }
            total
        }
    }

    impl Default for MetricsHarness {
        fn default() -> Self {
            Self::new()
        }
    }

    fn attributes_match<'a>(
        actual_attrs: impl Iterator<Item = &'a opentelemetry::KeyValue>,
        expected: &[(&str, &str)],
    ) -> bool {
        let actual = actual_attrs.collect::<Vec<_>>();
        expected.iter().all(|(k, v)| {
            actual
                .iter()
                .any(|kv| kv.key.as_str() == *k && kv.value.as_str() == *v)
        }) && actual.len() == expected.len()
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
