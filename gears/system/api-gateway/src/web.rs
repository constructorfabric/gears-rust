use axum::{
    extract::Extension,
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{MethodRouter, get},
};
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use toolkit::runtime::{ReadinessLifecycle, ReadinessReport};
use toolkit::{DependencyChecker, HealthcheckStatus, RestHealthcheckRegistry};

/// Per-check timeout (from `ApiGatewayConfig::healthcheck_timeout_ms`), injected as an
/// `Extension`. A newtype avoids colliding with any other `Duration` extension.
#[derive(Clone, Copy)]
pub struct HealthcheckTimeout(
    /// Maximum time a readiness probe may take before it is reported as failed.
    pub Duration,
);

/// Returns a 501 Not Implemented handler for operations without implementations
#[allow(dead_code)]
pub fn placeholder_handler_501() -> MethodRouter {
    get(|| async move {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "message": "Handler not implemented - will be routed via gRPC in future",
                "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
            })),
        )
    })
}

/// `GET /health`: runs all checks, returns JSON with per-component detail. Public (no auth) in
/// every serving mode; the per-component detail is protected by network placement, not auth,
/// and check messages are sanitized. 200 (Healthy/Degraded), 503 (Unhealthy).
pub async fn health_detail(
    Extension(registry): Extension<Arc<RestHealthcheckRegistry>>,
    Extension(timeout): Extension<HealthcheckTimeout>,
) -> Response {
    let report = registry.report(timeout.0).await;
    let status_code = status_for_report(report.is_ready());
    let body = Json(json!({
        "status": report.status,
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "components": report.components,
    }));
    (status_code, body).into_response()
}

/// The runtime's readiness gates, injected as an `Extension`; `None` outside a
/// runtime (an embedder serving the health router itself).
#[derive(Clone)]
pub struct ReadinessDeps(
    /// The runtime's dependency checker, when the gateway is hosted by a runtime.
    pub Option<Arc<DependencyChecker>>,
);

/// `GET /readyz`: runs all checks, public. 200 (Healthy/Degraded), 503
/// (Unhealthy), with the same [`ReadinessReport`] body as an `OoP` unit's
/// `/readyz`: the lifecycle state and the unresolved readiness gates, no
/// per-check detail (that is `/health`).
pub async fn readyz_check(
    Extension(registry): Extension<Arc<RestHealthcheckRegistry>>,
    Extension(timeout): Extension<HealthcheckTimeout>,
    Extension(ReadinessDeps(deps)): Extension<ReadinessDeps>,
) -> Response {
    let report = registry.report(timeout.0).await;
    let ready = report.is_ready();
    let draining = deps.as_ref().is_some_and(|d| d.is_draining());
    let state = match (draining, ready, report.status) {
        (true, _, _) => ReadinessLifecycle::Draining,
        (false, true, HealthcheckStatus::Degraded) => ReadinessLifecycle::Degraded,
        (false, true, _) => ReadinessLifecycle::Ready,
        (false, false, _) => ReadinessLifecycle::Starting,
    };
    let body = ReadinessReport {
        state,
        ready: ready && !draining,
        unresolved_deps: deps.map(|d| d.unresolved_deps()).unwrap_or_default(),
    };
    (status_for_report(body.ready), Json(body)).into_response()
}

fn status_for_report(report_ready: bool) -> StatusCode {
    if report_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Renders the API docs HTML page for the `OpenAPI` document served under `prefix_path`.
#[cfg(not(feature = "embed_elements"))]
pub fn serve_docs(prefix_path: &str) -> Html<String> {
    let openapi_url = if prefix_path.is_empty() {
        String::from("/openapi.json")
    } else {
        format!("{prefix_path}/openapi.json")
    };
    // External mode: load from CDN @latest
    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>API Docs</title>
  <script src="https://unpkg.com/@stoplight/elements@latest/web-components.min.js"></script>
  <link rel="stylesheet" href="https://unpkg.com/@stoplight/elements@latest/styles.min.css">
</head>
<body>
  <elements-api apiDescriptionUrl="{openapi_url}" router="hash" layout="sidebar"></elements-api>
</body>
</html>"#,
    ))
}

#[cfg(feature = "embed_elements")]
pub fn serve_docs(prefix_path: &str) -> Html<String> {
    let (openapi_url, js_url, css_url) = if prefix_path.is_empty() {
        (
            String::from("/openapi.json"),
            String::from("/docs/assets/web-components.min.js"),
            String::from("/docs/assets/styles.min.css"),
        )
    } else {
        (
            format!("{prefix_path}/openapi.json"),
            format!("{prefix_path}/docs/assets/web-components.min.js"),
            format!("{prefix_path}/docs/assets/styles.min.css"),
        )
    };

    // Embedded mode: reference local embedded assets
    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>API Docs</title>
  <script src="{js_url}"></script>
  <link rel="stylesheet" href="{css_url}">
</head>
<body>
  <elements-api apiDescriptionUrl="{openapi_url}" router="hash" layout="sidebar"></elements-api>
</body>
</html>"#,
    ))
}
