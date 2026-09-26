//! Structured HTTP access log middleware.
//!
//! Emits one `tracing::info!` event per completed request with the
//! following fields:
//! `msg`, `pid`, `request_id`, `method`, `uri`, `remote_addr`,
//! `remote_addr_ip`, `remote_addr_port`, `content_length`, `user_agent`,
//! `duration_ms`, `duration` (µs), `status`, `bytes_sent`.
//!
//! The event carries no `trace_id` field of its own: the JSON log-correlation
//! formatter splices the live `OTel` span's `trace_id` / `span_id` onto the top
//! level of every record, so a middleware-supplied copy would only duplicate
//! it. The event is emitted inside the active `TraceLayer` span, so the splice
//! resolves — except on the client-disconnect (`Drop`) path, where the span is
//! gone; there the `OTel` context captured at request time is re-attached first
//! (see `CountingBody`).

use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::{body::Body, extract::ConnectInfo, middleware::Next, response::Response};
use bytes::Bytes;
use http_body::Frame;

use super::request_id::XRequestId;

/// Middleware that emits a structured access log line for every HTTP request.
///
/// Must be placed **inside** the `TraceLayer` span (so the event inherits
/// trace context) and **outside** business middleware (auth, rate-limit, etc.)
/// so that the logged status reflects all middleware processing.
///
/// The log is emitted once the response body has been fully streamed (or
/// dropped), so `bytes_sent` reflects actual bytes written — including
/// chunked-transfer and SSE responses that lack a `Content-Length` header.
pub async fn access_log_middleware(req: axum::extract::Request, next: Next) -> Response {
    let start = std::time::Instant::now();

    // --- Request-phase data capture ---

    let method = req.method().to_string();
    let uri = req.uri().path_and_query().map_or_else(
        || req.uri().path().to_owned(),
        std::string::ToString::to_string,
    );

    let content_length: u64 = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    let request_id = req
        .extensions()
        .get::<XRequestId>()
        .map_or_else(String::new, |x| x.0.clone());

    let (remote_addr, remote_addr_ip, remote_addr_port) = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| {
            let addr = ci.0;
            (addr.to_string(), addr.ip().to_string(), addr.port())
        })
        .unwrap_or_default();

    // --- Invoke downstream ---

    let response = next.run(req).await;

    // --- Response-phase data ---

    let status = response.status().as_u16();

    // Wrap the response body in a counting wrapper so that `bytes_sent`
    // reflects the actual number of bytes streamed to the client —
    // including chunked-transfer and SSE responses that have no
    // Content-Length header.
    let log_ctx = AccessLogContext {
        start,
        pid: std::process::id(),
        request_id,
        // Snapshot the active OTel context while still inside the `TraceLayer`
        // span, so `emit` can restore it on the drop path (see the field doc).
        otel_context: opentelemetry::Context::current(),
        method,
        uri,
        remote_addr,
        remote_addr_ip,
        remote_addr_port,
        content_length,
        user_agent,
        status,
    };

    let (parts, body) = response.into_parts();
    let counting_body = CountingBody {
        inner: body,
        bytes_sent: 0,
        log_ctx: Some(log_ctx),
    };
    Response::from_parts(parts, Body::new(counting_body))
}

/// All data needed to emit the access log once the body completes.
struct AccessLogContext {
    start: std::time::Instant,
    pid: u32,
    request_id: String,
    /// `OTel` context captured while the request was still inside the
    /// `TraceLayer` span, re-attached in `emit` so the log-correlation
    /// formatter can splice the live `trace_id` even on the drop path.
    otel_context: opentelemetry::Context,
    method: String,
    uri: String,
    remote_addr: String,
    remote_addr_ip: String,
    remote_addr_port: u16,
    content_length: u64,
    user_agent: String,
    status: u16,
}

impl AccessLogContext {
    fn emit(self, bytes_sent: u64) {
        let elapsed = self.start.elapsed();
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let duration_micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);

        // Restore the captured context so the formatter resolves the span's
        // `trace_id` on the drop path; a no-op restore on the normal path.
        let _guard = self.otel_context.attach();

        tracing::info!(
            target: "access_log",
            msg = "response completed",
            pid = self.pid,
            request_id = %self.request_id,
            method = %self.method,
            uri = %self.uri,
            remote_addr = %self.remote_addr,
            remote_addr_ip = %self.remote_addr_ip,
            remote_addr_port = self.remote_addr_port,
            content_length = self.content_length,
            user_agent = %self.user_agent,
            duration_ms = duration_ms,
            duration = duration_micros,
            status = self.status,
            bytes_sent = bytes_sent,
        );
    }
}

/// A body wrapper that counts bytes as frames are streamed, then emits
/// the access log once the body is fully consumed or dropped.
struct CountingBody {
    inner: Body,
    bytes_sent: u64,
    log_ctx: Option<AccessLogContext>,
}

impl http_body::Body for CountingBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let inner = Pin::new(&mut this.inner);

        match inner.poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                // Body stream finished — emit the access log.
                if let Some(ctx) = this.log_ctx.take() {
                    ctx.emit(this.bytes_sent);
                }
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.bytes_sent = this.bytes_sent.saturating_add(data.len() as u64);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                // Body stream errored — emit the log now rather than
                // deferring to Drop so timing is accurate.
                if let Some(ctx) = this.log_ctx.take() {
                    ctx.emit(this.bytes_sent);
                }
                Poll::Ready(Some(Err(e)))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for CountingBody {
    fn drop(&mut self) {
        // If the body is dropped before the stream completes (e.g. client
        // disconnect), still emit the access log with whatever we counted.
        if let Some(ctx) = self.log_ctx.take() {
            ctx.emit(self.bytes_sent);
        }
    }
}
