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

use crate::runtime::config::ReconnectConfig;
use crate::runtime::http::{body_to_byte_stream, map_http_error, parse_retry_after};
use crate::runtime::sse::{LastEventId, parse_sse_stream_with_id};
use crate::runtime::transport_error::TransportError;

/// Boxed byte stream produced from an HTTP response body for SSE parsing.
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

/// Builder for an SSE request that can be re-issued on reconnect.
///
/// `build` receives the latest seen `Last-Event-ID` (or `None` on the first
/// attempt) and must return a fresh, configured `RequestBuilder`.
/// Implementations should set the `Last-Event-ID` header from the parameter
/// when present.
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

/// Send a streaming SSE request and adapt the response into a typed stream.
///
/// `factory` produces a fresh `RequestBuilder` per attempt; the
/// `last_event_id` argument is `None` on the first attempt and contains the
/// most recently observed SSE `id:` field on every reconnect.
///
/// The returned stream yields `Result<T, TransportError>` items per SSE
/// event. With `reconnect.max_attempts == 0` (the default), a transient
/// transport failure ends the stream immediately. With a non-zero limit,
/// the client re-issues the request with a `Last-Event-ID` header up to
/// `max_attempts` times, applying exponential backoff between attempts.
pub fn send_streaming<F, T>(
    factory: F,
    reconnect: ReconnectConfig,
    timeout: Option<Duration>,
) -> Pin<Box<dyn Stream<Item = Result<T, TransportError>> + Send>>
where
    F: StreamRequestFactory,
    T: DeserializeOwned + Send + 'static,
{
    use futures_util::StreamExt;

    Box::pin(async_stream::try_stream! {
        let last_id = LastEventId::empty();
        let mut attempt = 0u32;

        loop {
            let snapshot = last_id.current();
            let builder = match factory.build(snapshot.as_deref()) {
                Ok(b) => b,
                Err(e) => {
                    // URL-build / serialization failure on the request side —
                    // not eligible for reconnect (config-shape error).
                    Err(e)?;
                    return;
                }
            };
            let send_fut = builder.send();
            let response = match timeout {
                Some(d) => if let Ok(r) = tokio::time::timeout(d, send_fut).await { r } else {
                    let err = TransportError::Timeout(d);
                    if attempt < reconnect.max_attempts && err.is_transient() {
                        attempt += 1;
                        sleep_backoff(&reconnect, attempt).await;
                        continue;
                    }
                    Err(err)?;
                    return;
                },
                None => send_fut.await,
            };
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    // Pre-flight failures (DNS, connect refused) are
                    // network-class — eligible for reconnect.
                    let err = TransportError::network(e);
                    if attempt < reconnect.max_attempts && err.is_transient() {
                        attempt += 1;
                        sleep_backoff(&reconnect, attempt).await;
                        continue;
                    }
                    Err(err)?;
                    return;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let status_code = status.as_u16();
                let retry_after = parse_retry_after(response.headers());
                // Bound the error-body read by the same per-attempt deadline as
                // the initial send — otherwise a slow/oversized error body on
                // the pre-stream path could stall the attempt indefinitely,
                // defeating the timeout guarantee.
                let body_fut = response.bytes();
                let bytes_result = match timeout {
                    Some(d) => if let Ok(r) = tokio::time::timeout(d, body_fut).await { r } else {
                        let err = TransportError::Timeout(d);
                        if attempt < reconnect.max_attempts && err.is_transient() {
                            attempt += 1;
                            sleep_backoff(&reconnect, attempt).await;
                            continue;
                        }
                        Err(err)?;
                        return;
                    },
                    None => body_fut.await,
                };
                let bytes = bytes_result.map_err(TransportError::network)?;
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let err = map_http_error(status_code, body, retry_after);
                // A transient status (e.g. 503 during a rolling deploy) on the
                // initial connect is reconnect-eligible, consistent with the
                // pre-flight failure branch above; other statuses are domain
                // errors and bubble straight through.
                if attempt < reconnect.max_attempts && err.is_transient() {
                    attempt += 1;
                    sleep_backoff(&reconnect, attempt).await;
                    continue;
                }
                Err(err)?;
                return;
            }

            // `parse_sse_stream_with_id` needs `Unpin + 'static`; pinning
            // on the stack with `pin_mut!` would borrow `byte_stream` for
            // less than `'static`, so move ownership behind `Box::pin` and
            // hand the boxed stream to the parser.
            let byte_stream: BoxByteStream = Box::pin(body_to_byte_stream(response.into_body()));
            let mut inner = parse_sse_stream_with_id::<T, _, _>(
                byte_stream,
                last_id.clone(),
            );
            let activity = inner.activity_handle();
            let mut stream_err: Option<TransportError> = None;
            loop {
                // Idle timeout is *idle*: keepalive comments (and any other wire
                // chunk that dispatches no item) still count as activity, so a
                // quiet-but-alive stream is not torn down. On elapse we only
                // error if no chunk arrived while we waited.
                let item = match timeout {
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
                                stream_err = Some(TransportError::Timeout(d));
                                break None;
                            }
                        }
                    },
                    None => inner.next().await,
                };
                if stream_err.is_some() {
                    break;
                }
                match item {
                    Some(Ok(v)) => yield v,
                    Some(Err(e)) => {
                        stream_err = Some(e);
                        break;
                    }
                    None => {
                        // The underlying byte stream ended with no error, but
                        // that alone doesn't mean the peer is finished: an
                        // `event: done` frame and a bare connection close both
                        // surface as `None` here. Treat a close WITHOUT an
                        // explicit `done` as an anomaly (reconnect-eligible)
                        // rather than silently reporting success — otherwise a
                        // server restart / LB idle-timeout / rolling deploy
                        // that closes the connection mid-stream would look
                        // identical to a fully-delivered stream.
                        if !inner.saw_done_event() {
                            stream_err = Some(TransportError::sse(
                                "SSE stream ended without a terminal `done` event",
                            ));
                        }
                        break;
                    }
                }
            }

            match stream_err {
                None => return, // Stream ended cleanly (`event: done`).
                Some(e) if attempt < reconnect.max_attempts && e.is_transient() => {
                    attempt += 1;
                    sleep_backoff(&reconnect, attempt).await;
                    // Fall through to next loop iteration to retry.
                }
                Some(e) => {
                    Err(e)?;
                    return;
                }
            }
        }
    })
}

