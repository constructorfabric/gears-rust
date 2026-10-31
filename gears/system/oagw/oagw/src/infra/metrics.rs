//! OpenTelemetry-backed implementation of [`OagwMetricsPort`].
//!
//! Holds the active P0 instruments listed on the domain port. Deferred
//! instruments from feature 0008 (circuit breaker, multi-endpoint routing,
//! upstream availability) are NOT declared here yet — they are introduced
//! when their owning feature lands. The proxy configuration cache's
//! `oagw_proxy_cache_*` instruments follow ADR-0020 (feature 0008, *Proxy
//! Configuration Cache*).
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};

use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{
    CacheEviction, CacheLookup, CacheMap, METHOD_OTHER, key,
};

/// Map an HTTP method to a low-cardinality `http.request.method` label value.
///
/// Standard verbs from RFC 9110 are returned verbatim; anything else collapses
/// to [`METHOD_OTHER`] (`"_OTHER"`), mirroring `api-gateway`'s normalizer
/// (`gears/system/api-gateway/src/middleware/http_metrics.rs::normalize_method`)
/// so both gears emit the same `http.request.method` vocabulary.
///
/// Lives in the infra layer because the domain layer must not depend on
/// transport-level types like `http::Method` (`cargo gears lint` rules `DE0301`/`DE0308`).
#[must_use]
pub(crate) fn normalize_method(method: &http::Method) -> &'static str {
    match *method {
        http::Method::GET => "GET",
        http::Method::POST => "POST",
        http::Method::PUT => "PUT",
        http::Method::DELETE => "DELETE",
        http::Method::PATCH => "PATCH",
        http::Method::HEAD => "HEAD",
        http::Method::OPTIONS => "OPTIONS",
        http::Method::CONNECT => "CONNECT",
        http::Method::TRACE => "TRACE",
        _ => METHOD_OTHER,
    }
}

/// Histogram bucket boundaries (seconds) for `oagw_request_duration_seconds`.
/// Matches the 12-bucket configuration in feature 0008 §5
/// (`cpt-cf-oagw-dod-obs-prometheus-metrics`).
const DURATION_BUCKETS_SECONDS: [f64; 12] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Histogram bucket boundaries (seconds) for
/// `oagw_websocket_session_duration_seconds`. Sessions live on a different
/// scale than per-request latency — typical sessions span seconds to hours.
const WEBSOCKET_SESSION_BUCKETS_SECONDS: [f64; 8] =
    [1.0, 10.0, 60.0, 300.0, 1_800.0, 3_600.0, 14_400.0, 86_400.0];

/// OpenTelemetry-backed implementation of the OAGW metrics port.
pub struct OagwMetricsMeter {
    requests: Counter<u64>,
    errors: Counter<u64>,
    request_duration_seconds: Histogram<f64>,
    requests_in_flight: UpDownCounter<i64>,
    rate_limit_exceeded: Counter<u64>,
    rate_limit_usage_ratio: Gauge<f64>,
    rate_limit_buckets_evicted: Counter<u64>,
    rate_limit_buckets: Gauge<u64>,
    active_websocket_sessions: UpDownCounter<i64>,
    websocket_session_duration_seconds: Histogram<f64>,
    registry_provisioning_failures: Counter<u64>,
    registry_provisioning_runs: Counter<u64>,
    proxy_cache_lookups: Counter<u64>,
    proxy_cache_entries: Gauge<u64>,
    proxy_cache_evictions: Counter<u64>,
    proxy_cache_changes: Counter<u64>,
    #[cfg_attr(
        all(not(feature = "cluster"), not(test)),
        expect(dead_code, reason = "recorded by the change log watcher")
    )]
    proxy_cache_flushes: Counter<u64>,
    proxy_cache_loads_discarded: Counter<u64>,
    #[cfg_attr(
        all(not(feature = "cluster"), not(test)),
        expect(
            dead_code,
            reason = "recorded by the change log publisher and the write hook's publishing"
        )
    )]
    proxy_cache_publish_failures: Counter<u64>,
}

