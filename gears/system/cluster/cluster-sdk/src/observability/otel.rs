// Created: 2026-06-18 by Constructor Tech
//! OpenTelemetry-backed [`ClusterMetrics`] adapter (the `otel` feature).
//!
//! Concrete sink for the cluster metrics contract, built once per provider over
//! an OpenTelemetry [`Meter`]. It is the single, shared implementation every
//! plugin reuses, so instrument names, units, and the label allowlist are
//! defined in one place rather than re-derived per backend (ADR-004).
//!
//! # Instrument names and the `_total` suffix
//!
//! Counter instruments are created **without** the contract's `_total` suffix:
//! the `opentelemetry-prometheus` exporter appends `_total` to counters when it
//! renders them, so the scraped series matches the catalog name
//! (`cluster_cache_ops` instrument → `cluster_cache_ops_total` scraped).
//! Histogram names carry no such suffix and are used verbatim.
//!
//! # Cardinality
//!
//! Only the bounded [`fields::label`](crate::observability::fields::label) keys
//! are attached as [`KeyValue`] labels. High-cardinality values never reach this
//! adapter — the [`ClusterMetrics`] port has no key/name parameter.

use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue};

use crate::observability::{ClusterMetrics, fields, metrics as names};

/// The instrumentation scope name under which the cluster meter is registered.
const SCOPE: &str = "cf-gears-cluster";

/// Histogram bucket boundaries (seconds) for the operation-latency histograms.
/// Coordination ops range from sub-millisecond (in-process) to a few seconds
/// (a contended remote acquisition), so the buckets span 0.5ms → 5s.
const DURATION_BUCKETS_SECONDS: [f64; 12] = [
    0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
];

/// Strips the contract's `_total` suffix from a counter name, since the
/// Prometheus exporter re-appends it. Keeps the catalog constant the single
/// source of truth for the name.
fn counter_name(contract: &'static str) -> &'static str {
    contract.strip_suffix("_total").unwrap_or(contract)
}

/// An OpenTelemetry-backed [`ClusterMetrics`] sink. Construct with
/// [`new`](Self::new) over a caller-supplied [`Meter`], or
/// [`from_global_meter`](Self::from_global_meter) to use the process-global
/// provider.
pub struct OtelClusterMetrics {
    /// The bounded `provider` label, fixed for this sink.
    provider: &'static str,
    cache_ops: Counter<u64>,
    cache_op_duration: Histogram<f64>,
    lock_ops: Counter<u64>,
    lock_op_duration: Histogram<f64>,
    leader_transitions: Counter<u64>,
    discovery_ops: Counter<u64>,
    watch_resets: Counter<u64>,
    provider_errors: Counter<u64>,
}

impl OtelClusterMetrics {
    /// Builds all instruments over `meter`, labelling every signal with
    /// `provider`.
    #[must_use]
    pub fn new(meter: &Meter, provider: &'static str) -> Self {
        Self {
            provider,
            cache_ops: meter
                .u64_counter(counter_name(names::CACHE_OPS_TOTAL))
                .with_description("Total cluster cache operations")
                .build(),
            cache_op_duration: meter
                .f64_histogram(names::CACHE_OP_DURATION_SECONDS)
                .with_description("Cluster cache operation latency")
                .with_unit("s")
                .with_boundaries(DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
            lock_ops: meter
                .u64_counter(counter_name(names::LOCK_OPS_TOTAL))
                .with_description("Total cluster distributed-lock operations")
                .build(),
            lock_op_duration: meter
                .f64_histogram(names::LOCK_OP_DURATION_SECONDS)
                .with_description("Cluster distributed-lock operation latency")
                .with_unit("s")
                .with_boundaries(DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
            leader_transitions: meter
                .u64_counter(counter_name(names::LEADER_TRANSITIONS_TOTAL))
                .with_description("Cluster leadership transitions")
                .build(),
            discovery_ops: meter
                .u64_counter(counter_name(names::DISCOVERY_OPS_TOTAL))
                .with_description("Total cluster service-discovery operations")
                .build(),
            watch_resets: meter
                .u64_counter(counter_name(names::WATCH_RESETS_TOTAL))
                .with_description("Cluster watch resets / resubscriptions")
                .build(),
            provider_errors: meter
                .u64_counter(counter_name(names::PROVIDER_ERRORS_TOTAL))
                .with_description("Cluster backend/provider errors")
                .build(),
        }
    }

    /// Builds the sink over the process-global meter provider, under the
    /// `cf-gears-cluster` instrumentation scope.
    #[must_use]
    pub fn from_global_meter(provider: &'static str) -> Self {
        let scope = InstrumentationScope::builder(SCOPE).build();
        Self::new(&opentelemetry::global::meter_with_scope(scope), provider)
    }

    fn provider_label(&self) -> KeyValue {
        KeyValue::new(fields::label::PROVIDER, self.provider)
    }
}

impl ClusterMetrics for OtelClusterMetrics {
    fn cache_op(&self, op: &str, result: &str) {
        self.cache_ops.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::OP, op.to_owned()),
                KeyValue::new(fields::label::RESULT, result.to_owned()),
            ],
        );
    }

    fn cache_op_duration(&self, op: &str, seconds: f64) {
        self.cache_op_duration.record(
            seconds,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::OP, op.to_owned()),
            ],
        );
    }

    fn lock_op(&self, op: &str, result: &str) {
        self.lock_ops.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::OP, op.to_owned()),
                KeyValue::new(fields::label::RESULT, result.to_owned()),
            ],
        );
    }

    fn lock_op_duration(&self, op: &str, seconds: f64) {
        self.lock_op_duration.record(
            seconds,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::OP, op.to_owned()),
            ],
        );
    }

    fn leader_transition(&self, transition: &str) {
        self.leader_transitions.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::TRANSITION, transition.to_owned()),
            ],
        );
    }

    fn discovery_op(&self, op: &str, result: &str) {
        self.discovery_ops.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::OP, op.to_owned()),
                KeyValue::new(fields::label::RESULT, result.to_owned()),
            ],
        );
    }

    fn watch_reset(&self, primitive: &str) {
        self.watch_resets.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::PRIMITIVE, primitive.to_owned()),
            ],
        );
    }

    fn provider_error(&self, kind: &str) {
        self.provider_errors.add(
            1,
            &[
                self.provider_label(),
                KeyValue::new(fields::label::KIND, kind.to_owned()),
            ],
        );
    }
}
