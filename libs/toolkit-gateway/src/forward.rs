//! Reverse-proxy request forwarder over a shared [`ProxyRegistry`].
//!
//! [`Forwarder::forward`] matches the inbound request path against the registry,
//! forwards the request to the owning gear's endpoint via a
//! [`toolkit_http::HttpClient`], and returns the upstream response. The
//! `Authorization: Bearer <jwt>` header is propagated as-is for per-hop
//! re-validation (ADR-0008); hop-by-hop headers are dropped.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderName, Method, StatusCode};
use toolkit_canonical_errors::Problem;
use toolkit_http::{HttpClient, HttpError, HttpResponse, RequestBuilder};

use crate::registry::{ProxyRegistry, RouteMatch};

/// Maximum request body buffered before forwarding (10 MiB).
const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Reverse-proxy forwarder backed by a shared [`ProxyRegistry`].
///
/// Cheap to clone: it holds an `Arc` to the registry and a cloneable
/// [`HttpClient`].
#[derive(Clone)]
pub struct Forwarder {
    registry: Arc<ProxyRegistry>,
    client: HttpClient,
}

impl Forwarder {
    /// Builds a forwarder over `registry`, using `client` for upstream calls.
    #[must_use]
    pub fn new(registry: Arc<ProxyRegistry>, client: HttpClient) -> Self {
        Self { registry, client }
    }

    /// Forwards `req` to the registered upstream for its path.
    ///
    /// Returns the upstream response verbatim (minus hop-by-hop headers), or an
    /// RFC 9457 `problem+json` response: `404` if no route matches, `413` if the
    /// body exceeds the proxy limit, `400` if the request body cannot be read
    /// (e.g. a client abort mid-stream), `501` for an unsupported method, and
    /// `502`/`503`/`504` for upstream failures.
    pub async fn forward(&self, req: Request) -> Response {
        let path = req.uri().path().to_owned();
        let Some(target) = self.registry.match_path(&path) else {
            return problem(
                StatusCode::NOT_FOUND,
                format!("no upstream route registered for '{path}'"),
            );
        };

        let method = req.method().clone();
        let url = build_target_url(&target, req.uri());
        let headers = req.headers().clone();

        let body = match axum::body::to_bytes(req.into_body(), MAX_PROXY_BODY_BYTES).await {
            Ok(body) => body,
            Err(err) => return body_read_problem(&err),
        };

        let Some(builder) = self.start_request(&method, &url) else {
            return problem(
                StatusCode::NOT_IMPLEMENTED,
                format!("method '{method}' is not supported by the gateway proxy"),
            );
        };
        let builder = apply_request_headers(builder, &headers).body_bytes(body);

        match builder.send().await {
            Ok(resp) => upstream_to_response(resp),
            Err(err) => {
                tracing::warn!(error = %err, upstream = %url, "proxy upstream request failed");
                problem(upstream_status(&err), "upstream request failed".to_owned())
            }
        }
    }

    /// Starts a request builder for `method`, or `None` if the method is not
    /// supported by the underlying client (e.g. `TRACE`/`CONNECT`).
    fn start_request(&self, method: &Method, url: &str) -> Option<RequestBuilder> {
        let builder = match method.as_str() {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "PATCH" => self.client.patch(url),
            "DELETE" => self.client.delete(url),
            "HEAD" => self.client.head(url),
            "OPTIONS" => self.client.options(url),
            _ => return None,
        };
        Some(builder)
    }
}

/// Axum fallback handler that forwards any unmatched request via a [`Forwarder`]
/// supplied as router state.
pub async fn proxy_fallback(State(forwarder): State<Forwarder>, req: Request) -> Response {
    forwarder.forward(req).await
}

