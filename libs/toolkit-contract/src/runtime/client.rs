//! Per-call helpers used by the generated REST client.
//!
//! The macro keeps emitted code small by funnelling the common
//! "send unary request and decode the response" path through
//! [`send_unary`], and the streaming path through
//! [`send_streaming`].

use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use toolkit_http::RequestBuilder;

use crate::ir::binding::StreamFraming;
use crate::runtime::config::ReconnectConfig;
use crate::runtime::http::{
    body_to_byte_stream, map_http_error, parse_retry_after, read_error_body_prefix,
};
use crate::runtime::multipart::{
    MultipartStream, boundary_from_content_type, parse_multipart_stream,
};
use crate::runtime::sse::{LastEventId, SseStream, StreamActivity, parse_sse_stream_with_id};
use crate::runtime::transport_error::TransportError;

/// Boxed byte stream produced from an HTTP response body for framing parsers.
type BoxByteStream = Pin<
    Box<
        dyn Stream<Item = Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>>
            + Send,
    >,
>;

/// Send a unary request and decode the JSON response.
///
/// `build` is a closure returning a `RequestBuilder` configured with method,
/// URL, headers, and any auth state. It is invoked once per attempt.
///
/// `timeout` bounds the **whole** attempt — connection, response headers, and
/// reading the response body — as a per-attempt deadline (mirroring the
/// streaming path). Elapse maps to [`TransportError::Timeout`], which is
/// transient, so a `#[retryable]` method retries a timed-out attempt. `None`
/// leaves the deadline to the underlying transport.
///
/// # Errors
/// Returns [`TransportError`] when the builder closure fails, the deadline elapses,
/// the network call fails, the response body cannot be read, JSON deserialization of a
/// success body fails, or the server returns a non-success HTTP status (mapped via
/// [`map_http_error`]).
pub async fn send_unary<F, R>(build: F, timeout: Option<Duration>) -> Result<R, TransportError>
where
    F: FnOnce() -> Result<RequestBuilder, TransportError>,
    R: DeserializeOwned,
{
    let builder = build()?;
    let op = async move {
        let response = builder.send().await.map_err(TransportError::network)?;
        let status = response.status();
        if status.is_success() {
            // `HttpResponse::bytes` does NOT enforce status check; we already did.
            // Avoiding `.json()` here because it would re-check status and
            // duplicate work on the success path.
            let bytes = response.bytes().await.map_err(TransportError::network)?;
            // A `204 No Content` (or any 2xx with an empty body) is valid for
            // methods returning `Result<(), _>` and other unit-like `Ok` types.
            // `serde_json::from_slice::<()>(b"")` would fail with an EOF error
            // (and `()` deserializes only from JSON `null`), so map an empty
            // success body to JSON `null` before decoding — `null` deserializes
            // into `()` and `Option::None`, while any non-unit `R` still errors.
            let bytes: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
            serde_json::from_slice::<R>(bytes).map_err(TransportError::serialization)
        } else {
            let retry_after = parse_retry_after(response.headers());
            let bytes = response.bytes().await.map_err(TransportError::network)?;
            let body = String::from_utf8_lossy(&bytes).into_owned();
            Err(map_http_error(status.as_u16(), body, retry_after))
        }
    };
    match timeout {
        Some(d) => tokio::time::timeout(d, op)
            .await
            .map_err(|_| TransportError::Timeout(d))?,
        None => op.await,
    }
}

/// Build the default `toolkit-http` client used by macro-generated REST clients.
///
/// Transport-layer retry is **disabled** — the SDK runs its own retry loop in
/// [`retry_with_backoff`](crate::runtime::retry::retry_with_backoff).
///
/// `require_tls` selects the transport security mode: `false` (the default
/// via [`ClientConfig::new`](crate::runtime::config::ClientConfig::new))
/// allows plaintext `http://`, preserving the platform's existing in-mesh
/// service-to-service convention; `true`
/// ([`ClientConfig::with_require_tls`](crate::runtime::config::ClientConfig::with_require_tls))
/// switches to `toolkit_http::TransportSecurity::TlsOnly`, rejecting
/// plaintext. This matters because the tenant bearer token is forwarded via
/// `Authorization` on whatever scheme the resolved endpoint uses — set
/// `require_tls` when a client may talk to an endpoint outside a trusted
/// network boundary.
///
/// `.with_otel()` is called in BOTH cfg variants below — it always installs
/// `toolkit-http`'s `OtelLayer`, but the layer's actual W3C `traceparent`
/// injection is itself `#[cfg(feature = "otel")]` **inside `toolkit-http`**
/// (a no-op otherwise). Because Cargo unifies features workspace-wide, that
/// gate tracks whether *anything* in the final binary enables
/// `toolkit-http/otel` — which may not be this crate's own `otel` feature.
/// The one thing genuinely gated by *this* crate's `otel` feature (which
/// forwards `toolkit-http/otel`) is `.with_metrics(client_type)` below, since
/// that builder method only exists when the dependency is compiled with the
/// feature on. The generated method's per-method `tracing` span is always
/// opened either way, independent of any of this.
///
/// # Errors
/// Propagates [`toolkit_http::HttpError`] from the underlying builder (e.g. a
/// TLS backend that cannot be constructed under FIPS).
#[cfg(feature = "otel")]
pub fn build_default_http_client(
    client_type: &str,
    require_tls: bool,
) -> Result<toolkit_http::HttpClient, toolkit_http::HttpError> {
    toolkit_http::HttpClient::builder()
        .retry(None)
        .transport(transport_security(require_tls))
        .with_otel()
        .with_metrics(client_type)
        .build()
}

