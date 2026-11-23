//! OpenTelemetry instruments for the client-side cache.

use opentelemetry::metrics::{Counter, Meter, UpDownCounter};
use opentelemetry::{InstrumentationScope, KeyValue, global};

use super::store::Usage;

const SCOPE: &str = "cf-gears-types-registry-sdk";

pub(super) struct CacheMetrics {
    plane: KeyValue,
    lookups: Counter<u64>,
    evictions: Counter<u64>,
    entries: UpDownCounter<i64>,
    size: UpDownCounter<i64>,
}

impl CacheMetrics {
    pub(super) fn from_global(plane: &'static str) -> Self {
        let meter = global::meter_with_scope(InstrumentationScope::builder(SCOPE).build());
        Self::new(&meter, KeyValue::new("plane", plane))
    }

    fn new(meter: &Meter, plane: KeyValue) -> Self {
        Self {
            plane,
            lookups: meter
                .u64_counter("types_registry_client_cache_lookups")
                .with_unit("{key}")
                .with_description("Distinct keys by cache lookup outcome")
                .build(),
            evictions: meter
                .u64_counter("types_registry_client_cache_evictions")
                .with_unit("{entry}")
                .with_description("Cache entries evicted to enforce capacity limits")
                .build(),
            entries: meter
                .i64_up_down_counter("types_registry_client_cache_entries")
                .with_unit("{entry}")
                .with_description("Resident cache entries summed across client instances")
                .build(),
            size: meter
                .i64_up_down_counter("types_registry_client_cache_size")
                .with_unit("By")
                .with_description(
                    "Bytes charged against max_size_bytes summed across client instances",
                )
                .build(),
        }
    }

    #[cfg(test)]
    pub(super) fn use_test_meter(&mut self, meter: &Meter) {
        *self = Self::new(meter, self.plane.clone());
    }

    pub(super) fn lookups(&self, operation: &'static str, counts: &[(&'static str, usize)]) {
        for &(result, count) in counts {
            if count != 0 {
                self.lookups.add(
                    count as u64,
                    &[
                        self.plane.clone(),
                        KeyValue::new("operation", operation),
                        KeyValue::new("result", result),
                    ],
                );
            }
        }
    }

    pub(super) fn resolutions(&self, hits: usize, misses: usize) {
        self.lookups("batch_resolve", &[("hit", hits), ("miss", misses)]);
    }

    pub(super) fn evictions(&self, cache: &'static str, count: u64) {
        if count != 0 {
            self.evictions
                .add(count, &[self.plane.clone(), KeyValue::new("cache", cache)]);
        }
    }

    pub(super) fn update_usage(&self, before: Usage, after: Usage) {
        for (cache, before, after) in [
            ("snapshot", before.snapshots, after.snapshots),
            ("identity", before.identities, after.identities),
        ] {
            let change = delta(before, after);
            if change != 0 {
                self.entries
                    .add(change, &[self.plane.clone(), KeyValue::new("cache", cache)]);
            }
        }
        let bytes = delta(before.snapshot_bytes, after.snapshot_bytes);
        if bytes != 0 {
            self.size.add(bytes, std::slice::from_ref(&self.plane));
        }
    }
}

fn delta(before: usize, after: usize) -> i64 {
    i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(i64::MAX)
}