/// Build the upstream URL: `scheme://authority` + inbound path (+ query).
fn build_target_url(target: &RouteMatch, uri: &http::Uri) -> String {
    let mut url = format!(
        "{}://{}{}",
        target.endpoint.scheme(),
        target.endpoint.authority(),
        uri.path()
    );
    if let Some(query) = uri.query() {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// Copy inbound headers onto the outbound request, dropping hop-by-hop headers.
/// `Authorization` is preserved for per-hop re-validation.
fn apply_request_headers(mut builder: RequestBuilder, headers: &HeaderMap) -> RequestBuilder {
    for (name, value) in headers {
        if strip_from_request(name) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            builder = builder.header(name.as_str(), value);
        }
    }
    builder
}

/// Convert an upstream [`HttpResponse`] into an axum [`Response`], copying status
/// and headers (minus hop-by-hop / decompression headers).
///
/// The upstream body is streamed through [`HttpResponse::into_limited_body`]
/// rather than fully buffered, bounding memory use while still enforcing the
/// client's configured size limit. Because status and headers are emitted before
/// the body is read, a mid-stream upstream read error terminates the response
/// body rather than producing a `502` problem response.
fn upstream_to_response(resp: HttpResponse) -> Response {
    let status = resp.status();
    let headers = resp.headers().clone();

    let mut response = Response::new(Body::new(resp.into_limited_body()));
    *response.status_mut() = status;
    let out = response.headers_mut();
    for (name, value) in &headers {
        if strip_from_response(name) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    response
}

/// Map an upstream transport error to the client-facing gateway status.
fn upstream_status(err: &HttpError) -> StatusCode {
    match err {
        HttpError::Timeout(_) | HttpError::DeadlineExceeded(_) => StatusCode::GATEWAY_TIMEOUT,
        HttpError::Overloaded | HttpError::ServiceClosed => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Hop-by-hop request headers (RFC 9110 §7.6.1) plus `host`/`content-length`,
/// which the proxy client sets itself.
fn strip_from_request(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

/// Response headers to drop: hop-by-hop plus `content-encoding` (the client has
/// already transparently decompressed the body).
fn strip_from_response(name: &HeaderName) -> bool {
    strip_from_request(name) || name.as_str() == "content-encoding"
}

/// Map a request-body buffering failure to a problem response.
///
/// Returns `413 Payload Too Large` only when the body exceeded
/// [`MAX_PROXY_BODY_BYTES`] (an [`http_body_util::LengthLimitError`]); any other
/// failure is a client stream read error (e.g. an aborted upload or reset
/// connection) and maps to `400 Bad Request`.
fn body_read_problem(err: &axum::Error) -> Response {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(cause) = source {
        if cause.is::<http_body_util::LengthLimitError>() {
            return problem(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeded the proxy limit".to_owned(),
            );
        }
        source = cause.source();
    }

    tracing::warn!(error = %err, "reading proxied request body failed");
    problem(
        StatusCode::BAD_REQUEST,
        "failed to read request body".to_owned(),
    )
}

/// Build an RFC 9457 `problem+json` response with the given status and detail.
fn problem(status: StatusCode, detail: String) -> Response {
    Problem {
        problem_type: "about:blank".to_owned(),
        title: status.canonical_reason().unwrap_or("Error").to_owned(),
        status: status.as_u16(),
        detail,
        instance: None,
        trace_id: None,
        context: serde_json::Value::Object(serde_json::Map::new()),
    }
    .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::extract::Request;
    use http::StatusCode;
    use toolkit_http::HttpClient;

    use super::{Forwarder, body_read_problem};
    use crate::registry::ProxyRegistry;

    #[tokio::test]
    async fn oversize_body_maps_to_413() {
        // A body larger than the limit yields a `LengthLimitError`, which must
        // map to 413 Payload Too Large.
        let err = axum::body::to_bytes(Body::from(vec![0u8; 128]), 16)
            .await
            .expect_err("body exceeds limit");
        assert_eq!(
            body_read_problem(&err).status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn client_read_error_maps_to_400() {
        // A stream that errors mid-read (client abort / reset) is not a size-limit
        // failure and must map to 400 Bad Request, not 413.
        let stream = futures_util::stream::once(async {
            Err::<bytes::Bytes, std::io::Error>(std::io::Error::other("client aborted"))
        });
        let err = axum::body::to_bytes(Body::from_stream(stream), 1024)
            .await
            .expect_err("stream read fails");
        assert_eq!(body_read_problem(&err).status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unmatched_path_returns_404_problem() {
        let registry = Arc::new(ProxyRegistry::new());
        let client = HttpClient::new().expect("client builds");
        let forwarder = Forwarder::new(registry, client);

        let req = Request::builder()
            .uri("/nope/v1/thing")
            .body(Body::empty())
            .expect("request builds");
        let response = forwarder.forward(req).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        assert_eq!(content_type, Some("application/problem+json"));
    }
}