/// Non-`otel` build of [`build_default_http_client`]: RED metrics
/// (`.with_metrics`) are NOT compiled in — that builder method only exists
/// under `toolkit-http/otel`. `client_type` is unused in this configuration.
/// `.with_otel()` is still called (see the doc on the `otel`-cfg sibling
/// above): whether it actually propagates `traceparent` depends on whether
/// `toolkit-http/otel` ends up enabled by *some* crate in the build, not on
/// this crate's own `otel` feature.
///
/// # Errors
/// Propagates [`toolkit_http::HttpError`] from the underlying builder (e.g. a
/// TLS backend that cannot be constructed under FIPS).
#[cfg(not(feature = "otel"))]
pub fn build_default_http_client(
    _client_type: &str,
    require_tls: bool,
) -> Result<toolkit_http::HttpClient, toolkit_http::HttpError> {
    toolkit_http::HttpClient::builder()
        .retry(None)
        .transport(transport_security(require_tls))
        .with_otel()
        .build()
}

fn transport_security(require_tls: bool) -> toolkit_http::TransportSecurity {
    if require_tls {
        toolkit_http::TransportSecurity::TlsOnly
    } else {
        toolkit_http::TransportSecurity::AllowInsecureHttp
    }
}

/// Add a JSON body to a request builder. Wraps `toolkit_http`'s fallible
/// `.json()` (which can fail to serialize) in our [`TransportError`] surface
/// so the macro emit path can `?` uniformly.
///
/// # Errors
/// Returns [`TransportError::Serialization`] when `body` cannot be serialized to JSON.
pub fn with_json_body<T: Serialize>(
    builder: RequestBuilder,
    body: &T,
) -> Result<RequestBuilder, TransportError> {
    builder.json(body).map_err(TransportError::serialization)
}

/// Builder for a streaming request that can be re-issued on reconnect.
///
/// `build` receives the latest seen `Last-Event-ID` (or `None` on the first
/// attempt) and must return a fresh, configured `RequestBuilder`.
/// Implementations should set the `Last-Event-ID` header from the parameter
/// when present.
///
/// The parameter is permanently `None` under any framing other than SSE:
/// `Last-Event-ID` is an SSE mechanism and no other framing has a resume
/// token, so the signature stays framing-free rather than growing a resume-token
/// abstraction with exactly one inhabitant.
pub trait StreamRequestFactory: Send + 'static {
    /// Construct a `RequestBuilder` for the next stream attempt.
    ///
    /// # Errors
    /// Returns [`TransportError`] when the factory cannot produce a builder
    /// (e.g. URL composition or auth header attachment fails).
    fn build(&self, last_event_id: Option<&str>) -> Result<RequestBuilder, TransportError>;
}

impl<F> StreamRequestFactory for F
where
    F: Fn(Option<&str>) -> Result<RequestBuilder, TransportError> + Send + 'static,
{
    fn build(&self, last: Option<&str>) -> Result<RequestBuilder, TransportError> {
        (self)(last)
    }
}

/// A configured streaming request, ready to be opened.
///
/// Replaces what was a positional argument list on [`send_streaming`]. The
/// knobs are independent and all but the factory have a meaningful default, so
/// a builder keeps call sites readable as more of them arrive.
///
/// Defaults: [`StreamFraming::ServerSentEvents`], no reconnect
/// ([`ReconnectConfig::disabled`]), no open deadline and no idle deadline.
///
/// # Stability
/// **Not settled surface.** This exists to be driven by generated code; it has
/// no hand-written consumer yet. Expect it to change until a real consumer has
/// exercised it.
pub struct StreamRequest<F> {
    factory: F,
    framing: StreamFraming,
    reconnect: ReconnectConfig,
    open_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
}

