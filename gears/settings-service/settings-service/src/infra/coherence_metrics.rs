//! The cache broadcast's counter, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

use crate::domain::coherence::BroadcastMetrics;

/// `settings_cache_broadcast_total{result}`: `sent` for a broadcast the broker
/// accepted, `skipped` for one never attempted, `failed` for one refused or
/// not accepted in time. The label never carries a key, a tenant or a value.
pub struct OtelBroadcastMetrics {
    broadcasts: Counter<u64>,
}

impl OtelBroadcastMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            broadcasts: meter
                .u64_counter("settings_cache_broadcast_total")
                .with_description("cache_invalidate broadcasts by result")
                .build(),
        }
    }
}

impl Default for OtelBroadcastMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastMetrics for OtelBroadcastMetrics {
    fn broadcast(&self, result: &'static str) {
        self.broadcasts.add(1, &[KeyValue::new("result", result)]);
    }
}
