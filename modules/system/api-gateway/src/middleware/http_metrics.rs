//! HTTP server metrics middleware (OpenTelemetry Semantic Conventions).
//!
//! Records two instruments per request:
//! - `http.server.request.duration` — histogram (seconds)
//! - `http.server.active_requests` — up-down counter
//!
//! Attributes follow [OpenTelemetry HTTP semantic conventions][otel]:
//! `http.request.method`, `http.route`, `http.response.status_code`.
//!
//! [otel]: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/

use std::sync::Arc;

use axum::{
    extract::{MatchedPath, State},
    middleware::Next,
    response::Response,
};
use opentelemetry::{
    KeyValue,
    metrics::{Histogram, UpDownCounter},
};

/// Holds the two OpenTelemetry instruments for HTTP server metrics.
pub struct HttpMetrics {
    duration: Histogram<f64>,
    active_requests: UpDownCounter<i64>,
}

impl HttpMetrics {
    /// Create instruments on the global meter scoped to the given module name.
    ///
    /// When `prefix` is non-empty the metric names become
    /// `{prefix}.http.server.request.duration` and
    /// `{prefix}.http.server.active_requests`.
    #[must_use]
    pub fn new(module_name: &str, prefix: &str) -> Self {
        let prefix = prefix.trim().trim_end_matches('.'); // Normalize prefix.

        let scope = opentelemetry::InstrumentationScope::builder(module_name.to_owned()).build();
        let meter = opentelemetry::global::meter_with_scope(scope);

        let (duration_name, active_name) = if prefix.is_empty() {
            (
                "http.server.request.duration".to_owned(),
                "http.server.active_requests".to_owned(),
            )
        } else {
            (
                format!("{prefix}.http.server.request.duration"),
                format!("{prefix}.http.server.active_requests"),
            )
        };

        let duration = meter
            .f64_histogram(duration_name)
            .with_description("Duration of HTTP server requests")
            .with_unit("s")
            .build();

        let active_requests = meter
            .i64_up_down_counter(active_name)
            .with_description("Number of active HTTP server requests")
            .build();

        Self {
            duration,
            active_requests,
        }
    }
}

/// Drop guard that decrements the active-requests counter, ensuring
/// the counter is decremented even if downstream handlers panic.
struct ActiveRequestGuard {
    counter: UpDownCounter<i64>,
    attrs: [KeyValue; 1],
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.counter.add(-1, &self.attrs);
    }
}

/// Tiny `route_layer` that copies [`MatchedPath`] into **response** extensions
/// so that outer `layer()` middleware (e.g. metrics) can read the route template.
pub async fn propagate_matched_path(
    matched_path: Option<MatchedPath>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let mut response = next.run(req).await;
    if let Some(path) = matched_path {
        response.extensions_mut().insert(path);
    }
    response
}

/// Normalize HTTP method per [OTel semantic conventions][semconv].
///
/// Unknown methods are mapped to `_OTHER` to bound attribute cardinality
/// and prevent metric explosion from arbitrary method strings.
///
/// [semconv]: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/
fn normalize_method(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        axum::http::Method::PUT => "PUT",
        axum::http::Method::DELETE => "DELETE",
        axum::http::Method::PATCH => "PATCH",
        axum::http::Method::HEAD => "HEAD",
        axum::http::Method::OPTIONS => "OPTIONS",
        axum::http::Method::CONNECT => "CONNECT",
        axum::http::Method::TRACE => "TRACE",
        _ => "_OTHER",
    }
}

/// Axum middleware that records HTTP server metrics.
///
/// Use with `axum::middleware::from_fn_with_state` and add as a **`layer`**
/// (not `route_layer`) so it captures responses from all middleware layers.
/// The `http.route` attribute is read from response extensions, populated
/// by [`propagate_matched_path`] which must be added as an inner `route_layer`.
pub async fn http_metrics_middleware(
    State(metrics): State<Arc<HttpMetrics>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method_kv = KeyValue::new("http.request.method", normalize_method(req.method()));

    metrics
        .active_requests
        .add(1, std::slice::from_ref(&method_kv));
    let _guard = ActiveRequestGuard {
        counter: metrics.active_requests.clone(),
        attrs: [method_kv.clone()],
    };

    let start = std::time::Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed().as_secs_f64();

    let route = response
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let route_kv = KeyValue::new("http.route", route);
    let status = i64::from(response.status().as_u16());

    metrics.duration.record(
        elapsed,
        &[
            method_kv,
            route_kv,
            KeyValue::new("http.response.status_code", status),
        ],
    );

    response
}