impl<F: StreamRequestFactory> StreamRequest<F> {
    /// Start from a request factory, with SSE framing, reconnect disabled and
    /// no open or idle deadline.
    #[must_use]
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            framing: StreamFraming::default(),
            reconnect: ReconnectConfig::disabled(),
            open_timeout: None,
            idle_timeout: None,
        }
    }

    /// Select the wire framing the response body is parsed as.
    ///
    /// This does **not** set the request's `Accept` header — the factory owns
    /// the request, so advertising the matching media type is its job. The two
    /// come from one declaration in generated code.
    #[must_use]
    pub fn framing(mut self, framing: StreamFraming) -> Self {
        self.framing = framing;
        self
    }

    /// Set the reconnect policy applied to transient open and stream failures.
    #[must_use]
    pub fn reconnect(mut self, reconnect: ReconnectConfig) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Set the deadline for **opening** the stream: the connect, the response
    /// headers, and (on a non-success status) the error-body read. This is a
    /// bound on the open handshake only, distinct from
    /// [`idle_timeout`](Self::idle_timeout), which bounds the gap between items
    /// once the stream is live. Without it, a peer that accepts the socket and
    /// never answers hangs the open forever. Generated code defaults this from
    /// the client's unary [`timeout`](crate::runtime::config::ClientConfig::timeout).
    #[must_use]
    pub fn open_timeout(mut self, open_timeout: Duration) -> Self {
        self.open_timeout = Some(open_timeout);
        self
    }

    /// Set the per-item idle deadline. This is *idle*, not total: any wire
    /// chunk resets it, including ones that dispatch no item.
    #[must_use]
    pub fn idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = Some(idle_timeout);
        self
    }
}

/// Why one open attempt failed, and whether the reconnect budget applies.
///
/// The distinction is not derivable from [`TransportError::is_transient`]: a
/// request-shape failure is a configuration bug that will fail identically on
/// every attempt, so it is fatal regardless of how its error class is
/// otherwise classified.
enum OpenFailure {
    /// Request-shape failure (URL composition, auth attach), or an error-body
    /// read that itself failed. Never retried.
    Fatal(TransportError),
    /// Connect, send, or non-success status. Retried when the error is
    /// transient and budget remains.
    Retryable(TransportError),
}

/// Send one request and await response headers, bounded by `timeout`.
// cancel-safe: holds only the in-flight send; being dropped (the `timeout`
// below wraps `send`, so this is the future cancelled on elapse) abandons the
// attempt with nothing committed and no buffered state to lose.
async fn send_open_request(
    builder: RequestBuilder,
    timeout: Option<Duration>,
) -> Result<toolkit_http::HttpResponse, OpenFailure> {
    let send_fut = builder.send();
    let sent = match timeout {
        Some(d) => match tokio::time::timeout(d, send_fut).await {
            Ok(r) => r,
            Err(_) => return Err(OpenFailure::Retryable(TransportError::Timeout(d))),
        },
        None => send_fut.await,
    };
    // Pre-flight failures (DNS, connect refused) are network-class — eligible
    // for reconnect.
    sent.map_err(|e| OpenFailure::Retryable(TransportError::network(e)))
}

/// Read and classify a non-success open response.
// cancel-safe: reads and discards the error body only to build a message;
// dropping it abandons that read with nothing committed. Awaited to completion
// inside `open`.
async fn open_status_failure(
    response: toolkit_http::HttpResponse,
    timeout: Option<Duration>,
) -> OpenFailure {
    let status_code = response.status().as_u16();
    let retry_after = parse_retry_after(response.headers());
    // Bound the error-body read two ways. In *time*, by the same per-attempt
    // deadline as the initial send — otherwise a slow error body on the
    // pre-stream path could stall the attempt indefinitely, defeating the
    // timeout guarantee. In *size*, by reading only a bounded prefix rather
    // than `response.bytes()` (which buffers up to the client's `max_body_size`,
    // megabytes by default) — the body only feeds a diagnostic, so a large or
    // hostile error body cannot force an outsized allocation here.
    let body_fut = read_error_body_prefix(response.into_body());
    let bytes_result = match timeout {
        Some(d) => match tokio::time::timeout(d, body_fut).await {
            Ok(r) => r,
            Err(_) => return OpenFailure::Retryable(TransportError::Timeout(d)),
        },
        None => body_fut.await,
    };
    let bytes = match bytes_result {
        Ok(b) => b,
        // A failed error-body read ends the stream rather than consuming a
        // reconnect attempt: we no longer know what the peer said, so
        // re-issuing would be guessing.
        Err(e) => return OpenFailure::Fatal(TransportError::network(e)),
    };
    let body = String::from_utf8_lossy(&bytes).into_owned();
    // A transient status (e.g. 503 during a rolling deploy) is
    // reconnect-eligible, consistent with the pre-flight failure branch; other
    // statuses are domain errors and bubble straight through.
    OpenFailure::Retryable(map_http_error(status_code, body, retry_after))
}

