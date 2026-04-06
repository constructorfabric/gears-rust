use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::infra::proxy::headers;
use crate::infra::proxy::websocket::{WebSocketBridgeHandle, websocket_bridge};
use axum::body::Body;
use axum::extract::{Extension, Request};
use axum::response::Response;
use http::StatusCode;
use modkit_security::SecurityContext;
use oagw_sdk::api::ErrorSource;
use tracing::Instrument;

use crate::api::rest::error::error_response;
use crate::module::AppState;

/// Proxy handler for `/oagw/v1/proxy/{alias}/{path:.*}`.
///
/// Parses the alias and path suffix from the URL, validates the request,
/// builds an `http::Request<oagw_sdk::Body>`, and delegates to the Data Plane service.
///
/// For WebSocket upgrade requests (`Upgrade: websocket`), extracts the
/// `hyper::upgrade::OnUpgrade` handle, skips body buffering, and spawns a
/// bidirectional byte-forwarding task after receiving a 101 response.
pub async fn proxy_handler(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    req: Request,
) -> Result<Response, Response> {
    // Short-circuit CORS preflight — return permissive 204 without upstream resolution.
    // The actual request validates the origin against the upstream's CORS config.
    if req.method() == http::Method::OPTIONS
        && req.headers().contains_key(http::header::ORIGIN)
        && req
            .headers()
            .contains_key(http::header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let origin = req
            .headers()
            .get(http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("*");

        let requested_method = req
            .headers()
            .get(http::header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let requested_headers = req
            .headers()
            .get(http::header::ACCESS_CONTROL_REQUEST_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        return Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("access-control-allow-origin", origin)
            .header("access-control-allow-methods", requested_method)
            .header("access-control-allow-headers", requested_headers)
            .header("access-control-allow-credentials", "true")
            .header("access-control-max-age", "86400")
            .header(
                "vary",
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
            )
            .body(Body::empty())
            .unwrap());
    }

    let max_body_size = state.config.max_body_size_bytes;
    let (mut parts, body) = req.into_parts();

    // Detect WebSocket upgrade and extract the hyper upgrade handle.
    // This must happen before body consumption — the OnUpgrade future is
    // stored in extensions by hyper and is needed after returning the 101.
    let is_upgrade = headers::is_websocket_upgrade(&parts.headers);

    // RFC 6455 §4.1: WebSocket upgrade MUST be GET with no body.
    if is_upgrade {
        let path = parts.uri.path().to_string();
        if parts.method != http::Method::GET {
            return Err(error_response(DomainError::Validation {
                detail: "WebSocket upgrade requires GET method".into(),
                instance: path,
            }));
        }
        if parts.headers.contains_key(http::header::CONTENT_LENGTH)
            || parts.headers.contains_key(http::header::TRANSFER_ENCODING)
        {
            return Err(error_response(DomainError::Validation {
                detail: "WebSocket upgrade request must not contain a body".into(),
                instance: path,
            }));
        }
    }

    let on_upgrade = if is_upgrade {
        parts.extensions.remove::<hyper::upgrade::OnUpgrade>()
    } else {
        None
    };

    // Parse alias from the URI to validate it's present.
    let path = parts.uri.path();
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).ok_or_else(|| {
        error_response(DomainError::Validation {
            detail: "invalid proxy path".into(),
            instance: path.to_string(),
        })
    })?;

    // Validate alias is not empty.
    let alias_end = remaining.find('/').unwrap_or(remaining.len());
    if alias_end == 0 {
        return Err(error_response(DomainError::Validation {
            detail: "missing alias in proxy path".into(),
            instance: path.to_string(),
        }));
    }

    // Validate Content-Length if present (skip for WebSocket — no body).
    if !is_upgrade && let Some(cl) = parts.headers.get(http::header::CONTENT_LENGTH) {
        let cl_str = cl.to_str().map_err(|_| {
            error_response(DomainError::Validation {
                detail: "invalid Content-Length header".into(),
                instance: path.to_string(),
            })
        })?;
        let cl_val: usize = cl_str.parse().map_err(|_| {
            error_response(DomainError::Validation {
                detail: format!("Content-Length is not a valid integer: '{cl_str}'"),
                instance: path.to_string(),
            })
        })?;
        if cl_val > max_body_size {
            return Err(error_response(DomainError::PayloadTooLarge {
                detail: format!(
                    "request body of {cl_val} bytes exceeds maximum of {max_body_size} bytes"
                ),
                instance: path.to_string(),
            }));
        }
    }

    // Read body bytes (limited to max_body_size).
    // WebSocket upgrade requests have no body — skip buffering.
    let body_bytes = if is_upgrade {
        Bytes::new()
    } else {
        axum::body::to_bytes(body, max_body_size)
            .await
            .map_err(|_| {
                error_response(DomainError::PayloadTooLarge {
                    detail: format!("request body exceeds maximum of {max_body_size} bytes"),
                    instance: path.to_string(),
                })
            })?
    };

    // Strip the proxy prefix from the URI so the DP receives /{alias}/{path}?query.
    let new_uri_str = if let Some(query) = parts.uri.query() {
        format!("/{remaining}?{query}")
    } else {
        format!("/{remaining}")
    };
    parts.uri = new_uri_str.parse().map_err(|_| {
        error_response(DomainError::Validation {
            detail: "failed to parse proxy URI".into(),
            instance: path.to_string(),
        })
    })?;

    // Build http::Request<Body> for the DP service.
    let sdk_body = oagw_sdk::Body::from(body_bytes);
    let proxy_req = http::Request::from_parts(parts, sdk_body);

    // Execute proxy pipeline.
    let proxy_resp = state
        .dp
        .proxy_request(ctx, proxy_req)
        .await
        .map_err(error_response)?;

    // Convert http::Response<oagw_sdk::Body> to axum Response.
    let (mut resp_parts, sdk_body) = proxy_resp.into_parts();

    // Handle WebSocket 101 Switching Protocols.
    if resp_parts.status == StatusCode::SWITCHING_PROTOCOLS {
        let bridge = resp_parts
            .extensions
            .remove::<WebSocketBridgeHandle>()
            .and_then(|h| h.take())
            .ok_or_else(|| {
                error_response(DomainError::Internal {
                    message: "101 Switching Protocols but WebSocket bridge handle is missing"
                        .into(),
                })
            })?;
        let on_upgrade = on_upgrade.ok_or_else(|| {
            error_response(DomainError::ProtocolError {
                detail: "WebSocket upgrade requested but connection does not support upgrades"
                    .into(),
                instance: String::new(),
            })
        })?;

        // Build the 101 response to return to hyper (triggers the upgrade).
        let mut builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
        for (name, value) in &resp_parts.headers {
            builder = builder.header(name, value);
        }
        // Data now originates from upstream — consistent with the non-upgrade path.
        builder = builder.header("x-oagw-error-source", ErrorSource::Upstream.as_str());
        let response = builder.body(Body::empty()).map_err(|e| {
            error_response(DomainError::Internal {
                message: format!("failed to build WebSocket upgrade response: {e}"),
            })
        })?;

        // Spawn the bidirectional bridge task. It awaits the upgrade
        // (which completes after hyper sends the 101 to the client),
        // then copies raw bytes between the client and the DuplexStream.
        let span = tracing::Span::current();
        tokio::spawn(
            async move {
                match on_upgrade.await {
                    Ok(upgraded) => {
                        websocket_bridge(upgraded, bridge).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "WebSocket upgrade failed");
                    }
                }
            }
            .instrument(span),
        );

        return Ok(response);
    }

    let error_source = resp_parts
        .extensions
        .get::<ErrorSource>()
        .copied()
        .unwrap_or(ErrorSource::Gateway);

    // Build axum response.
    // Response headers are already sanitized by the DP service layer.
    let mut builder = Response::builder().status(resp_parts.status);

    for (name, value) in &resp_parts.headers {
        builder = builder.header(name, value);
    }

    // Add error source header.
    builder = builder.header("x-oagw-error-source", error_source.as_str());

    // Stream the response body.
    let body = Body::from_stream(sdk_body.into_stream());

    builder.body(body).map_err(|e| {
        error_response(DomainError::DownstreamError {
            detail: format!("failed to build response: {e}"),
            instance: String::new(),
        })
    })
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod proxy_tests;
