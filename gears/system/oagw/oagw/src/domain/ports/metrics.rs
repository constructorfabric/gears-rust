//! Output port for recording OAGW operational metrics.
//!
//! Implementations live in `infra/metrics.rs` (OpenTelemetry instruments).
//! Domain code depends only on this trait — no knowledge of OTel.
//!
//! ## `_total` suffix
//!
//! Counter instrument names intentionally omit the `_total` suffix that
//! appears in Prometheus metric names. The `opentelemetry-prometheus`
//! exporter appends `_total` automatically for counters, so including it
//! here would produce a doubled `_total_total` suffix when scraped.
//!
//! ## Cardinality control
//!
//! Per [feature 0008](../../../../docs/features/0008-cpt-cf-oagw-feature-observability.md):
//!
//! - `host` is the upstream **alias** (not the raw `Host` header)
//! - `path` is the route **match pattern** (not the raw request path)
//! - `http.response.status_code` is the numeric upstream status (OTel HTTP semconv)
//! - no `tenant_id` label anywhere on the surface
//!
//! The proxy configuration cache's labels are typed ([`CacheMap`],
//! [`CacheLookup`], [`CacheEviction`], [`CacheStale`]) where a method takes two, so they
//! cannot be swapped, as are [`RateLimitBucket`] and [`StreamOutcome`]; the
//! rest are `&'static str` constants from
//! [`super::metric_labels`], so the hot path never copies a label.
use toolkit_macros::domain_model;

use super::metric_labels::{
    CacheEviction, CacheLookup, CacheMap, CacheStale, RateLimitBucket, StreamOutcome,
};

/// Output port for recording OAGW operational metrics.
pub trait OagwMetricsPort: Send + Sync {
    /// `{prefix}_requests_total` — counter
    ///
    /// Incremented once per proxy request when its response head is known:
    /// the upstream's status (4xx/5xx included), or for a gateway error the
    /// status the client gets. A denied `proxy:invoke` check, and a granted
    /// request the REST handler rejects before proxying it (a bad path, a
    /// body over the limit, a failed body read), count with `host`/`path`
    /// `"unknown"`. A request whose future was dropped first
    /// (the client went away) is not counted here, only in
    /// [`Self::record_error`] as `Cancelled`. A response the REST handler
    /// fails to send after the Data Plane returned it (a 101 without its
    /// tunnel, a client connection that cannot upgrade) stays counted with
    /// the Data Plane's status.
    /// `credential_mode` labels `config` or `request_ref` (ADR-0022);
    /// `None` labels `unknown`.
    fn record_request(
        &self,
        host: &str,
        path: &str,
        method: &str,
        status_code: u16,
        credential_mode: Option<crate::domain::plugin::credentials::CredentialMode>,
    );

    /// `{prefix}_errors_total` — counter
    ///
    /// Incremented on every `DomainError` returned from the proxy pipeline,
    /// and as `Cancelled` for a request dropped before its response head.
    /// Upstream error responses are not errors here. `host`/`path` may be
    /// `"unknown"` when the failure occurred before upstream/route
    /// resolution.
    /// `credential_mode` labels `config` or `request_ref`; `None` (before
    /// the upstream resolves) labels `unknown` (ADR-0022). Typed, so it
    /// cannot be swapped with the string labels beside it.
    fn record_error(
        &self,
        host: &str,
        path: &str,
        error_type: &str,
        credential_mode: Option<crate::domain::plugin::credentials::CredentialMode>,
    );

    /// `{prefix}_request_duration_seconds` — histogram
    ///
    /// Phase `total` ends at the response head (or the gateway error), not
    /// at the end of the body: see [`Self::record_stream_duration_seconds`].
    /// Buckets are configured by the infra implementation to match the
    /// feature-doc spec (12 buckets, 1ms → 10s).
    fn record_request_duration_seconds(&self, host: &str, path: &str, phase: &str, seconds: f64);

    /// `{prefix}_stream_duration_seconds` — histogram
    ///
    /// Recorded once per relayed response body when it ends, errors or is
    /// dropped: request start to that moment. Bodies are relayed as
    /// streams, so this covers SSE and every other upstream body. A body
    /// with a `Content-Length` is `completed` once that many bytes went
    /// through; the gateway's stop ends one as `shutdown`, an idle cut or an
    /// upstream failure as `error`. A response without a body (101, HEAD,
    /// 204, 304, `Content-Length: 0`) records none.
    fn record_stream_duration_seconds(
        &self,
        host: &str,
        path: &str,
        outcome: StreamOutcome,
        seconds: f64,
    );