/// One open attempt against an already-built request: send it and check the
/// status.
///
/// Steps 1-3 of the streaming lifecycle, shared by [`send_streaming`] and
/// [`open_streaming`]. Performs no retry of its own.
///
/// Takes the built `RequestBuilder` rather than the factory deliberately: an
/// `async fn` holding a `&F` would put that reference in the returned future,
/// which is `Send` only if `F: Sync` — a bound [`StreamRequestFactory`] does
/// not require and should not have to.
async fn attempt_open(
    builder: RequestBuilder,
    timeout: Option<Duration>,
) -> Result<toolkit_http::HttpResponse, OpenFailure> {
    let response = send_open_request(builder, timeout).await?;
    if response.status().is_success() {
        return Ok(response);
    }
    Err(open_status_failure(response, timeout).await)
}

/// A successfully opened streaming response, together with whatever
/// framing-specific setup its body needs before it can be parsed.
///
/// An enum rather than a struct carrying an `Option<String>` boundary, so
/// "multipart with no boundary" is not a representable state. The multipart arm
/// is why this type exists at all: its boundary lives in the response's
/// `Content-Type`, so a multipart open can still fail *after* a `200` — and
/// resolving it here, at open time, is what makes that failure an `Err` from
/// [`open_streaming`] rather than the returned stream's first item.
enum OpenedResponse {
    /// SSE needs no setup beyond the body itself.
    ServerSentEvents(toolkit_http::HttpResponse),
    /// `multipart/mixed`, with the boundary read from the response's
    /// `Content-Type`.
    MultipartMixed {
        response: toolkit_http::HttpResponse,
        boundary: String,
    },
}

/// Perform the framing-specific setup a success response needs.
///
/// # Errors
/// [`TransportError::Framing`] when a `multipart/mixed` response carries no
/// `Content-Type`, or one from which no boundary can be read.
fn open_response(
    framing: StreamFraming,
    response: toolkit_http::HttpResponse,
) -> Result<OpenedResponse, TransportError> {
    match framing {
        StreamFraming::ServerSentEvents => Ok(OpenedResponse::ServerSentEvents(response)),
        StreamFraming::MultipartMixed => {
            let content_type = response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    TransportError::framing(
                        StreamFraming::MultipartMixed,
                        "success response carries no readable `Content-Type` header",
                    )
                })?;
            let boundary = boundary_from_content_type(content_type)?;
            Ok(OpenedResponse::MultipartMixed { response, boundary })
        }
    }
}

/// Owns everything that persists across open attempts: the request factory,
/// the reconnect budget, and the `Last-Event-ID` cell the SSE parser advances.
///
/// Exists so the retry loop can be written once and shared by the eager and
/// lazy entry points. Its methods take `&mut self` rather than `&F` — `&mut T`
/// is `Send` when `T: Send`, so this keeps the futures `Send` without
/// demanding `F: Sync`.
struct StreamOpener<F> {
    factory: F,
    framing: StreamFraming,
    reconnect: ReconnectConfig,
    /// Deadline for the open handshake (connect, headers, error-body read),
    /// applied per attempt. Distinct from `idle_timeout`, which bounds only the
    /// item loop once the stream is live.
    open_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    last_id: LastEventId,
    /// Reconnect attempts consumed so far. Shared by the open and the stream
    /// so a burst of failures with no delivered item is capped as one budget
    /// rather than one per phase. Reset by a healthy connection.
    attempt: u32,
    /// Reopens performed over the whole lifetime of this stream, never reset.
    /// Bounds a peer that keeps resetting the burst budget from reopening
    /// forever (#4740), against `reconnect.max_total_reopens`.
    total_reopens: u32,
}

impl<F: StreamRequestFactory> StreamOpener<F> {
    fn new(request: StreamRequest<F>) -> Self {
        Self {
            factory: request.factory,
            framing: request.framing,
            reconnect: request.reconnect,
            open_timeout: request.open_timeout,
            idle_timeout: request.idle_timeout,
            last_id: LastEventId::empty(),
            attempt: 0,
            total_reopens: 0,
        }
    }

