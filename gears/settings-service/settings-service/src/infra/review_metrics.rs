// Created: 2026-09-25 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-needs-review-gauge:p2
//! The needs-review gauge on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::Gauge;

use crate::domain::ports::ReviewMetrics;

/// `settings_needs_review_total` by declaration source: overrides flagged
/// `needs_review` and awaiting an administrator's fix. A flagged override
/// falls through on read without an error, so this is the signal an operator
/// alerts on; which settings and scopes are affected is the needs-review
/// listing's to show.
pub struct OtelReviewMetrics {
    flagged: Gauge<u64>,
}

impl OtelReviewMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            flagged: meter
                .u64_gauge("settings_needs_review_total")
                .with_description("Overrides flagged needs_review, by declaration source")
                .build(),
        }
    }
}

impl Default for OtelReviewMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ReviewMetrics for OtelReviewMetrics {
    fn needs_review(&self, source: &'static str, count: u64) {
        self.flagged
            .record(count, &[KeyValue::new("source", source)]);
    }
}
