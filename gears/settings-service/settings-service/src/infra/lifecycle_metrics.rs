// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-needs-review-gauge:p2
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-retention:p1
//! What the managed lifecycle's passes report, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Meter};

use crate::domain::declaration::DeclarationStatus;
use crate::domain::metric_labels::{DeclarationSource, RetentionResult, ScopeClass, ScopeKind};
use crate::domain::ports::LifecycleMetrics;

/// The lifecycle's signals.
///
/// `settings_needs_review` by declaration source: overrides flagged
/// `needs_review` and awaiting an administrator's fix. A flagged override
/// falls through on read without an error, so this is the signal an operator
/// alerts on; which settings and scopes are affected is the needs-review
/// listing's to show.
///
/// `settings_audit_retention_passes_total` by `result`, and
/// `settings_audit_records_pruned_total`: a retention pass that failed and one
/// that had nothing to prune both delete nothing, and the log line of the
/// first comes once a day. An alert on `result="failed"`, or on no `ok` for
/// two days, is what notices a pass that has stopped working.
///
/// `settings_service_ready`: 1 while the lifecycle runs, 0 once it is told to
/// stop. `settings_declarations` by `source` and `status`, and
/// `settings_values` by `scope_class` and `scope_kind`: the inventory,
/// refreshed on the review tick.
pub struct OtelLifecycleMetrics {
    up: Gauge<u64>,
    declarations: Gauge<u64>,
    values: Gauge<u64>,
    flagged: Gauge<u64>,
    retention_passes: Counter<u64>,
    pruned: Counter<u64>,
}

impl OtelLifecycleMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        Self::on(&opentelemetry::global::meter("settings-service"))
    }

    /// On `meter`: the gear's in production, a test's own provider in tests.
    #[must_use]
    pub fn on(meter: &Meter) -> Self {
        Self {
            up: meter
                .u64_gauge("settings_service_ready")
                .with_description("1 while the managed lifecycle runs, 0 once it is told to stop")
                .build(),
            declarations: meter
                .u64_gauge("settings_declarations")
                .with_description("Declarations, by source and status")
                .build(),
            values: meter
                .u64_gauge("settings_values")
                .with_description("Stored overrides, by scope class and scope kind")
                .build(),
            flagged: meter
                .u64_gauge("settings_needs_review")
                .with_description("Overrides flagged needs_review, by declaration source")
                .build(),
            retention_passes: meter
                .u64_counter("settings_audit_retention_passes_total")
                .with_description("Audit retention passes, by result")
                .build(),
            pruned: meter
                .u64_counter("settings_audit_records_pruned_total")
                .with_description("Audit records deleted past their retention horizon")
                .build(),
        }
    }
}

impl Default for OtelLifecycleMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleMetrics for OtelLifecycleMetrics {
    fn needs_review(&self, source: DeclarationSource, count: u64) {
        self.flagged
            .record(count, &[KeyValue::new("source", source.as_str())]);
    }

    fn retention_pass(&self, result: RetentionResult, pruned: u64) {
        self.retention_passes
            .add(1, &[KeyValue::new("result", result.as_str())]);
        self.pruned.add(pruned, &[]);
    }

    fn service_up(&self, up: bool) {
        self.up.record(u64::from(up), &[]);
    }

    fn declarations(&self, source: DeclarationSource, status: DeclarationStatus, count: u64) {
        self.declarations.record(
            count,
            &[
                KeyValue::new("source", source.as_str()),
                KeyValue::new("status", status.as_str()),
            ],
        );
    }

    fn values(&self, scope_class: ScopeClass, scope_kind: ScopeKind, count: u64) {
        self.values.record(
            count,
            &[
                KeyValue::new("scope_class", scope_class.as_str()),
                KeyValue::new("scope_kind", scope_kind.as_str()),
            ],
        );
    }
}

#[cfg(test)]
#[path = "lifecycle_metrics_tests.rs"]
mod lifecycle_metrics_tests;
