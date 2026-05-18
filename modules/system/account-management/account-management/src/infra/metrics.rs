//! OpenTelemetry-backed adapter for the AM metric ports.
//!
//! [`AmMetricsMeter`] owns all OpenTelemetry instruments declared by
//! the AM metric catalog (see [`crate::domain::metrics`]) and
//! implements every port trait from
//! [`crate::domain::ports::metrics`]. A single
//! `Arc<AmMetricsMeter>` is constructed at module init and coerced
//! into per-trait `Arc<dyn ...>` views by DI.
//!
//! The adapter also implements
//! [`crate::domain::metrics::MetricsFacadeBridge`], which lets the
//! existing stringly-typed `emit_metric` / `emit_gauge_value` /
//! `emit_histogram_value` call sites continue to work during the
//! per-family migration. The bridge is removed in the same cleanup
//! PR that retires the last legacy call site.
//!
//! ## Instrument naming
//!
//! Instruments use the dot-separated metric names from
//! [`crate::domain::metrics`] (e.g. `am.dependency_health`) verbatim,
//! with `prefix` substituted for the leading `am` segment. Unit hints
//! are attached via `with_unit("ms")` / `with_unit("s")`; the metric
//! name itself is unit-free, matching `OTel` semantic-convention style.
//! Counters carry no `_total` suffix — the downstream
//! Prometheus exporter appends it.

use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::domain::metrics::{
    AM_BOOTSTRAP_LIFECYCLE, AM_CONVERSION_LIFECYCLE, AM_CROSS_TENANT_DENIAL, AM_DEPENDENCY_HEALTH,
    AM_HIERARCHY_DEPTH_EXCEEDANCE, AM_HIERARCHY_INTEGRITY_DURATION,
    AM_HIERARCHY_INTEGRITY_LAST_FAILURE, AM_HIERARCHY_INTEGRITY_LAST_SUCCESS,
    AM_HIERARCHY_INTEGRITY_REPAIR_RUNS, AM_HIERARCHY_INTEGRITY_REPAIRED,
    AM_HIERARCHY_INTEGRITY_RUNS, AM_HIERARCHY_INTEGRITY_VIOLATIONS, AM_INTEGRITY_LOCK_EVENTS,
    AM_METADATA_RESOLUTION, AM_RETENTION_INVALID_WINDOW, AM_SERIALIZABLE_RETRY,
    AM_TENANT_RETENTION, MetricKind, MetricsFacadeBridge,
};
use crate::domain::ports::metrics::{
    BootstrapClassification, BootstrapMetricsPort, BootstrapOutcome, BootstrapPhase,
    ConversionMetricsPort, ConversionOp, ConversionOutcome, DependencyMetricsPort, DependencyOp,
    DependencyOutcome, DependencyTarget, HierarchyDepthMode, HierarchyDepthOutcome,
    IntegrityBucket, IntegrityLockEvent, IntegrityMetricsPort, IntegrityPhase, IntegrityRunOutcome,
    MetadataMetricsPort, SerializableRetryOutcome, StorageMetricsPort, TenantMetricsPort,
    TenantRetentionJob, TenantRetentionOutcome,
};
use crate::domain::tenant::integrity::IntegrityCategory;

/// OpenTelemetry adapter that owns one instrument per AM metric
/// family and implements every port trait.
///
/// Pre-allocated families that have no live call sites today
/// (`am.cross_tenant_denial`, `am.metadata_resolution`) are still
/// constructed so the catalog shape is uniform and the typed port
/// methods can light up without an adapter change.
pub struct AmMetricsMeter {
    // Counters (12)
    bootstrap_lifecycle: Counter<u64>,
    conversion_lifecycle: Counter<u64>,
    cross_tenant_denial: Counter<u64>,
    dependency_health: Counter<u64>,
    hierarchy_depth_exceedance: Counter<u64>,
    hierarchy_integrity_repair_runs: Counter<u64>,
    hierarchy_integrity_runs: Counter<u64>,
    integrity_lock_events: Counter<u64>,
    metadata_resolution: Counter<u64>,
    retention_invalid_window: Counter<u64>,
    serializable_retry: Counter<u64>,
    tenant_retention: Counter<u64>,

    // Gauges (4)
    hierarchy_integrity_last_failure: Gauge<i64>,
    hierarchy_integrity_last_success: Gauge<i64>,
    hierarchy_integrity_repaired: Gauge<i64>,
    hierarchy_integrity_violations: Gauge<i64>,