    /// `{prefix}_requests_in_flight` — gauge increment.
    ///
    /// A request is in flight from the start of the proxy pipeline until its
    /// response body ends, errors or is dropped; a request that fails, or
    /// has no body, leaves at its response head. Until the upstream is
    /// resolved it counts under `host` `"unknown"`, then moves to the
    /// upstream's alias.
    fn increment_in_flight(&self, host: &str);

    /// `{prefix}_requests_in_flight` — gauge decrement.
    fn decrement_in_flight(&self, host: &str);

    /// `{prefix}_rate_limit_exceeded_total` — counter
    ///
    /// Incremented when the rate-limit `bucket` rejects a request.
    fn record_rate_limit_exceeded(&self, host: &str, path: &str, bucket: RateLimitBucket);

    /// `{prefix}_rate_limit_usage_ratio` — gauge
    ///
    /// `ratio` is `1.0 - remaining/limit`, clamped to `[0.0, 1.0]`.
    /// Updated on each token consumption (allow or reject) of `bucket`. The
    /// shadowed ancestor routes of one route share the `shadowed_route`
    /// series: it is written once per request with the highest ratio among
    /// them (1.0 when one rejects).
    fn record_rate_limit_usage_ratio(
        &self,
        host: &str,
        path: &str,
        bucket: RateLimitBucket,
        ratio: f64,
    );

    /// `{prefix}_active_websocket_sessions` — gauge increment.
    ///
    /// Tracks **session lifetime** (101 upgrade → bridge teardown), NOT the
    /// handshake window. Distinct from [`Self::increment_in_flight`], which
    /// only covers `proxy_request` scope.
    fn increment_active_websocket_sessions(&self, host: &str);

    /// `{prefix}_active_websocket_sessions` — gauge decrement. Called once
    /// per session at bridge teardown (RAII-guarded — also fires on drop).
    fn decrement_active_websocket_sessions(&self, host: &str);

    /// `{prefix}_websocket_session_duration_seconds` — histogram
    ///
    /// Recorded once per session at bridge teardown, with how it ended:
    /// `completed` when the tunnel closed, from either side; `error` on an
    /// idle timeout or an IO error; `shutdown` on the gateway's stop;
    /// `cancelled` when the bridge task was dropped or panicked. Uses
    /// coarser bucket boundaries than the request histogram (sessions live
    /// seconds → hours).
    fn record_websocket_session_duration_seconds(
        &self,
        host: &str,
        outcome: StreamOutcome,
        seconds: f64,
    );

    /// `{prefix}_rate_limit_buckets_evicted_total` — counter, and
    /// `{prefix}_rate_limit_buckets` — gauge.
    ///
    /// Recorded on every idle-bucket sweep: the buckets it evicted and the
    /// buckets still held in memory afterwards.
    fn record_rate_limit_sweep(&self, evicted: u64, live: u64);

    /// `{prefix}_registry_provisioning_failures_total` — counter
    ///
    /// One registry instance the reconcile skipped (`kind` `upstream` or
    /// `route`, `reason` from [`super::metric_labels::provisioning_reason`]),
    /// or a dropped upstream it kept because routes still use it
    /// (`kept_in_use`).
    fn record_registry_provisioning_failure(&self, kind: &str, reason: &str);

    /// `{prefix}_registry_provisioning_runs_total` — counter
    ///
    /// One reconcile run, by `outcome` from
    /// [`super::metric_labels::provisioning_outcome`].
    fn record_registry_provisioning_run(&self, outcome: &str);