    /// Open the stream, retrying transient failures against the budget.
    // cancel-safe: the only carried state is the budget counters in `self`,
    // which are monotonic, so a drop mid-attempt at worst counts an attempt
    // that wasn't retried (conservative) — never a double-open or a lost item.
    // The driver awaits it to completion rather than racing it under a timeout.
    async fn open(&mut self) -> Result<OpenedResponse, TransportError> {
        loop {
            let snapshot = match self.framing {
                // Re-read per attempt: a reconnect replays the most recently
                // observed `id:` field, which the parser may have advanced
                // since the previous open.
                StreamFraming::ServerSentEvents => self.last_id.current(),
                // `Last-Event-ID` is an SSE mechanism and `multipart/mixed`
                // has no resume token of its own, so the factory is handed
                // `None` on every attempt. Stated as a branch rather than left
                // to the cell simply never being advanced, so the reason is
                // visible at the one place it decides anything: resuming a
                // multipart stream is the caller's own reopen loop, not the
                // transport's.
                StreamFraming::MultipartMixed => None,
            };
            // URL-build / serialization failure on the request side — not
            // eligible for reconnect (config-shape error).
            let built = self.factory.build(snapshot.as_deref());
            let attempted = match built {
                Ok(builder) => attempt_open(builder, self.open_timeout).await,
                Err(e) => Err(OpenFailure::Fatal(e)),
            };
            match attempted {
                // A framing-setup failure (a `200` whose `Content-Type` names
                // no usable boundary) is not retried: it is a response-shape
                // error that repeats identically, the same argument as the
                // request-shape branch above. Returning it from here is also
                // what makes it an `Err` from `open_streaming` rather than the
                // stream's first item.
                Ok(response) => return open_response(self.framing, response),
                Err(OpenFailure::Fatal(e)) => return Err(e),
                Err(OpenFailure::Retryable(err)) => {
                    if !self.consume_attempt(&err).await {
                        return Err(err);
                    }
                }
            }
        }
    }

    /// Consume one reconnect attempt for `err`, backing off before returning
    /// `true`. Returns `false` when the budget is spent or the error class is
    /// not retryable, meaning the caller must surface `err`.
    // cancel-safe: the counter increments precede the backoff `sleep`, so a drop
    // during the sleep leaves the budget consistently decremented (conservative)
    // rather than in a half-updated state. No wire data flows through here.
    async fn consume_attempt(&mut self, err: &TransportError) -> bool {
        // Absolute lifetime ceiling, checked before the burst budget so it also
        // stops a peer that keeps resetting that budget. It survives
        // `reset_budget` (which only clears `attempt`), so it is the one bound a
        // healthy-looking flapping peer cannot escape.
        if self.total_reopens >= self.reconnect.max_total_reopens {
            tracing::warn!(
                total_reopens = self.total_reopens,
                max_total_reopens = self.reconnect.max_total_reopens,
                framing = ?self.framing,
                error = %err,
                "stream lifetime reopen cap reached; surfacing error",
            );
            return false;
        }
        if self.attempt < self.reconnect.max_attempts && err.is_transient() {
            self.attempt += 1;
            self.total_reopens += 1;
            // A server-advised `Retry-After` (from a 429/503 open failure) wins
            // over computed backoff, clamped to `max_delay` so a hostile or
            // misconfigured peer cannot stall the reopen indefinitely — the same
            // preference the unary retry loop applies in `retry::next_delay`.
            let delay = match err.retry_after() {
                Some(advised) => advised.min(self.reconnect.max_delay),
                None => backoff_delay(&self.reconnect, self.attempt),
            };
            // The single choke point for every reconnect: an operator otherwise
            // sees only the final error and nothing when a reopen succeeds.
            tracing::warn!(
                attempt = self.attempt,
                max_attempts = self.reconnect.max_attempts,
                delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                framing = ?self.framing,
                error = %err,
                "stream failed; reconnecting after backoff",
            );
            tokio::time::sleep(delay).await;
            true
        } else {
            // A non-transient error, or the budget is spent: no more reopens.
            if self.attempt >= self.reconnect.max_attempts && err.is_transient() {
                tracing::warn!(
                    attempt = self.attempt,
                    max_attempts = self.reconnect.max_attempts,
                    framing = ?self.framing,
                    error = %err,
                    "stream reconnect budget exhausted; surfacing error",
                );
            }
            false
        }
    }

    /// Reset the budget after a connection that delivered items.
    fn reset_budget(&mut self) {
        self.attempt = 0;
    }
}

/// The framing parser driving one opened response body, behind the single
/// surface [`drive`] needs.
///
/// A `dyn` decoder trait is not available here — decoding is generic in `T`, so
/// the trait could not be object-safe — and the two framings genuinely differ
/// in only three places, so an enum keeps each difference visible at the point
/// it is decided rather than dispersing it behind a vtable.
enum FramedItems<T> {
    Sse(SseStream<T, BoxByteStream>),
    Multipart(MultipartStream<T, BoxByteStream>),
}

