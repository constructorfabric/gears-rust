//! `ClientHub` provider test doubles (DESIGN 11 §4.12; feature 01 §5.4), behind the
//! `test-providers` feature.
//!
//! Each double stands in for one upstream contract that exists on `main`, so every Rating
//! feature can be accepted against it before the provider runs beside Rating. A double never
//! stands in for a production contract (DESIGN §4.10). The doubles of contracts that are not yet
//! defined — the usage feed, Subscriptions' facts and reads, coverage, the Billing consumer, the
//! derived-declaration read and collector reconciliation — land with the feature that designs
//! their adapter.

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-contracts:p2

pub mod authz;
pub mod cluster;
pub mod pricing;

use std::sync::Mutex;
use std::time::Duration;

pub use authz::{AllowShape, EvaluatedSubject, FakePdp};
pub use cluster::FakeClusterLeaders;
pub use pricing::{FakePricingCatalog, PricingCall};

use crate::domain::exception::SweepReport;
use crate::domain::inbox::InboxSource;
use crate::domain::ports::{Dependency, FailureCode, RatingMetricsPort, SweepOutcome};

/// One metric sample `RecordingMetrics` captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricSample {
    /// `rating_inbox_quarantined_total{source}`.
    InboxQuarantined(InboxSource),
    /// `rating_dependency_errors_total{dependency, code}`.
    DependencyError(Dependency, FailureCode),
    /// One exception sweep: its outcome and report.
    ExceptionSweep(SweepOutcome, SweepReport),
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
    #[allow(clippy::expect_used)]
    #[must_use]
    pub fn samples(&self) -> Vec<MetricSample> {
        self.samples.lock().expect("metrics lock").clone()
    }

    #[allow(clippy::expect_used)]
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
