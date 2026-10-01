//! OpenTelemetry instruments of the gear, on the meter scope
//! `admission-control` of the platform's global provider: verdicts by cause,
//! engine call latency and dropped refusal events. No tenant, subject or
//! resource identifier is ever a label.

use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue};

use crate::domain::service::AdmissionMetrics;

/// Meter scope of every instrument.
pub const METER_SCOPE: &str = "admission-control";

/// Buckets (seconds) of the engine-call histogram.
const ENGINE_BUCKETS_SECONDS: [f64; 10] =
    [0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0];

/// Every OpenTelemetry instrument of the gear.
pub struct AdmissionControlMetrics {
    verdicts: Counter<u64>,
    engine_call: Histogram<f64>,
    events_dropped: Counter<u64>,
}

impl std::fmt::Debug for AdmissionControlMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionControlMetrics")
            .finish_non_exhaustive()
    }
}

impl AdmissionControlMetrics {
    /// Instruments on the platform's global meter provider.
    #[must_use]
    pub fn global() -> Self {
        let scope = InstrumentationScope::builder(METER_SCOPE).build();
        Self::new(&opentelemetry::global::meter_with_scope(scope))
    }

    /// Instruments on `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            verdicts: meter
                .u64_counter("admission_control_verdicts_total")
                .with_description("Decisions by cause (admitted or the refusal cause)")
                .build(),
            engine_call: meter
                .f64_histogram("admission_control_engine_call_seconds")
                .with_description("Wall time of engine calls actually made")
                .with_boundaries(ENGINE_BUCKETS_SECONDS.to_vec())
                .build(),
            events_dropped: meter
                .u64_counter("admission_control_events_dropped_total")
                .with_description("Refusal events dropped (queue full or broker failing)")
                .build(),
        }
    }

    /// One decision, labelled `admitted` or by refusal cause.
    pub fn verdict(&self, cause: &'static str) {
        self.verdicts.add(1, &[KeyValue::new("cause", cause)]);
    }

    /// Wall time of one engine call.
    pub fn engine_latency(&self, elapsed: Duration) {
        self.engine_call.record(elapsed.as_secs_f64(), &[]);
    }

    /// One refusal event was dropped.
    pub fn event_dropped(&self) {
        self.events_dropped.add(1, &[]);
    }
}

impl AdmissionMetrics for AdmissionControlMetrics {
    fn verdict(&self, cause: &'static str) {
        Self::verdict(self, cause);
    }

    fn engine_latency(&self, elapsed: Duration) {
        Self::engine_latency(self, elapsed);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_instrument_records_without_a_provider() {
        let metrics = AdmissionControlMetrics::global();
        metrics.verdict("policy");
        metrics.engine_latency(Duration::from_millis(3));
        metrics.event_dropped();
    }
}