impl<T> FramedItems<T>
where
    T: DeserializeOwned + 'static,
{
    /// Build the parser for an opened response.
    ///
    /// `last_id` is consumed **only** by the SSE arm: the cell is
    /// SSE-parser-owned and multipart never reads or writes it.
    fn new(opened: OpenedResponse, last_id: LastEventId) -> Self {
        match opened {
            OpenedResponse::ServerSentEvents(response) => {
                // The parsers need `Unpin + 'static`; pinning on the stack with
                // `pin_mut!` would borrow the byte stream for less than
                // `'static`, so move ownership behind `Box::pin` and hand the
                // boxed stream over.
                let byte_stream: BoxByteStream =
                    Box::pin(body_to_byte_stream(response.into_body()));
                Self::Sse(parse_sse_stream_with_id::<T, _, _>(byte_stream, last_id))
            }
            OpenedResponse::MultipartMixed { response, boundary } => {
                ::std::mem::drop(last_id);
                let byte_stream: BoxByteStream =
                    Box::pin(body_to_byte_stream(response.into_body()));
                Self::Multipart(parse_multipart_stream::<T, _, _>(byte_stream, &boundary))
            }
        }
    }

    /// Wire-activity counter, so the idle deadline stays *idle* rather than
    /// merely *quiet* under either framing.
    fn activity_handle(&self) -> StreamActivity {
        match self {
            Self::Sse(s) => s.activity_handle(),
            Self::Multipart(s) => s.activity_handle(),
        }
    }

    // cancel-safe: the parser state (buffered wire bytes, partial frame) lives
    // in `self`, not this future — `drive` drops and re-creates this future
    // around its idle `timeout` on every quiet window, and resumes without
    // losing bytes. Both arms delegate to a `Stream::next` that only reads into
    // the stream's own buffer. This is the load-bearing one: if it regressed to
    // holding partial state in the future, every idle elapse would drop bytes.
    async fn next(&mut self) -> Option<Result<T, TransportError>> {
        use futures_util::StreamExt;
        match self {
            Self::Sse(s) => s.next().await,
            Self::Multipart(s) => s.next().await,
        }
    }

    /// What a **graceful** end of the underlying byte stream means for this
    /// framing. `None` is a clean end; `Some` ends the stream with that error,
    /// which is transient and so reconnect-eligible.
    ///
    /// This is the one place the two framings disagree about success, and
    /// getting it backwards is silent in both directions — every completed
    /// multipart stream would become an error, or every truncated SSE stream a
    /// success.
    fn end_of_stream_error(&self) -> Option<TransportError> {
        match self {
            // SSE has no application-level terminator, so the transport must
            // supply one: an `event: done` frame and a bare connection close
            // both surface as an end of stream. Treat a close WITHOUT an
            // explicit `done` as an anomaly rather than silently reporting
            // success — otherwise a server restart / LB idle-timeout / rolling
            // deploy that closes the connection mid-stream would look identical
            // to a fully-delivered stream.
            Self::Sse(s) => (!s.saw_done_event()).then(|| {
                TransportError::framing(
                    StreamFraming::ServerSentEvents,
                    "SSE stream ended without a terminal `done` event",
                )
            }),
            // A complete multipart body ends with the `--<boundary>--` close
            // delimiter (RFC 2046). A graceful EOF that did NOT see it is a
            // truncation — a proxy idle-timeout, an LB half-close, a rolling
            // deploy closing the connection mid-body — so report it, mirroring
            // the SSE arm (#4740). On the public path the caller has no other
            // way to tell truncation from completion: `send_streaming` /
            // `open_streaming` hand back a boxed stream that erases
            // `MultipartStream::saw_close_delimiter`, and a generic item type
            // carries no terminal marker of its own. An *aborted* body is a
            // different thing and still arrives as a `Network` error item, not
            // as an end of stream.
            Self::Multipart(s) => (!s.saw_close_delimiter()).then(|| {
                TransportError::framing(
                    StreamFraming::MultipartMixed,
                    "multipart body ended without a closing `--<boundary>--` delimiter",
                )
            }),
        }
    }
}

