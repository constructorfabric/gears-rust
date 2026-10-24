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