    /// `{prefix}_credentials_withheld_total` — counter
    ///
    /// An ancestor's auth or headers not sent because the resolved upstream
    /// serves the alias on endpoints their owner did not configure, by
    /// `outcome` from [`super::metric_labels::withheld_outcome`].
    fn record_credential_withheld(&self, host: &str, outcome: &'static str);

    /// `{prefix}_proxy_cache_lookups_total` — counter
    ///
    /// One key looked up in the proxy configuration cache section `map`
    /// ([`CacheMap`]), by `result` ([`CacheLookup`]); a batched lookup counts
    /// once per key.
    fn record_proxy_cache_lookup(&self, map: CacheMap, result: CacheLookup);

    /// `{prefix}_proxy_cache_entries` — gauge
    ///
    /// The entries section `map` holds, recorded whenever it changes.
    fn record_proxy_cache_entries(&self, map: CacheMap, entries: u64);

    /// `{prefix}_proxy_cache_evictions_total` — counter
    ///
    /// `count` entries evicted from section `map`, by `reason`
    /// ([`CacheEviction`]).
    fn record_proxy_cache_evictions(&self, map: CacheMap, reason: CacheEviction, count: u64);

    /// `{prefix}_proxy_cache_changes_total` — counter
    ///
    /// One configuration change applied to the cache, by `source`
    /// ([`super::metric_labels::cache_change_source`]).
    fn record_proxy_cache_change(&self, source: &'static str);

    /// `{prefix}_proxy_cache_flushes_total` — counter
    ///
    /// The whole cache emptied because changes may have been missed, by
    /// `cause` ([`super::metric_labels::cache_flush`]). Recorded by the
    /// change log watcher (the `cluster` feature).
    #[cfg_attr(
        all(not(feature = "cluster"), not(test)),
        expect(dead_code, reason = "recorded by the change log watcher")
    )]
    fn record_proxy_cache_flush(&self, cause: &'static str);

    /// `{prefix}_proxy_cache_loads_discarded_total` — counter
    ///
    /// One key of a load in section `map` whose rows were not stored: a
    /// change detached the load while it ran.
    fn record_proxy_cache_load_discarded(&self, map: CacheMap);

    /// `{prefix}_proxy_cache_stale_served_total` — counter
    ///
    /// One key of section `map` answered with a stale entry instead of its
    /// reload's result, by `cause` ([`CacheStale`]).
    fn record_proxy_cache_stale_served(&self, map: CacheMap, cause: CacheStale);

    /// `{prefix}_proxy_cache_publish_failures_total` — counter
    ///
    /// `count` configuration changes that did not reach the cluster change
    /// log, by `reason` ([`super::metric_labels::cache_publish_failure`]).
    /// Each change counts once, when its fate is known: a write that stops
    /// waiting for its append counts nothing, as the append may still land.
    #[cfg_attr(
        all(not(feature = "cluster"), not(test)),
        expect(
            dead_code,
            reason = "recorded by the change log publisher and the write hook's publishing"
        )
    )]
    fn record_proxy_cache_publish_failures(&self, reason: &'static str, count: u64);
}

/// No-op implementation for tests and contexts where metrics are disabled.
#[domain_model]
#[allow(dead_code)] // constructed only by test/test-utils builds
pub struct NoopMetrics;

impl OagwMetricsPort for NoopMetrics {
    fn record_request(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u16,
        _: Option<crate::domain::plugin::credentials::CredentialMode>,
    ) {
    }
    fn record_error(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<crate::domain::plugin::credentials::CredentialMode>,
    ) {
    }
    fn record_request_duration_seconds(&self, _: &str, _: &str, _: &str, _: f64) {}
    fn record_stream_duration_seconds(&self, _: &str, _: &str, _: StreamOutcome, _: f64) {}
    fn increment_in_flight(&self, _: &str) {}
    fn decrement_in_flight(&self, _: &str) {}
    fn record_rate_limit_exceeded(&self, _: &str, _: &str, _: RateLimitBucket) {}
    fn record_rate_limit_usage_ratio(&self, _: &str, _: &str, _: RateLimitBucket, _: f64) {}
    fn increment_active_websocket_sessions(&self, _: &str) {}
    fn decrement_active_websocket_sessions(&self, _: &str) {}
    fn record_websocket_session_duration_seconds(&self, _: &str, _: StreamOutcome, _: f64) {}
    fn record_rate_limit_sweep(&self, _: u64, _: u64) {}
    fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
    fn record_registry_provisioning_run(&self, _: &str) {}
    fn record_credential_withheld(&self, _: &str, _: &'static str) {}
    fn record_proxy_cache_lookup(&self, _: CacheMap, _: CacheLookup) {}
    fn record_proxy_cache_entries(&self, _: CacheMap, _: u64) {}
    fn record_proxy_cache_evictions(&self, _: CacheMap, _: CacheEviction, _: u64) {}
    fn record_proxy_cache_change(&self, _: &str) {}
    fn record_proxy_cache_flush(&self, _: &str) {}
    fn record_proxy_cache_load_discarded(&self, _: CacheMap) {}
    fn record_proxy_cache_stale_served(&self, _: CacheMap, _: CacheStale) {}
    fn record_proxy_cache_publish_failures(&self, _: &str, _: u64) {}
}
