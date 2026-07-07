//! OpenTelemetry adapter implementing [`CredStoreMetricsPort`].
//!
//! Instruments are pulled from the process-global meter provider installed by
//! the host; a no-op until an exporter is wired. Instrument names are full
//! literal Prometheus names: counters end in `_total`, duration histograms in
//! `_seconds`, with suffixes baked in (no `.with_unit()`). Matches the
//! platform's `add_metric_suffixes: false` collector posture.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, FenceVerify, Outcome, ReadOutcome, SecretCounts,
};

/// Meter / instrumentation scope name.
pub(crate) const METER_NAME: &str = "credstore";

// ─── Metric names (literal Prometheus form; `add_metric_suffixes: false`) ─────
const CREDSTORE_SECRETS: &str = "credstore_secrets";
const CREDSTORE_SECRETS_PROVISIONING: &str = "credstore_secrets_provisioning";
const CREDSTORE_SECRETS_DEPROVISIONING: &str = "credstore_secrets_deprovisioning";
const CREDSTORE_TENANTS_WITH_SECRETS: &str = "credstore_tenants_with_secrets";
const CREDSTORE_READ_OUTCOME: &str = "credstore_read_outcome_total";
const CREDSTORE_WALKUP_DEPTH: &str = "credstore_walkup_depth";
const CREDSTORE_DEPENDENCY_QUERY_DURATION: &str = "credstore_dependency_query_duration_seconds";
const CREDSTORE_DEPENDENCY_HEALTH: &str = "credstore_dependency_health_total";
const CREDSTORE_PROVISIONING_REAPED: &str = "credstore_provisioning_reaped_total";
const CREDSTORE_DEPROVISIONING_REAPED: &str = "credstore_deprovisioning_reaped_total";
const CREDSTORE_PROVISIONING_ROLLBACK: &str = "credstore_provisioning_rollback_total";
const CREDSTORE_CROSS_TENANT_DENIED: &str = "credstore_cross_tenant_denied_total";
const CREDSTORE_FENCE_VERIFY: &str = "credstore_fence_verify_total";
const CREDSTORE_FENCE_BACKFILL: &str = "credstore_fence_backfill_total";

/// OpenTelemetry-backed metrics handle for the credstore module.
pub struct CredStoreMetricsMeter {
    secrets: Gauge<i64>,
    secrets_provisioning: Gauge<i64>,
    secrets_deprovisioning: Gauge<i64>,
    tenants_with_secrets: Gauge<i64>,
    read_outcome: Counter<u64>,
    walkup_depth: Histogram<u64>,
    dependency_query_duration: Histogram<f64>,
    dependency_health: Counter<u64>,
    provisioning_reaped: Counter<u64>,
    deprovisioning_reaped: Counter<u64>,
    provisioning_rollback: Counter<u64>,
    cross_tenant_denied: Counter<u64>,
    fence_verify: Counter<u64>,
    fence_backfill: Counter<u64>,
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
            secrets: meter
                .i64_gauge(CREDSTORE_SECRETS)
                .with_description("Live count of secrets by sharing scope")
                .build(),
            secrets_provisioning: meter
                .i64_gauge(CREDSTORE_SECRETS_PROVISIONING)
                .with_description("Live count of secrets in provisioning state")
                .build(),
            secrets_deprovisioning: meter
                .i64_gauge(CREDSTORE_SECRETS_DEPROVISIONING)
                .with_description("Live count of secrets in deprovisioning state")
                .build(),
            tenants_with_secrets: meter
                .i64_gauge(CREDSTORE_TENANTS_WITH_SECRETS)
                .with_description("Live count of tenants that own at least one secret")
                .build(),
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
            provisioning_reaped: meter
                .u64_counter(CREDSTORE_PROVISIONING_REAPED)
                .with_description("Provisioning secrets reaped by the background sweeper")
                .build(),
            deprovisioning_reaped: meter
                .u64_counter(CREDSTORE_DEPROVISIONING_REAPED)
                .with_description(
                    "Stuck deprovisioning secrets completed by the background sweeper",
                )
                .build(),
            provisioning_rollback: meter
                .u64_counter(CREDSTORE_PROVISIONING_ROLLBACK)
                .with_description(
                    "Create-saga provisioning-row rollbacks after a backend write failure, by outcome",
                )
                .build(),
            cross_tenant_denied: meter
                .u64_counter(CREDSTORE_CROSS_TENANT_DENIED)
                .with_description("Cross-tenant secret access attempts that were denied")
                .build(),
            fence_verify: meter
                .u64_counter(CREDSTORE_FENCE_VERIFY)
                .with_description(
                    "Value-fingerprint fence verdicts on reads, by outcome \
                     (mismatch = fail-closed 404, the alertable signal)",
                )
                .build(),
            fence_backfill: meter
                .u64_counter(CREDSTORE_FENCE_BACKFILL)
                .with_description(
                    "Lazy fingerprint backfills of out-of-band seeded rows, by outcome",
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
    fn record_inventory(&self, counts: SecretCounts) {
        self.secrets
            .record(counts.private, &[KeyValue::new("sharing", "private")]);
        self.secrets
            .record(counts.tenant, &[KeyValue::new("sharing", "tenant")]);
        self.secrets
            .record(counts.shared, &[KeyValue::new("sharing", "shared")]);
        self.secrets_provisioning.record(counts.provisioning, &[]);
        self.secrets_deprovisioning
            .record(counts.deprovisioning, &[]);
        self.tenants_with_secrets.record(counts.tenants, &[]);
    }

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

    fn provisioning_reaped(&self, n: u64) {
        self.provisioning_reaped.add(n, &[]);
    }

    fn deprovisioning_reaped(&self, n: u64) {
        self.deprovisioning_reaped.add(n, &[]);
    }

    fn provisioning_rollback(&self, outcome: Outcome) {
        self.provisioning_rollback
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn cross_tenant_denied(&self) {
        self.cross_tenant_denied.add(1, &[]);
    }

    fn fence_verify(&self, outcome: FenceVerify) {
        self.fence_verify
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn fence_backfill(&self, outcome: Outcome) {
        self.fence_backfill
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
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