impl OagwMetricsMeter {
    /// Create all instruments. `prefix` is prepended to every instrument name
    /// (e.g. `"oagw"` → `oagw_requests_total`).
    #[must_use]
    pub fn new(meter: &Meter, prefix: &str) -> Self {
        Self {
            requests: meter
                .u64_counter(format!("{prefix}_requests"))
                .with_description("Total proxy requests processed")
                .build(),
            errors: meter
                .u64_counter(format!("{prefix}_errors"))
                .with_description("Proxy request errors by DomainError variant")
                .build(),
            request_duration_seconds: meter
                .f64_histogram(format!("{prefix}_request_duration_seconds"))
                .with_description("End-to-end proxy request latency (seconds)")
                .with_unit("s")
                .with_boundaries(DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
            requests_in_flight: meter
                .i64_up_down_counter(format!("{prefix}_requests_in_flight"))
                .with_description("Currently in-flight proxy requests")
                .build(),
            rate_limit_exceeded: meter
                .u64_counter(format!("{prefix}_rate_limit_exceeded"))
                .with_description("Requests rejected by an OAGW rate-limit bucket")
                .build(),
            rate_limit_usage_ratio: meter
                .f64_gauge(format!("{prefix}_rate_limit_usage_ratio"))
                .with_description("Current rate-limit bucket usage ratio (1 - remaining/limit)")
                .build(),
            rate_limit_buckets_evicted: meter
                .u64_counter(format!("{prefix}_rate_limit_buckets_evicted"))
                .with_description("Idle rate-limit buckets evicted by the periodic sweep")
                .build(),
            rate_limit_buckets: meter
                .u64_gauge(format!("{prefix}_rate_limit_buckets"))
                .with_description("Rate-limit buckets held in memory after the last sweep")
                .build(),
            active_websocket_sessions: meter
                .i64_up_down_counter(format!("{prefix}_active_websocket_sessions"))
                .with_description("Currently open WebSocket bridge sessions")
                .build(),
            websocket_session_duration_seconds: meter
                .f64_histogram(format!("{prefix}_websocket_session_duration_seconds"))
                .with_description("WebSocket bridge session lifetime (seconds)")
                .with_unit("s")
                .with_boundaries(WEBSOCKET_SESSION_BUCKETS_SECONDS.to_vec())
                .build(),
            registry_provisioning_failures: meter
                .u64_counter(format!("{prefix}_registry_provisioning_failures"))
                .with_description(
                    "Registry instances the reconcile skipped, and dropped upstreams it kept",
                )
                .build(),
            registry_provisioning_runs: meter
                .u64_counter(format!("{prefix}_registry_provisioning_runs"))
                .with_description("Registry reconcile runs by outcome")
                .build(),
            proxy_cache_lookups: meter
                .u64_counter(format!("{prefix}_proxy_cache_lookups"))
                .with_description("Proxy configuration cache lookups by section and result")
                .build(),
            proxy_cache_entries: meter
                .u64_gauge(format!("{prefix}_proxy_cache_entries"))
                .with_description("Entries held by each proxy configuration cache section")
                .build(),
            proxy_cache_evictions: meter
                .u64_counter(format!("{prefix}_proxy_cache_evictions"))
                .with_description("Proxy configuration cache entries evicted, by reason")
                .build(),
            proxy_cache_changes: meter
                .u64_counter(format!("{prefix}_proxy_cache_changes"))
                .with_description("Configuration changes applied to the proxy configuration cache")
                .build(),
            proxy_cache_flushes: meter
                .u64_counter(format!("{prefix}_proxy_cache_flushes"))
                .with_description("Times the whole proxy configuration cache was emptied, by cause")
                .build(),
            proxy_cache_loads_discarded: meter
                .u64_counter(format!("{prefix}_proxy_cache_loads_discarded"))
                .with_description("Proxy configuration cache loads not stored after a change")
                .build(),
            proxy_cache_publish_failures: meter
                .u64_counter(format!("{prefix}_proxy_cache_publish_failures"))
                .with_description(
                    "Proxy configuration changes not appended to the cluster change log",
                )
                .build(),
        }
    }
}

impl OagwMetricsPort for OagwMetricsMeter {
    fn record_request(&self, host: &str, path: &str, method: &str, status_code: u16) {
        self.requests.add(
            1,
            &[
                KeyValue::new(key::HOST, host.to_owned()),
                KeyValue::new(key::PATH, path.to_owned()),
                KeyValue::new(key::METHOD, method.to_owned()),
                KeyValue::new(key::STATUS_CODE, i64::from(status_code)),
            ],
        );
    }

    fn record_error(&self, host: &str, path: &str, error_type: &str) {
        self.errors.add(
            1,
            &[
                KeyValue::new(key::HOST, host.to_owned()),
                KeyValue::new(key::PATH, path.to_owned()),
                KeyValue::new(key::ERROR_TYPE, error_type.to_owned()),
            ],
        );
    }

