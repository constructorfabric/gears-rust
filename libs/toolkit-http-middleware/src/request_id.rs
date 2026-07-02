//! `X-Request-Id` generation and propagation.
//!
//! [`MakeReqId`] generates a fresh id for requests that arrive without one;
//! [`push_req_id_to_extensions`] copies the (incoming or generated) id into
//! request extensions as [`XRequestId`] so handlers can read it, and records it
//! on the current tracing span.

use axum::http::{HeaderName, Request};
use axum::{body::Body, middleware::Next, response::Response};
use tower_http::request_id::{MakeRequestId, RequestId};

/// The request id carried in request extensions for handler access.
#[derive(Clone, Debug)]
pub struct XRequestId(pub String);

/// The `x-request-id` header name.
#[must_use]
pub fn header() -> HeaderName {
    HeaderName::from_static("x-request-id")
}

/// [`MakeRequestId`] implementation that mints a fresh id per request.
#[derive(Clone, Default)]
pub struct MakeReqId;

impl MakeRequestId for MakeReqId {
    fn make_request_id<B>(&mut self, _req: &Request<B>) -> Option<RequestId> {
        // Generate a unique request ID using nanoid
        let id = nanoid::nanoid!();
        Some(RequestId::new(id.parse().ok()?))
    }
}

/// Middleware that stores `request_id` in Request.extensions and records it in the current span
pub async fn push_req_id_to_extensions(mut req: Request<Body>, next: Next) -> Response {
    let hdr = header();
    if let Some(rid) = req
        .headers()
        .get(&hdr)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
    {
        // Save for business logic usage
        req.extensions_mut().insert(XRequestId(rid.clone()));
        // Record into the current http span (created by TraceLayer)
        tracing::Span::current().record("request_id", rid.as_str());
    }

    next.run(req).await
}
