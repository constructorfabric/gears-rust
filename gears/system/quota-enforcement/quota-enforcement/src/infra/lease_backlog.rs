//! `LeaseBacklogCell`: the [`LeaseBacklogSink`] the `lease_unreclaimed_expired`
//! gauge reads from.
//!
//! The elected lease sweeper stores a sample after each cycle; the gauge
//! callback loads it lock-free and observes one point per metric, and nothing
//! while no sample is published. Storage I/O stays in the sweeper.

use std::sync::Arc;

use arc_swap::ArcSwapOption;

use crate::domain::ports::metrics::{LeaseBacklog, LeaseBacklogSink};

/// Catalogue name of the expired-but-unreclaimed lease gauge.
pub const LEASE_UNRECLAIMED_EXPIRED: &str = "lease_unreclaimed_expired";

/// The published backlog, or nothing.
#[derive(Debug, Default)]
pub struct LeaseBacklogCell(ArcSwapOption<LeaseBacklog>);

impl LeaseBacklogCell {
    /// The current sample, if one is published.
    #[must_use]
    pub fn load(&self) -> Option<Arc<LeaseBacklog>> {
        self.0.load_full()
    }
}

impl LeaseBacklogSink for LeaseBacklogCell {
    fn publish(&self, backlog: Option<LeaseBacklog>) {
        self.0.store(backlog.map(Arc::new));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lease_backlog_tests.rs"]
mod lease_backlog_tests;
