//! Helpers for the proxy configuration cache's tests: metrics that keep what
//! the cache records and, with the `cluster` feature, a cluster ([`cluster`]).

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{CacheEviction, CacheLookup, CacheMap};

#[cfg(feature = "cluster")]
mod cluster;

#[cfg(feature = "cluster")]
pub(crate) use cluster::{
    BACKEND_LOG_KEY, ClusterFixture, ScriptedCache, connection_lost, facade, publisher,
    publisher_recording, publisher_reporting, put_log, read_log, resolver, until, upstream,
};

/// Metrics that keep what the proxy configuration cache records: each
/// counter's total by its labels, and each section's last entries value.
#[derive(Default)]
pub(crate) struct CacheMetrics {
    counts: Mutex<HashMap<String, u64>>,
    entries: Mutex<HashMap<String, u64>>,
}

impl CacheMetrics {
    /// The total of a counter, named by its short name and label values in
    /// order, e.g. `lookups alias hit` or `flushes log_gap`.
    pub(crate) fn count(&self, counter: &str) -> u64 {
        self.counts.lock().get(counter).copied().unwrap_or(0)
    }

    /// Changes not appended to the change log, whatever the reason.
    #[cfg(feature = "cluster")]
    pub(crate) fn publish_failures(&self) -> u64 {
        self.counts
            .lock()
            .iter()
            .filter(|(counter, _)| counter.starts_with("publish_failures "))
            .map(|(_, n)| n)
            .sum()
    }

    /// Every counter total recorded, by name and labels.
    #[cfg(feature = "cluster")]
    pub(crate) fn counts(&self) -> HashMap<String, u64> {
        self.counts.lock().clone()
    }

    /// The last entries value recorded for section `map`.
    pub(crate) fn entries(&self, map: &str) -> Option<u64> {
        self.entries.lock().get(map).copied()
    }

    fn add(&self, counter: String, n: u64) {
        *self.counts.lock().entry(counter).or_default() += n;
    }
}

impl OagwMetricsPort for CacheMetrics {
    fn record_request(&self, _: &str, _: &str, _: &str, _: u16) {}
    fn record_error(&self, _: &str, _: &str, _: &str) {}
    fn record_request_duration_seconds(&self, _: &str, _: &str, _: &str, _: f64) {}
    fn increment_in_flight(&self, _: &str) {}
    fn decrement_in_flight(&self, _: &str) {}
    fn record_rate_limit_exceeded(&self, _: &str, _: &str) {}
    fn record_rate_limit_usage_ratio(&self, _: &str, _: &str, _: f64) {}
    fn increment_active_websocket_sessions(&self, _: &str) {}
    fn decrement_active_websocket_sessions(&self, _: &str) {}
    fn record_websocket_session_duration_seconds(&self, _: &str, _: f64) {}
    fn record_rate_limit_sweep(&self, _: u64, _: u64) {}
    fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
    fn record_registry_provisioning_run(&self, _: &str) {}
    fn record_proxy_cache_lookup(&self, map: CacheMap, result: CacheLookup) {
        self.add(format!("lookups {} {}", map.as_str(), result.as_str()), 1);
    }
    fn record_proxy_cache_entries(&self, map: CacheMap, entries: u64) {
        self.entries.lock().insert(map.as_str().to_owned(), entries);
    }
    fn record_proxy_cache_evictions(&self, map: CacheMap, reason: CacheEviction, count: u64) {
        self.add(
            format!("evictions {} {}", map.as_str(), reason.as_str()),
            count,
        );
    }
    fn record_proxy_cache_change(&self, source: &str) {
        self.add(format!("changes {source}"), 1);
    }
    fn record_proxy_cache_flush(&self, cause: &str) {
        self.add(format!("flushes {cause}"), 1);
    }
    fn record_proxy_cache_load_discarded(&self, map: CacheMap) {
        self.add(format!("discarded {}", map.as_str()), 1);
    }
    fn record_proxy_cache_publish_failures(&self, reason: &str, count: u64) {
        self.add(format!("publish_failures {reason}"), count);
    }
}
