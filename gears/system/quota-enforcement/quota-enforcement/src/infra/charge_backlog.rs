//! `ChargeBacklogCell`: the [`ChargeBacklogSink`] the
//! `allocation_charges_past_replay_window` and
//! `allocation_charge_max_age_seconds` gauges read from.
//!
//! The elected retention sweeper stores a sample after each cycle; the gauge
//! callbacks load it lock-free and observe one point per metric, and nothing
//! while no sample is published.

use std::sync::Arc;

use arc_swap::ArcSwapOption;

use crate::domain::ports::metrics::{ChargeBacklog, ChargeBacklogSink};

/// Catalogue name of the gauge of held allocation charges.
pub const ALLOCATION_CHARGES_PAST_REPLAY_WINDOW: &str = "allocation_charges_past_replay_window";

/// Catalogue name of the gauge of the oldest held allocation charge's age.
pub const ALLOCATION_CHARGE_MAX_AGE_SECONDS: &str = "allocation_charge_max_age_seconds";

/// The published sample, or nothing.
#[derive(Debug, Default)]
pub struct ChargeBacklogCell(ArcSwapOption<ChargeBacklog>);

impl ChargeBacklogCell {
    /// The current sample, if one is published.
    #[must_use]
    pub fn load(&self) -> Option<Arc<ChargeBacklog>> {
        self.0.load_full()
    }
}

impl ChargeBacklogSink for ChargeBacklogCell {
    fn publish(&self, backlog: Option<ChargeBacklog>) {
        self.0.store(backlog.map(Arc::new));
    }
}