/// Compute backoff for reconnect attempt #N (1-indexed). Doubles the base
/// delay each attempt, capped at `max_delay`. Multiplied by a ±25% jitter
/// factor — fleet-wide reconnect synchronization (many clients reconnecting
/// in lockstep after a shared upstream blip) is the real concern.
async fn sleep_backoff(config: &ReconnectConfig, attempt: u32) {
    use rand::RngExt;
    let exp = attempt.saturating_sub(1);
    let multiplier = 2u32.saturating_pow(exp);
    let base = config
        .base_delay
        .saturating_mul(multiplier)
        .min(config.max_delay);
    let jitter: f64 = rand::rng().random_range(0.75..=1.25);
    let secs = base.as_secs_f64() * jitter;
    let delay = if secs.is_finite() && secs >= 0.0 {
        Duration::from_secs_f64(secs).min(config.max_delay)
    } else {
        base
    };
    tokio::time::sleep(delay).await;
}

// `with_json_body` and the streaming path are exercised end-to-end (real
// serialized body observed by a real server) by the integration tests in
// `tests/rest_client_codegen.rs` (`unary_post_round_trip` et al.) — a local
// unit test here could only check "builds without panicking" without
// access to `toolkit_http::RequestBuilder`'s private fields, which is
// strictly weaker than the existing round-trip coverage.
