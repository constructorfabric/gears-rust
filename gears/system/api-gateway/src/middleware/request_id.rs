use axum::http::{HeaderName, Request};
use axum::{body::Body, middleware::Next, response::Response};
use tower_http::request_id::{MakeRequestId, RequestId};

#[derive(Clone, Debug)]
pub struct XRequestId(pub String);

#[must_use]
pub fn header() -> HeaderName {
    HeaderName::from_static("x-request-id")
}

#[derive(Clone, Default)]
pub struct MakeReqId;

impl MakeRequestId for MakeReqId {
    fn make_request_id<B>(&mut self, _req: &Request<B>) -> Option<RequestId> {
        // Generate a unique request ID using nanoid
        let id = nanoid::nanoid!();
        Some(RequestId::new(id.parse().ok()?))
    }
}

/// Middleware that stores `request_id` in Request.extensions, records it in the
/// current span, and scopes it as the task-local carrier so outbound
/// `toolkit-http` calls forward it.
pub async fn push_req_id_to_extensions(mut req: Request<Body>, next: Next) -> Response {
    if let Some(rid) = toolkit::api::extract_request_id(req.headers()) {
        // Save for business logic usage
        req.extensions_mut().insert(XRequestId(rid.clone()));
        // Record into the current http span (created by TraceLayer)
        tracing::Span::current().record("request_id", rid.as_str());
        // Install as the task-local carrier so outbound `toolkit-http` calls
        // (generated contract clients, proxied gears) forward the edge id.
        return toolkit::api::with_request_id(Some(rid), next.run(req)).await;
    }

    next.run(req).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::middleware::from_fn;
    use axum::routing::get;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt as _;

    /// Mounted on its own (no `SetRequestIdLayer`), a missing header leaves the
    /// carrier empty. Unreachable in the full router, which always mints an id.
    #[tokio::test]
    async fn leaves_carrier_empty_without_header() {
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_handler = Arc::clone(&captured);

        let app = Router::new()
            .route(
                "/x",
                get(move || {
                    let captured_handler = Arc::clone(&captured_handler);
                    async move {
                        *captured_handler.lock().unwrap() = toolkit::api::current_request_id();
                        StatusCode::OK
                    }
                }),
            )
            .layer(from_fn(push_req_id_to_extensions));

        let resp = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(captured.lock().unwrap().is_none());
    }
}