    fn record_request_duration_seconds(&self, host: &str, path: &str, phase: &str, seconds: f64) {
        self.request_duration_seconds.record(
            seconds,
            &[
                KeyValue::new(key::HOST, host.to_owned()),
                KeyValue::new(key::PATH, path.to_owned()),
                KeyValue::new(key::PHASE, phase.to_owned()),
            ],
        );
    }

    fn increment_in_flight(&self, host: &str) {
        self.requests_in_flight
            .add(1, &[KeyValue::new(key::HOST, host.to_owned())]);
    }

    fn decrement_in_flight(&self, host: &str) {
        self.requests_in_flight
            .add(-1, &[KeyValue::new(key::HOST, host.to_owned())]);
    }

    fn record_rate_limit_exceeded(&self, host: &str, path: &str) {
        self.rate_limit_exceeded.add(
            1,
            &[
                KeyValue::new(key::HOST, host.to_owned()),
                KeyValue::new(key::PATH, path.to_owned()),
            ],
        );
    }

    fn record_rate_limit_usage_ratio(&self, host: &str, path: &str, ratio: f64) {
        let clamped = ratio.clamp(0.0, 1.0);
        self.rate_limit_usage_ratio.record(
            clamped,
            &[
                KeyValue::new(key::HOST, host.to_owned()),
                KeyValue::new(key::PATH, path.to_owned()),
            ],
        );
    }

    fn increment_active_websocket_sessions(&self, host: &str) {
        self.active_websocket_sessions
            .add(1, &[KeyValue::new(key::HOST, host.to_owned())]);
    }

    fn decrement_active_websocket_sessions(&self, host: &str) {
        self.active_websocket_sessions
            .add(-1, &[KeyValue::new(key::HOST, host.to_owned())]);
    }

    fn record_websocket_session_duration_seconds(&self, host: &str, seconds: f64) {
        self.websocket_session_duration_seconds
            .record(seconds, &[KeyValue::new(key::HOST, host.to_owned())]);
    }

    fn record_rate_limit_sweep(&self, evicted: u64, live: u64) {
        self.rate_limit_buckets_evicted.add(evicted, &[]);
        self.rate_limit_buckets.record(live, &[]);
    }

    fn record_registry_provisioning_failure(&self, kind: &str, reason: &str) {
        self.registry_provisioning_failures.add(
            1,
            &[
                KeyValue::new(key::KIND, kind.to_owned()),
                KeyValue::new(key::REASON, reason.to_owned()),
            ],
        );
    }

    fn record_registry_provisioning_run(&self, outcome: &str) {
        self.registry_provisioning_runs
            .add(1, &[KeyValue::new(key::OUTCOME, outcome.to_owned())]);
    }

    // The cache's labels are static: no allocation per lookup.
    fn record_proxy_cache_lookup(&self, map: CacheMap, result: CacheLookup) {
        self.proxy_cache_lookups.add(
            1,
            &[
                KeyValue::new(key::MAP, map.as_str()),
                KeyValue::new(key::RESULT, result.as_str()),
            ],
        );
    }

    fn record_proxy_cache_entries(&self, map: CacheMap, entries: u64) {
        self.proxy_cache_entries
            .record(entries, &[KeyValue::new(key::MAP, map.as_str())]);
    }

    fn record_proxy_cache_evictions(&self, map: CacheMap, reason: CacheEviction, count: u64) {
        self.proxy_cache_evictions.add(
            count,
            &[
                KeyValue::new(key::MAP, map.as_str()),
                KeyValue::new(key::REASON, reason.as_str()),
            ],
        );
    }

    fn record_proxy_cache_change(&self, source: &'static str) {
        self.proxy_cache_changes
            .add(1, &[KeyValue::new(key::SOURCE, source)]);
    }

    fn record_proxy_cache_flush(&self, cause: &'static str) {
        self.proxy_cache_flushes
            .add(1, &[KeyValue::new(key::CAUSE, cause)]);
    }

    fn record_proxy_cache_load_discarded(&self, map: CacheMap) {
        self.proxy_cache_loads_discarded
            .add(1, &[KeyValue::new(key::MAP, map.as_str())]);
    }

    fn record_proxy_cache_publish_failures(&self, reason: &'static str, count: u64) {
        self.proxy_cache_publish_failures
            .add(count, &[KeyValue::new(key::REASON, reason)]);
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
