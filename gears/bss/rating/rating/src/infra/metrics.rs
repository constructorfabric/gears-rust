//! The OpenTelemetry adapter of the metrics port (DESIGN §4.7). Instrument names are the literal
//! Prometheus names (`_total` counters); the collector runs without metric suffixes.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};
use crate::domain::usage::CaptureRejectKind;
use crate::domain::usage::intake::PageReport;
use crate::domain::usage::loss::LossKind;
use crate::domain::usage::metrics::UsageMetricsPort;
use crate::domain::usage::normalize::RejectReason;
use crate::domain::usage::reader::{FeedPosition, PauseReason};
use crate::domain::usage::source::SourceId;

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
/// `rating_feed_lag_seconds{source}`.
pub const FEED_LAG: &str = "rating_feed_lag_seconds";
/// `rating_feed_cursor_age_seconds{source}`.
pub const FEED_CURSOR_AGE: &str = "rating_feed_cursor_age_seconds";
/// `rating_feed_oldest_unread_age_seconds{source}`.
pub const FEED_OLDEST_UNREAD_AGE: &str = "rating_feed_oldest_unread_age_seconds";
/// `rating_feed_restart_total{source}`.
pub const FEED_RESTARTS: &str = "rating_feed_restart_total";
/// `rating_feed_paused{source, reason}`.
pub const FEED_PAUSED: &str = "rating_feed_paused";
/// `rating_usage_rejected_total{reason}`.
pub const USAGE_REJECTED: &str = "rating_usage_rejected_total";
/// `rating_usage_duplicate_total`.
pub const USAGE_DUPLICATES: &str = "rating_usage_duplicate_total";
/// `rating_usage_content_conflict_total`.
pub const USAGE_CONTENT_CONFLICTS: &str = "rating_usage_content_conflict_total";
/// `rating_usage_natural_key_conflict_total`.
pub const USAGE_NATURAL_KEY_CONFLICTS: &str = "rating_usage_natural_key_conflict_total";
/// `rating_usage_capture_rejected_total{reason}`.
pub const USAGE_CAPTURE_REJECTED: &str = "rating_usage_capture_rejected_total";
/// `rating_source_loss_open{source, kind}`.
pub const SOURCE_LOSS_OPEN: &str = "rating_source_loss_open";

/// Buckets (seconds) of `rating_exception_sweep_duration_seconds`: from a sweep with nothing to
/// retry (milliseconds) to one that walks many tenants' pages, up to the 15-minute sweep interval
/// a sweep should never reach. The SDK's default buckets are sized for milliseconds and would put
/// every sweep of up to five seconds in one bucket.
pub const EXCEPTION_SWEEP_DURATION_BUCKETS_SECONDS: [f64; 13] = [
    0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0,
];

/// The meter name of the gear.
pub const METER_NAME: &str = "bss-rating";