/// Parse one already-opened response body and yield its items.
///
/// Steps 4-5 of the streaming lifecycle. Ends after yielding at most one
/// `Err`; a clean end yields nothing further. Reconnect is the caller's
/// concern.
fn drive<T>(
    opened: OpenedResponse,
    last_id: LastEventId,
    idle_timeout: Option<Duration>,
) -> impl Stream<Item = Result<T, TransportError>> + Send
where
    T: DeserializeOwned + Send + 'static,
{
    async_stream::stream! {
        let mut inner = FramedItems::<T>::new(opened, last_id);
        let activity = inner.activity_handle();
        loop {
            // Idle timeout is *idle*: keepalive comments (and any other wire
            // chunk that dispatches no item) still count as activity, so a
            // quiet-but-alive stream is not torn down. On elapse we only
            // error if no chunk arrived while we waited.
            let item = match idle_timeout {
                Some(d) => loop {
                    let before = activity.generation();
                    match tokio::time::timeout(d, inner.next()).await {
                        Ok(v) => break v,
                        // Activity advanced since the wait started (e.g. a
                        // keepalive comment) — loop again rather than
                        // treating this as a genuine idle timeout. Falling
                        // off this arm already re-enters the loop; an
                        // explicit `continue` here is redundant.
                        Err(_) if activity.generation() != before => {}
                        Err(_) => {
                            yield Err(TransportError::Timeout(d));
                            return;
                        }
                    }
                },
                None => inner.next().await,
            };
            match item {
                Some(Ok(v)) => yield Ok(v),
                Some(Err(e)) => {
                    yield Err(e);
                    return;
                }
                None => {
                    // The underlying byte stream ended with no error. Whether
                    // that is success depends on the framing — see
                    // `FramedItems::end_of_stream_error`.
                    if let Some(e) = inner.end_of_stream_error() {
                        yield Err(e);
                    }
                    return;
                }
            }
        }
    }
}

