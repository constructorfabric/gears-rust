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
use modkit_macros::domain_model;

/// Output port for recording OAGW operational metrics.
pub trait OagwMetricsPort: Send + Sync {
    /// `{prefix}_requests_total` — counter
    ///
    /// Incremented once per completed proxy request (success or upstream
    /// 4xx/5xx). Pre-resolution failures (e.g. policy denial before
    /// upstream lookup) are NOT counted here; they go to
    /// [`Self::record_error`] only.
    fn record_request(&self, host: &str, path: &str, method: &str, status_code: u16);

    /// `{prefix}_errors_total` — counter
    ///
    /// Incremented on every `DomainError` returned from the proxy pipeline.
    /// `host`/`path` may be `"unknown"` when the failure occurred before
    /// upstream/route resolution.
    fn record_error(&self, host: &str, path: &str, error_type: &str);

    /// `{prefix}_request_duration_seconds` — histogram
    ///
    /// Buckets are configured by the infra implementation to match the
    /// feature-doc spec (12 buckets, 1ms → 10s).
    fn record_request_duration_seconds(&self, host: &str, path: &str, phase: &str, seconds: f64);

    /// `{prefix}_requests_in_flight` — gauge increment.
    fn increment_in_flight(&self, host: &str);

    /// `{prefix}_requests_in_flight` — gauge decrement.
    fn decrement_in_flight(&self, host: &str);

    /// `{prefix}_rate_limit_exceeded_total` — counter
    ///
    /// Incremented when a configured rate-limit bucket rejects a request
    /// (either upstream- or route-scoped).
    fn record_rate_limit_exceeded(&self, host: &str, path: &str);

    /// `{prefix}_rate_limit_usage_ratio` — gauge
    ///
    /// `ratio` is `1.0 - remaining/limit`, clamped to `[0.0, 1.0]`.
    /// Updated on each rate-limit token consumption (allow or reject).
    fn record_rate_limit_usage_ratio(&self, host: &str, path: &str, ratio: f64);

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
    /// Recorded once per session at bridge teardown. Uses coarser bucket
    /// boundaries than the request histogram (sessions live seconds → hours).
    fn record_websocket_session_duration_seconds(&self, host: &str, seconds: f64);
}

/// No-op implementation for tests and contexts where metrics are disabled.
#[domain_model]
#[allow(dead_code)] // constructed only by test/test-utils builds
pub struct NoopMetrics;

impl OagwMetricsPort for NoopMetrics {
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
}
