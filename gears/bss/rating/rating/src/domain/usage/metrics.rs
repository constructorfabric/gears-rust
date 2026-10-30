//! The usage feed's part of the metrics port (DESIGN §4.7). It extends [`RatingMetricsPort`], so a
//! reader reports its dependency failures through the same port, and keeps the feed's types out of
//! the shared `domain::ports`.

use crate::domain::ports::RatingMetricsPort;
use crate::domain::usage::intake::PageReport;
use crate::domain::usage::loss::LossKind;
use crate::domain::usage::reader::{FeedPosition, PauseReason};
use crate::domain::usage::source::SourceId;

/// The feed readers' metrics.
pub trait UsageMetricsPort: RatingMetricsPort {
    /// One committed page: `rating_usage_duplicate_total`, `rating_usage_content_conflict_total`,
    /// `rating_usage_natural_key_conflict_total`, `rating_usage_rejected_total{reason}` and
    /// `rating_usage_capture_rejected_total{reason}`.
    fn feed_page(&self, source: SourceId, report: &PageReport);

    /// Where `source` stands after a page: `rating_feed_lag_seconds`,
    /// `rating_feed_cursor_age_seconds` and `rating_feed_oldest_unread_age_seconds`.
    fn feed_position(&self, source: SourceId, position: FeedPosition);

    /// `rating_feed_restart_total{source}`: the cursor was past retention.
    fn feed_restart(&self, source: SourceId);

    /// `rating_feed_paused{source, reason}`: 1 while paused, 0 once resumed.
    fn feed_paused(&self, source: SourceId, reason: PauseReason, paused: bool);

    /// `rating_source_loss_open{source, kind}`: the losses of `source` not resolved.
    fn source_losses(&self, source: SourceId, kind: LossKind, unresolved: u64);
}

impl UsageMetricsPort for crate::domain::ports::NoopMetrics {
    fn feed_page(&self, _source: SourceId, _report: &PageReport) {}

    fn feed_position(&self, _source: SourceId, _position: FeedPosition) {}

    fn feed_restart(&self, _source: SourceId) {}

    fn feed_paused(&self, _source: SourceId, _reason: PauseReason, _paused: bool) {}

    fn source_losses(&self, _source: SourceId, _kind: LossKind, _unresolved: u64) {}
}