/// `rating_*` instruments on an OpenTelemetry meter.
pub struct OtelRatingMetrics {
    inbox_quarantined: Counter<u64>,
    dependency_errors: Counter<u64>,
    exception_sweeps: Counter<u64>,
    exception_sweep_retries: Counter<u64>,
    exception_sweep_duration: Histogram<f64>,
    feed_lag: Gauge<f64>,
    feed_cursor_age: Gauge<f64>,
    feed_oldest_unread_age: Gauge<f64>,
    feed_restarts: Counter<u64>,
    feed_paused: Gauge<u64>,
    usage_rejected: Counter<u64>,
    usage_duplicates: Counter<u64>,
    usage_content_conflicts: Counter<u64>,
    usage_natural_key_conflicts: Counter<u64>,
    usage_capture_rejected: Counter<u64>,
    source_loss_open: Gauge<u64>,
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
                .with_boundaries(EXCEPTION_SWEEP_DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
            feed_lag: meter
                .f64_gauge(FEED_LAG)
                .with_unit("s")
                .with_description(
                    "Now minus the latest accepted_at of the last page read; 0 when caught up",
                )
                .build(),
            feed_cursor_age: meter
                .f64_gauge(FEED_CURSOR_AGE)
                .with_unit("s")
                .with_description("Time since the source's cursor last advanced")
                .build(),
            feed_oldest_unread_age: meter
                .f64_gauge(FEED_OLDEST_UNREAD_AGE)
                .with_unit("s")
                .with_description(
                    "Now minus the earliest window_end of the last page read while not caught up",
                )
                .build(),
            feed_restarts: meter
                .u64_counter(FEED_RESTARTS)
                .with_description("Sources restarted from Oldest after a cursor beyond retention")
                .build(),
            feed_paused: meter
                .u64_gauge(FEED_PAUSED)
                .with_description("1 while a source is paused, by reason")
                .build(),
            usage_rejected: meter
                .u64_counter(USAGE_REJECTED)
                .with_description("Usage entries stored rejected, by reason")
                .build(),
            usage_duplicates: meter
                .u64_counter(USAGE_DUPLICATES)
                .with_description("Re-read usage entries with the stored digest")
                .build(),
            usage_content_conflicts: meter
                .u64_counter(USAGE_CONTENT_CONFLICTS)
                .with_description("Re-read usage entries with a different digest")
                .build(),
            usage_natural_key_conflicts: meter
                .u64_counter(USAGE_NATURAL_KEY_CONFLICTS)
                .with_description("Usage entries with a new id and a stored entry's natural key")
                .build(),
            usage_capture_rejected: meter
                .u64_counter(USAGE_CAPTURE_REJECTED)
                .with_description(
                    "Usage entries set aside unread: unidentifiable, or outside the subscription",
                )
                .build(),
            source_loss_open: meter
                .u64_gauge(SOURCE_LOSS_OPEN)
                .with_description("Source losses not resolved, by source and kind")
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

impl UsageMetricsPort for OtelRatingMetrics {
    fn feed_page(&self, _source: SourceId, report: &PageReport) {
        let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        if report.duplicates > 0 {
            self.usage_duplicates.add(count(report.duplicates), &[]);
        }
        if report.content_conflicts > 0 {
            self.usage_content_conflicts
                .add(count(report.content_conflicts), &[]);
        }
        for (reason, rejected) in &report.rejected {
            if *reason == RejectReason::NaturalKeyConflict {
                self.usage_natural_key_conflicts.add(count(*rejected), &[]);
            }
            self.usage_rejected.add(
                count(*rejected),
                &[KeyValue::new("reason", reason.metric_label())],
            );
        }
        for (reason, set_aside) in [
            (CaptureRejectKind::UNIDENTIFIABLE, report.unidentifiable),
            (
                CaptureRejectKind::OUTSIDE_SUBSCRIPTION,
                report.outside_subscription,
            ),
        ] {
            if set_aside > 0 {
                self.usage_capture_rejected
                    .add(count(set_aside), &[KeyValue::new("reason", reason)]);
            }
        }
    }

    fn feed_position(&self, source: SourceId, position: FeedPosition) {
        let label = [KeyValue::new("source", source.to_string())];
        self.feed_lag.record(position.lag.as_secs_f64(), &label);
        self.feed_cursor_age
            .record(position.cursor_age.as_secs_f64(), &label);
        self.feed_oldest_unread_age
            .record(position.oldest_unread_age.as_secs_f64(), &label);
    }

    fn feed_restart(&self, source: SourceId) {
        self.feed_restarts
            .add(1, &[KeyValue::new("source", source.to_string())]);
    }

    fn feed_paused(&self, source: SourceId, reason: PauseReason, paused: bool) {
        self.feed_paused.record(
            u64::from(paused),
            &[
                KeyValue::new("source", source.to_string()),
                KeyValue::new("reason", reason.as_str()),
            ],
        );
    }

    fn source_losses(&self, source: SourceId, kind: LossKind, unresolved: u64) {
        self.source_loss_open.record(
            unresolved,
            &[
                KeyValue::new("source", source.to_string()),
                KeyValue::new("kind", kind.as_str()),
            ],
        );
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