    // Histograms (1)
    hierarchy_integrity_duration: Histogram<f64>,
}

impl AmMetricsMeter {
    /// Build every instrument. `prefix` defaults to `"am"` and is the
    /// leading namespace segment of every metric name
    /// (`{prefix}.dependency_health`, etc.).
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn new(meter: &Meter, prefix: &str) -> Self {
        Self {
            // ── Counters ────────────────────────────────────────────
            bootstrap_lifecycle: meter
                .u64_counter(format!("{prefix}.bootstrap_lifecycle"))
                .with_description(
                    "Root-tenant bootstrap saga telemetry by (phase, outcome, classification)",
                )
                .build(),
            conversion_lifecycle: meter
                .u64_counter(format!("{prefix}.conversion_lifecycle"))
                .with_description("Mode-conversion request transitions and outcomes")
                .build(),
            cross_tenant_denial: meter
                .u64_counter(format!("{prefix}.cross_tenant_denial"))
                .with_description("Cross-tenant denial counter (security-alert candidate)")
                .build(),
            dependency_health: meter
                .u64_counter(format!("{prefix}.dependency_health"))
                .with_description(
                    "Outbound dependency-call health (IdP / Resource Group / GTS / AuthZ / \
                     Types Registry / metadata upsert)",
                )
                .build(),
            hierarchy_depth_exceedance: meter
                .u64_counter(format!("{prefix}.hierarchy_depth_exceedance"))
                .with_description("Hierarchy-depth threshold exceedance (advisory + strict)")
                .build(),
            hierarchy_integrity_repair_runs: meter
                .u64_counter(format!("{prefix}.hierarchy_integrity_repair_runs"))
                .with_description("Periodic auto-repair tick outcomes")
                .build(),
            hierarchy_integrity_runs: meter
                .u64_counter(format!("{prefix}.hierarchy_integrity_runs"))
                .with_description("Periodic integrity-check job tick outcomes")
                .build(),
            integrity_lock_events: meter
                .u64_counter(format!("{prefix}.integrity_lock_events"))
                .with_description("Integrity-check lock-lifecycle events (stale-lock sweep)")
                .build(),
            metadata_resolution: meter
                .u64_counter(format!("{prefix}.metadata_resolution"))
                .with_description(
                    "Tenant-metadata resolution operations and inheritance policy outcomes",
                )
                .build(),
            retention_invalid_window: meter
                .u64_counter(format!("{prefix}.retention.invalid_window"))
                .with_description("Invalid retention-window configuration encountered")
                .build(),
            serializable_retry: meter
                .u64_counter(format!("{prefix}.serializable_retry"))
                .with_description("SERIALIZABLE-isolation retry helper outcomes")
                .build(),
            tenant_retention: meter
                .u64_counter(format!("{prefix}.tenant_retention"))
                .with_description(
                    "Provisioning reaper / hard-delete / deprovision background-job telemetry",
                )
                .build(),

            // ── Gauges ─────────────────────────────────────────────
            hierarchy_integrity_last_failure: meter
                .i64_gauge(format!("{prefix}.hierarchy_integrity_last_failure"))
                .with_description("Unix-epoch seconds of last failed integrity-check tick")
                .with_unit("s")
                .build(),
            hierarchy_integrity_last_success: meter
                .i64_gauge(format!("{prefix}.hierarchy_integrity_last_success"))
                .with_description("Unix-epoch seconds of last successful integrity-check tick")
                .with_unit("s")
                .build(),
            hierarchy_integrity_repaired: meter
                .i64_gauge(format!("{prefix}.hierarchy_integrity_repaired"))
                .with_description(
                    "Per-run repair telemetry: one sample per (category, bucket) pair",
                )
                .build(),
            hierarchy_integrity_violations: meter
                .i64_gauge(format!("{prefix}.hierarchy_integrity_violations"))
                .with_description("Hierarchy-integrity violation count per category")
                .build(),

            // ── Histograms ─────────────────────────────────────────
            hierarchy_integrity_duration: meter
                .f64_histogram(format!("{prefix}.hierarchy_integrity_duration"))
                .with_description("Periodic integrity-check tick wall-clock duration")
                .with_unit("ms")
                .build(),
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port-trait implementations
// ════════════════════════════════════════════════════════════════════

impl BootstrapMetricsPort for AmMetricsMeter {
    fn bootstrap_lifecycle(
        &self,
        phase: BootstrapPhase,
        outcome: BootstrapOutcome,
        classification: Option<BootstrapClassification>,
    ) {
        let mut labels: Vec<KeyValue> = vec![
            KeyValue::new("phase", phase.as_str()),
            KeyValue::new("outcome", outcome.as_str()),
        ];
        if let Some(c) = classification {
            labels.push(KeyValue::new("classification", c.as_str()));
        }
        self.bootstrap_lifecycle.add(1, &labels);
    }
}

impl ConversionMetricsPort for AmMetricsMeter {
    fn conversion_lifecycle(&self, op: ConversionOp, outcome: ConversionOutcome) {
        self.conversion_lifecycle.add(
            1,
            &[
                KeyValue::new("op", op.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }
}

impl DependencyMetricsPort for AmMetricsMeter {
    fn dependency_health(
        &self,
        op: DependencyOp,
        target: DependencyTarget,
        outcome: Option<DependencyOutcome>,
    ) {
        let mut labels: Vec<KeyValue> = vec![
            KeyValue::new("op", op.as_str()),
            KeyValue::new("target", target.as_str()),
        ];
        if let Some(o) = outcome {
            labels.push(KeyValue::new("outcome", o.as_str()));
        }
        self.dependency_health.add(1, &labels);
    }
}

impl IntegrityMetricsPort for AmMetricsMeter {
    fn hierarchy_integrity_runs(&self, outcome: IntegrityRunOutcome) {
        self.hierarchy_integrity_runs
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn hierarchy_integrity_repair_runs(&self, outcome: IntegrityRunOutcome) {
        self.hierarchy_integrity_repair_runs
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn hierarchy_integrity_duration_ms(&self, phase: IntegrityPhase, millis: f64) {
        self.hierarchy_integrity_duration
            .record(millis, &[KeyValue::new("phase", phase.as_str())]);
    }

    fn hierarchy_integrity_last_success(&self, epoch_seconds: i64) {
        self.hierarchy_integrity_last_success
            .record(epoch_seconds, &[]);
    }

    fn hierarchy_integrity_last_failure(&self, outcome: IntegrityRunOutcome, epoch_seconds: i64) {
        self.hierarchy_integrity_last_failure
            .record(epoch_seconds, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    fn hierarchy_integrity_violations(&self, category: IntegrityCategory, count: i64) {
        self.hierarchy_integrity_violations
            .record(count, &[KeyValue::new("category", category.as_str())]);
    }

    fn hierarchy_integrity_repaired(
        &self,
        category: IntegrityCategory,
        bucket: IntegrityBucket,
        count: i64,
    ) {
        self.hierarchy_integrity_repaired.record(
            count,
            &[
                KeyValue::new("category", category.as_str()),
                KeyValue::new("bucket", bucket.as_str()),
            ],
        );
    }

    fn integrity_lock_event(&self, event: IntegrityLockEvent) {
        self.integrity_lock_events
            .add(1, &[KeyValue::new("event", event.as_str())]);
    }
}

impl MetadataMetricsPort for AmMetricsMeter {}

impl TenantMetricsPort for AmMetricsMeter {
    fn tenant_retention(&self, job: TenantRetentionJob, outcome: TenantRetentionOutcome) {
        self.tenant_retention.add(
            1,
            &[
                KeyValue::new("job", job.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    fn retention_invalid_window(&self) {
        self.retention_invalid_window.add(1, &[]);
    }

    fn hierarchy_depth_exceedance(
        &self,
        mode: HierarchyDepthMode,
        outcome: HierarchyDepthOutcome,
        threshold: u32,
    ) {
        self.hierarchy_depth_exceedance.add(
            1,
            &[
                KeyValue::new("mode", mode.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
                KeyValue::new("threshold", threshold.to_string()),
            ],
        );
    }

    fn cross_tenant_denial(&self) {
        self.cross_tenant_denial.add(1, &[]);
    }
}

impl StorageMetricsPort for AmMetricsMeter {
    fn serializable_retry(&self, outcome: SerializableRetryOutcome) {
        self.serializable_retry
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }
}

// ════════════════════════════════════════════════════════════════════
//  Stringly-facade bridge (transitional)
// ════════════════════════════════════════════════════════════════════
//
// Forwards `emit_metric` / `emit_gauge_value` / `emit_histogram_value`
// calls onto the right instrument by matching the metric-family
// constant. Removed once every call site has moved to the typed
// port methods.

#[inline]
fn to_kvs(labels: &[(&'static str, &str)]) -> Vec<KeyValue> {
    labels
        .iter()
        .map(|(k, v)| KeyValue::new(*k, (*v).to_owned()))
        .collect()
}

impl MetricsFacadeBridge for AmMetricsMeter {
    fn emit(&self, family: &'static str, kind: MetricKind, labels: &[(&'static str, &str)]) {
        // Only counter-style emissions flow through here; the stringly
        // facade's gauge / histogram emissions go through the
        // dedicated helpers. A non-counter kind reaching this path is
        // a call-site bug — warn in debug, silently no-op in release
        // so a misconfigured caller does not crash the runtime.
        if !matches!(kind, MetricKind::Counter) {
            debug_assert!(
                false,
                "emit_metric called with non-counter kind {kind:?} for family {family}; \
                 use emit_gauge_value / emit_histogram_value instead",
            );
            return;
        }
        let kvs = to_kvs(labels);
        match family {
            AM_BOOTSTRAP_LIFECYCLE => self.bootstrap_lifecycle.add(1, &kvs),
            AM_CONVERSION_LIFECYCLE => self.conversion_lifecycle.add(1, &kvs),
            AM_CROSS_TENANT_DENIAL => self.cross_tenant_denial.add(1, &kvs),
            AM_DEPENDENCY_HEALTH => self.dependency_health.add(1, &kvs),
            AM_HIERARCHY_DEPTH_EXCEEDANCE => self.hierarchy_depth_exceedance.add(1, &kvs),
            AM_HIERARCHY_INTEGRITY_REPAIR_RUNS => self.hierarchy_integrity_repair_runs.add(1, &kvs),
            AM_HIERARCHY_INTEGRITY_RUNS => self.hierarchy_integrity_runs.add(1, &kvs),
            AM_INTEGRITY_LOCK_EVENTS => self.integrity_lock_events.add(1, &kvs),
            AM_METADATA_RESOLUTION => self.metadata_resolution.add(1, &kvs),
            AM_RETENTION_INVALID_WINDOW => self.retention_invalid_window.add(1, &kvs),
            AM_SERIALIZABLE_RETRY => self.serializable_retry.add(1, &kvs),
            AM_TENANT_RETENTION => self.tenant_retention.add(1, &kvs),
            _ => {
                debug_assert!(
                    false,
                    "emit_metric called with unknown counter family {family}"
                );
            }
        }
    }

    fn emit_gauge(&self, family: &'static str, value: i64, labels: &[(&'static str, &str)]) {
        let kvs = to_kvs(labels);
        match family {
            AM_HIERARCHY_INTEGRITY_LAST_FAILURE => {
                self.hierarchy_integrity_last_failure.record(value, &kvs);
            }
            AM_HIERARCHY_INTEGRITY_LAST_SUCCESS => {
                self.hierarchy_integrity_last_success.record(value, &kvs);
            }
            AM_HIERARCHY_INTEGRITY_REPAIRED => {
                self.hierarchy_integrity_repaired.record(value, &kvs);
            }
            AM_HIERARCHY_INTEGRITY_VIOLATIONS => {
                self.hierarchy_integrity_violations.record(value, &kvs);
            }
            _ => {
                debug_assert!(
                    false,
                    "emit_gauge_value called with unknown gauge family {family}"
                );
            }
        }
    }

    fn emit_histogram(&self, family: &'static str, value: f64, labels: &[(&'static str, &str)]) {
        let kvs = to_kvs(labels);
        match family {
            AM_HIERARCHY_INTEGRITY_DURATION => {
                self.hierarchy_integrity_duration.record(value, &kvs);
            }
            _ => {
                debug_assert!(
                    false,
                    "emit_histogram_value called with unknown histogram family {family}",
                );
            }
        }
    }
}

/// Default instrument-name prefix. The leading namespace segment of
/// every `am.*` metric family.
pub const DEFAULT_PREFIX: &str = "am";

/// Convenience constructor used at module init: builds an
/// `Arc<AmMetricsMeter>` against the process-global OpenTelemetry
/// meter provider, scoped to the AM instrumentation library.
#[must_use]
pub fn build_default_adapter() -> Arc<AmMetricsMeter> {
    let scope = opentelemetry::InstrumentationScope::builder("account-management").build();
    let meter = opentelemetry::global::meter_with_scope(scope);
    Arc::new(AmMetricsMeter::new(&meter, DEFAULT_PREFIX))
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
