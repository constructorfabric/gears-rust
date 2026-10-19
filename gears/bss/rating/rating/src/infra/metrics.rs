//! The OpenTelemetry adapter of the metrics port (DESIGN §4.7). Instrument names are the literal
//! Prometheus names (`_total` counters); the collector runs without metric suffixes.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};

/// `rating_inbox_quarantined_total{source}`.
pub const INBOX_QUARANTINED: &str = "rating_inbox_quarantined_total";
/// `rating_dependency_errors_total{dependency, code}`.
pub const DEPENDENCY_ERRORS: &str = "rating_dependency_errors_total";
/// `rating_exception_sweeps_total{outcome}`.
pub const EXCEPTION_SWEEPS: &str = "rating_exception_sweeps_total";
/// `rating_exception_sweep_retries_total{result}`.
pub const EXCEPTION_SWEEP_RETRIES: &str = "rating_exception_sweep_retries_total";
/// `rating_exception_sweep_duration_seconds`.
pub const EXCEPTION_SWEEP_DURATION: &str = "rating_exception_sweep_duration_seconds";

/// The meter name of the gear.
pub const METER_NAME: &str = "bss-rating";

/// `rating_*` instruments on an OpenTelemetry meter.
pub struct OtelRatingMetrics {
    inbox_quarantined: Counter<u64>,
    dependency_errors: Counter<u64>,
    exception_sweeps: Counter<u64>,
    exception_sweep_retries: Counter<u64>,
    exception_sweep_duration: Histogram<f64>,
}

impl OtelRatingMetrics {
    /// The instruments on `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            inbox_quarantined: meter
                .u64_counter(INBOX_QUARANTINED)
                .with_description("Inbox entries quarantined (digest conflict, unsupported schema)")
                .build(),
            dependency_errors: meter
                .u64_counter(DEPENDENCY_ERRORS)
                .with_description("Failures of the dependencies Rating calls")
                .build(),
            exception_sweeps: meter
                .u64_counter(EXCEPTION_SWEEPS)
                .with_description("Exception sweeps, by how they ended")
                .build(),
            exception_sweep_retries: meter
                .u64_counter(EXCEPTION_SWEEP_RETRIES)
                .with_description("Open exceptions a sweep handed back to their retrier")
                .build(),
            exception_sweep_duration: meter
                .f64_histogram(EXCEPTION_SWEEP_DURATION)
                .with_unit("s")
                .with_description("How long one exception sweep took")
                .build(),
        }
    }

    /// The instruments on the global meter provider.
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }
}

impl RatingMetricsPort for OtelRatingMetrics {
    fn inbox_quarantined(&self, source: InboxSource) {
        self.inbox_quarantined
            .add(1, &[KeyValue::new("source", source.as_str())]);
    }

    fn dependency_error(&self, dependency: Dependency, code: FailureCode) {
        self.dependency_errors.add(
            1,
            &[
                KeyValue::new("dependency", dependency.as_str()),
                KeyValue::new("code", code.as_str()),
            ],
        );
    }

    fn exception_sweep(&self, outcome: SweepOutcome, report: &SweepReport, duration: Duration) {
        self.exception_sweeps
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
        for (result, count) in [("retried", report.retried), ("failed", report.failed)] {
            if count > 0 {
                self.exception_sweep_retries.add(
                    u64::try_from(count).unwrap_or(u64::MAX),
                    &[KeyValue::new("result", result)],
                );
            }
        }
        self.exception_sweep_duration
            .record(duration.as_secs_f64(), &[]);
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
