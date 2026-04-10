//! Axum route helper that publishes the contract's `OpenAPI` spec at the
//! well-known path `/.well-known/openapi.json`.
//!
//! Industry convention (`OpenAPI` Initiative + AWS / Kong / Tyk patterns):
//! each service exposes its own spec; gateways aggregate. Consumers fetch
//! from this exact path.

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

/// Build an [`axum::Router`] that serves `spec` at
/// `/.well-known/openapi.json`. The spec is pre-serialized to bytes at
/// router-construction time so each request only clones an `Arc` and
/// returns the cached body — no per-request JSON serialization.
///
/// # Example
///
/// ```ignore
/// use toolkit_contract::openapi::{generate_openapi_spec, well_known_openapi_route};
///
/// let spec = generate_openapi_spec(&contract_ir, &binding, &schemas);
/// let app = well_known_openapi_route(spec).merge(my_routes);
/// ```
///
/// # Panics
/// Panics if `spec` cannot be serialized to JSON. The spec is built by the contract
/// macros from `serde_json::Value` shapes that are always serializable, so this is
/// a true invariant violation rather than a runtime failure mode.
#[allow(
    clippy::expect_used,
    reason = "spec is a serde_json::Value built by the contract macros from owned JSON shapes; serde_json::to_vec on a Value can only fail for custom Serialize impls, which Value does not have."
)]
pub fn well_known_openapi_route(spec: &Value) -> Router {
    let bytes = serde_json::to_vec(spec).expect("OpenAPI spec must serialize to JSON");
    let shared: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
    Router::new().route(
        "/.well-known/openapi.json",
        get(move || {
            let body = Arc::clone(&shared);
            async move { serve(&body) }
        }),
    )
}

fn serve(body: &Arc<[u8]>) -> Response {
    let mut response = body.as_ref().to_vec().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    *response.status_mut() = StatusCode::OK;
    response
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use tower::util::ServiceExt as _;

    #[tokio::test]
    async fn serves_spec_at_well_known_path() {
        let spec = json!({
            "openapi": "3.1.0",
            "info": { "title": "X", "version": "v1" },
            "paths": {},
        });
        let app = well_known_openapi_route(&spec);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body, spec);
    }

    #[tokio::test]
    async fn other_paths_404() {
        let app = well_known_openapi_route(&json!({}));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/other")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }
}
