// @cpt-dod:cpt-cf-settings-service-dod-audit-store-actor-names:p2
//! The actor name lookup's signals, on the process's meter.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram};

use crate::domain::ports::ActorNameMetrics;

/// Duration buckets in seconds: a pass is bounded by the 1 s budget inside a 2 s page deadline.
const LOOKUP_DURATION_BUCKETS_SECONDS: &[f64] =
    &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0];

/// The lookup's signals.
///
/// `settings_actor_name_lookups_total` and
/// `settings_actor_name_lookup_duration_seconds`, both by `result`: `ok`,
/// `degraded` (a chunk failed, timed out or hit the budget) or `skipped` (no
/// lookup was made). A rising `degraded` share is the signal that names are
/// reading `null` because Account Management is slow or failing.
pub struct OtelActorNameMetrics {
    lookups: Counter<u64>,
    duration: Histogram<f64>,
}

impl OtelActorNameMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            lookups: meter
                .u64_counter("settings_actor_name_lookups_total")
                .with_description("Actor name lookup passes on the history read, by result")
                .build(),
            duration: meter
                .f64_histogram("settings_actor_name_lookup_duration_seconds")
                .with_description("Duration of an actor name lookup pass, by result")
                .with_unit("s")
                .with_boundaries(LOOKUP_DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
        }
    }
}

impl Default for OtelActorNameMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ActorNameMetrics for OtelActorNameMetrics {
    fn pass(&self, result: &'static str, duration: Duration) {
        let labels = [KeyValue::new("result", result)];
        self.lookups.add(1, &labels);
        self.duration.record(duration.as_secs_f64(), &labels);
    }
}
