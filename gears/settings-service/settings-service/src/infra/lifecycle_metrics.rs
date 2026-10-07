// Created: 2026-09-25 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-needs-review-gauge:p2
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-retention:p1
//! What the managed lifecycle's passes report, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge};

use crate::domain::ports::LifecycleMetrics;

/// The lifecycle's signals.
///
/// `settings_needs_review_total` by declaration source: overrides flagged
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
pub struct OtelLifecycleMetrics {
    flagged: Gauge<u64>,
    retention_passes: Counter<u64>,
    pruned: Counter<u64>,
}

impl OtelLifecycleMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            flagged: meter
                .u64_gauge("settings_needs_review_total")
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
    fn needs_review(&self, source: &'static str, count: u64) {
        self.flagged
            .record(count, &[KeyValue::new("source", source)]);
    }

    fn retention_pass(&self, result: &'static str, pruned: u64) {
        self.retention_passes
            .add(1, &[KeyValue::new("result", result)]);
        self.pruned.add(pruned, &[]);
    }
}