/// Drive an opened response to completion, re-opening on transient failures.
///
/// `first` is the already-opened response; subsequent attempts re-open through
/// [`StreamOpener::open`].
fn drive_with_reconnect<F, T>(
    mut opener: StreamOpener<F>,
    first: OpenedResponse,
) -> impl Stream<Item = Result<T, TransportError>> + Send
where
    F: StreamRequestFactory,
    T: DeserializeOwned + Send + 'static,
{
    use futures_util::StreamExt;

    async_stream::try_stream! {
        let mut pending = Some(first);
        loop {
            let response = match pending.take() {
                Some(r) => r,
                // Only reached on a reopen (`pending` is `Some` on the first
                // iteration), so a success here is a recovered reconnect.
                None => match opener.open().await {
                    Ok(r) => {
                        tracing::debug!(
                            attempt = opener.attempt,
                            framing = ?opener.framing,
                            "stream reconnect succeeded",
                        );
                        r
                    }
                    Err(e) => {
                        Err(e)?;
                        return;
                    }
                },
            };

            // Measured from the moment the connection is live (its response is
            // open) to when its byte stream ends, so a one-item-then-drop peer
            // registers a near-zero uptime.
            let connection_started = tokio::time::Instant::now();
            let mut inner = Box::pin(
                drive::<T>(response, opener.last_id.clone(), opener.idle_timeout),
            );
            let mut stream_err: Option<TransportError> = None;
            // Whether this connection ever delivered an event. Necessary but not
            // sufficient for "healthy" — see the reset gate below.
            let mut delivered_an_event = false;
            while let Some(item) = inner.next().await {
                match item {
                    Ok(v) => {
                        delivered_an_event = true;
                        yield v;
                    }
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }
            let uptime = connection_started.elapsed();
            // Drop the parser before re-opening: it owns the previous
            // connection's body, and the next attempt gets a fresh one.
            ::std::mem::drop(inner);

            // A *healthy* connection resets the burst budget so `max_attempts`
            // is a burst cap, not a lifetime cap: a long-lived subscription that
            // survives N unrelated blips over days should not die on the N+1st.
            // "Healthy" needs both a delivered item AND a minimum uptime —
            // delivering one item then dropping immediately is the #4740 peer
            // that would otherwise reset the budget forever and reopen at
            // `base_delay` indefinitely, re-sending the auth token each time. A
            // too-brief connection instead counts against the budget, and the
            // absolute `max_total_reopens` cap (in `consume_attempt`) backstops
            // a peer that games the uptime threshold.
            if delivered_an_event && uptime >= opener.reconnect.min_healthy_uptime {
                opener.reset_budget();
            }

            match stream_err {
                // Stream ended cleanly, by whatever its framing calls clean.
                None => return,
                Some(e) => {
                    if !opener.consume_attempt(&e).await {
                        Err(e)?;
                        return;
                    }
                    // Budget consumed and backoff applied — fall through to
                    // the next iteration, which re-opens.
                }
            }
        }
    }
}

/// Open a stream **eagerly**: connect, check the response status, and perform
/// the framing's own setup before returning, so an open-time failure is an
/// `Err` from this call rather than the returned stream's first item.
///
/// This is the entry point for a contract method declared
/// `#[streaming] async fn` — a stream whose open is a distinct, fallible
/// operation. Open-time failures (a `404`, a `409` carrying a domain
/// `Problem`, a `410`) reach the caller before any item exists, where its
/// open-time error handling can act on them.
///
/// The request's reconnect policy governs the *open* as well as the stream:
/// with [`ReconnectConfig::disabled`] (the [`StreamRequest::new`] default, and
/// what generated code passes for a fallible open per D6) exactly one open
/// attempt is made.
///
/// # Errors
/// Returns [`TransportError`] when the request cannot be built, the connect
/// fails or times out, the response status is not a success, or the framing's
/// setup fails — for [`StreamFraming::MultipartMixed`] that last case is a
/// `200` whose `Content-Type` names no usable boundary, which is why a
/// multipart open can fail after a success status. Failures after a successful
/// open arrive as items of the returned stream.
///
/// # Stability
/// **Not settled surface** — see [`StreamRequest`].
pub async fn open_streaming<F, T>(
    request: StreamRequest<F>,
) -> Result<Pin<Box<dyn Stream<Item = Result<T, TransportError>> + Send>>, TransportError>
where
    F: StreamRequestFactory,
    T: DeserializeOwned + Send + 'static,
{
    let mut opener = StreamOpener::new(request);
    let response = opener.open().await?;
    Ok(Box::pin(drive_with_reconnect(opener, response)))
}

/// Send a streaming request and adapt the response into a typed stream, with
/// the open deferred until the stream is first polled.
///
/// The returned stream yields `Result<T, TransportError>` items, one per SSE
/// event or per `multipart/mixed` part. With `reconnect.max_attempts == 0` (the
/// default), a transient transport failure ends the stream immediately. With a
/// non-zero limit, the client re-issues the request up to `max_attempts` times,
/// applying exponential backoff between attempts — replaying `Last-Event-ID`
/// under SSE framing, and with no resume token under any other. A
/// `multipart/mixed` reopen therefore replays the body from its first part,
/// redelivering any items already yielded (at-least-once); see
/// [`ClientConfig::stream_reconnect`](crate::runtime::config::ClientConfig::stream_reconnect).
///
/// The open is **lazy**: nothing is sent until the returned stream is first
/// polled, so a connect failure, a non-success status or a framing-setup
/// failure arrives as the stream's first item. That is the shape a
/// `#[streaming] fn` method needs, since it has nowhere else to put an error.
/// Use [`open_streaming`] when the open must be able to fail on its own.
///
/// # Stability
/// The [`StreamRequest`] parameter is **not settled surface** — see
/// [`StreamRequest`]. This entry point took a positional argument list before
/// a framing selector existed.
pub fn send_streaming<F, T>(
    request: StreamRequest<F>,
) -> Pin<Box<dyn Stream<Item = Result<T, TransportError>> + Send>>
where
    F: StreamRequestFactory,
    T: DeserializeOwned + Send + 'static,
{
    use futures_util::StreamExt;

    Box::pin(async_stream::try_stream! {
        // Deferred into the stream so this entry point stays lazy.
        let mut inner = match open_streaming::<F, T>(request).await {
            Ok(s) => s,
            Err(e) => {
                Err(e)?;
                return;
            }
        };
        while let Some(item) = inner.next().await {
            yield item?;
        }
    })
}

/// Compute the (jittered) backoff delay for reconnect attempt #N (1-indexed).
/// Doubles the base delay each attempt, capped at `max_delay`, then multiplies
/// by a ±25% jitter factor — fleet-wide reconnect synchronization (many clients
/// reconnecting in lockstep after a shared upstream blip) is the real concern.
///
/// Pure so the caller can both log the chosen delay and sleep the same value —
/// the jitter is random, so it must be drawn exactly once.
fn backoff_delay(config: &ReconnectConfig, attempt: u32) -> Duration {
    use rand::RngExt;
    let exp = attempt.saturating_sub(1);
    let multiplier = 2u32.saturating_pow(exp);
    let base = config
        .base_delay
        .saturating_mul(multiplier)
        .min(config.max_delay);
    let jitter: f64 = rand::rng().random_range(0.75..=1.25);
    let secs = base.as_secs_f64() * jitter;
    if secs.is_finite() && secs >= 0.0 {
        Duration::from_secs_f64(secs).min(config.max_delay)
    } else {
        base
    }
}

// `with_json_body` and the streaming path are exercised end-to-end (real
// serialized body observed by a real server) by the integration tests in
// `tests/rest_client_codegen.rs` (`unary_post_round_trip` et al.) — a local
// unit test here could only check "builds without panicking" without
// access to `toolkit_http::RequestBuilder`'s private fields, which is
// strictly weaker than the existing round-trip coverage.
