//! OpenTelemetry implementation of [`DecisionMetrics`], on the meter scope
//! `policy-engine` of the platform's global provider.
//!
//! | Instrument | Kind | Labels |
//! |---|---|---|
//! | `policy_engine_evaluations_total` | counter | `outcome` (`permit`, `deny`, `failure`) |
//! | `policy_engine_evaluation_duration_seconds` | histogram | `outcome` |
//! | `policy_engine_compile_cache_total` | counter | `result` (`hit`, `miss`) |

use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{InstrumentationScope, KeyValue};

use crate::domain::decision::DecisionMetrics;

/// Meter scope of every instrument.
pub const METER_SCOPE: &str = "policy-engine";

const LATENCY_BUCKETS_SECONDS: [f64; 10] =
    [0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0];

/// The gear's OpenTelemetry instruments.
pub struct PolicyEngineMetrics {
    evaluations: Counter<u64>,
    evaluation_duration: Histogram<f64>,
    compile_cache: Counter<u64>,
}

impl std::fmt::Debug for PolicyEngineMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEngineMetrics")
            .field("scope", &METER_SCOPE)
            .finish_non_exhaustive()
    }
}

impl PolicyEngineMetrics {
    /// Instruments on the platform's global meter provider.
    #[must_use]
    pub fn global() -> Self {
        let scope = InstrumentationScope::builder(METER_SCOPE).build();
        let meter = opentelemetry::global::meter_with_scope(scope);
        Self {
            evaluations: meter
                .u64_counter("policy_engine_evaluations_total")
                .with_description("Evaluations by outcome")
                .build(),
            evaluation_duration: meter
                .f64_histogram("policy_engine_evaluation_duration_seconds")
                .with_description("Evaluation latency by outcome")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .build(),
            compile_cache: meter
                .u64_counter("policy_engine_compile_cache_total")
                .with_description("Compiled-version cache lookups by result")
                .build(),
        }
    }
}

impl DecisionMetrics for PolicyEngineMetrics {
    fn evaluation(&self, outcome: &'static str, elapsed: Duration) {
        let labels = [KeyValue::new("outcome", outcome)];
        self.evaluations.add(1, &labels);
        self.evaluation_duration
            .record(elapsed.as_secs_f64(), &labels);
    }

    fn compile_cache(&self, hit: bool) {
        let result = if hit { "hit" } else { "miss" };
        self.compile_cache
            .add(1, &[KeyValue::new("result", result)]);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_instrument_records_without_a_provider() {
        let metrics = PolicyEngineMetrics::global();
        metrics.evaluation("permit", Duration::from_micros(200));
        metrics.evaluation("failure", Duration::from_millis(5));
        metrics.compile_cache(true);
        metrics.compile_cache(false);
        assert!(format!("{metrics:?}").contains("policy-engine"));
    }
}
