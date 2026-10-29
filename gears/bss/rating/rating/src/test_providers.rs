//! `ClientHub` provider test doubles (DESIGN 11 §4.12; feature 01 §5.4), behind the
//! `test-providers` feature.
//!
//! Each double stands in for one upstream contract, so every Rating feature can be accepted
//! against it before the provider runs beside Rating. A double never stands in for a production
//! contract (DESIGN §4.10). Feature 04 adds the usage feed (Rating's `UsageFeedPort` over the
//! collector's v2 contract, T-D-84), the meter declaration read, the tenant hierarchy and the
//! queue depth. The doubles of
//! the contracts still undefined — Subscriptions' facts and reads, coverage, the Billing consumer,
//! the derived-declaration read and collector reconciliation — land with the feature that designs
//! their adapter.

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-contracts:p2

pub mod authz;
pub mod cluster;
pub mod pricing;
pub mod usage;

use std::sync::Mutex;
use std::time::Duration;

pub use authz::{AllowShape, EvaluatedSubject, FakePdp};
pub use cluster::FakeClusterLeaders;
pub use pricing::{FakePricingCatalog, PricingCall};
pub use usage::{FakeMeterDeclarations, FakeQueueDepth, FakeTenantHierarchy, FakeUsageFeed};

use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};
use crate::domain::usage::intake::PageReport;
use crate::domain::usage::loss::LossKind;
use crate::domain::usage::metrics::UsageMetricsPort;
use crate::domain::usage::reader::{FeedPosition, PauseReason};
use crate::domain::usage::source::SourceId;

/// One metric sample `RecordingMetrics` captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricSample {
    /// `rating_inbox_quarantined_total{source}`.
    InboxQuarantined(InboxSource),
    /// `rating_dependency_errors_total{dependency, code}`.
    DependencyError(Dependency, FailureCode),
    /// One exception sweep: its outcome and report.
    ExceptionSweep(SweepOutcome, SweepReport),
    /// One committed feed page of a source.
    FeedPage(SourceId, PageReport),
    /// A source's position after a page.
    FeedPosition(SourceId, FeedPosition),
    /// `rating_feed_restart_total{source}`.
    FeedRestart(SourceId),
    /// `rating_feed_paused{source, reason}` set (`true`) or cleared.
    FeedPaused(SourceId, PauseReason, bool),
    /// `rating_source_loss_open{source, kind}`.
    SourceLosses(SourceId, LossKind, u64),
}

/// A metrics port that records every sample.
#[derive(Debug, Default)]
pub struct RecordingMetrics {
    samples: Mutex<Vec<MetricSample>>,
}

impl RecordingMetrics {
    /// The samples so far.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    #[must_use]
    pub fn samples(&self) -> Vec<MetricSample> {
        self.samples.lock().expect("metrics lock").clone()
    }

    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    fn push(&self, sample: MetricSample) {
        self.samples.lock().expect("metrics lock").push(sample);
    }
}

impl RatingMetricsPort for RecordingMetrics {
    fn inbox_quarantined(&self, source: InboxSource) {
        self.push(MetricSample::InboxQuarantined(source));
    }

    fn dependency_error(&self, dependency: Dependency, code: FailureCode) {
        self.push(MetricSample::DependencyError(dependency, code));
    }

    fn exception_sweep(&self, outcome: SweepOutcome, report: &SweepReport, _duration: Duration) {
        self.push(MetricSample::ExceptionSweep(outcome, *report));
    }
}

impl UsageMetricsPort for RecordingMetrics {
    fn feed_page(&self, source: SourceId, report: &PageReport) {
        self.push(MetricSample::FeedPage(source, report.clone()));
    }

    fn feed_position(&self, source: SourceId, position: FeedPosition) {
        self.push(MetricSample::FeedPosition(source, position));
    }

    fn feed_restart(&self, source: SourceId) {
        self.push(MetricSample::FeedRestart(source));
    }

    fn feed_paused(&self, source: SourceId, reason: PauseReason, paused: bool) {
        self.push(MetricSample::FeedPaused(source, reason, paused));
    }

    fn source_losses(&self, source: SourceId, kind: LossKind, unresolved: u64) {
        self.push(MetricSample::SourceLosses(source, kind, unresolved));
    }
}
