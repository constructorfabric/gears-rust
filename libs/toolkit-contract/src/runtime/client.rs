//! Per-call helpers used by the generated REST client.
//!
//! The macro keeps emitted code small by funnelling the common
//! "send unary request and decode the response" path through
//! [`send_unary`], and the streaming path through
//! [`send_streaming`].

use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use toolkit_http::RequestBuilder;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::runtime::config::ReconnectConfig;
use crate::runtime::http::{body_to_byte_stream, map_http_error};
use crate::runtime::sse::{LastEventId, parse_sse_stream_with_id};
use crate::runtime::transport_error::TransportError;

/// Boxed byte stream produced from an HTTP response body for SSE parsing.
type BoxByteStream = Pin<
    Box<
        dyn Stream<Item = Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>>
            + Send,
    >,
>;

/// How a unary request body is encoded.
pub enum UnaryBody<'a, T: ?Sized + Serialize> {
    /// No body — used for GET / DELETE.
    None,
    /// JSON-serialized body.
    Json(&'a T),
}

/// Send a unary request and decode the JSON response.
///
/// `build` is a closure returning a `RequestBuilder` configured with method,
/// URL, headers, and any auth state. It is invoked once per attempt.
///
/// # Errors
/// Returns [`TransportError`] when the builder closure fails, the network call fails,
/// the response body cannot be read, JSON deserialization of a success body fails, or the
/// server returns a non-success HTTP status (mapped via [`map_http_error`]).
pub async fn send_unary<F, R>(build: F) -> Result<R, TransportError>
where
    F: FnOnce() -> Result<RequestBuilder, TransportError>,
    R: DeserializeOwned,
{
    let response = build()?.send().await.map_err(TransportError::network)?;
    let status = response.status();
    if status.is_success() {
        // `HttpResponse::bytes` does NOT enforce status check; we already did.
        // Avoiding `.json()` here because it would re-check status and
        // duplicate work on the success path.
        let bytes = response.bytes().await.map_err(TransportError::network)?;
        serde_json::from_slice::<R>(&bytes).map_err(TransportError::serialization)
    } else {
        let bytes = response.bytes().await.map_err(TransportError::network)?;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        Err(map_http_error(status.as_u16(), body))
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
                let bytes = response.bytes().await.map_err(TransportError::network)?;
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let err = map_http_error(status_code, body);
                // Non-success responses are typically domain errors —
                // bubble straight through without reconnect (server
                // explicitly told us "no").
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
            let mut stream_err: Option<TransportError> = None;
            loop {
                let next = inner.next();
                let item = match timeout {
                    Some(d) => if let Ok(v) = tokio::time::timeout(d, next).await { v } else {
                        stream_err = Some(TransportError::Timeout(d));
                        break;
                    },
                    None => next.await,
                };
                match item {
                    Some(Ok(v)) => yield v,
                    Some(Err(e)) => {
                        stream_err = Some(e);
                        break;
                    }
                    None => break,
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // The streaming path is exercised end-to-end in the demo's integration
    // tests once the generated client is wired up. Here we just sanity-check
    // that `with_json_body` returns a builder that the underlying client
    // accepts. `toolkit_http::HttpClient::new()` spawns the buffered tower
    // service on the current Tokio runtime, so this has to be a
    // `#[tokio::test]`.
    #[tokio::test]
    async fn with_json_body_compiles_and_chains() {
        let client = toolkit_http::HttpClient::new()
            .expect("default toolkit-http build is infallible in standard env");
        let req = client.post("https://example.invalid/test");
        drop(with_json_body(req, &serde_json::json!({ "k": "v" })).expect("json body"));
    }
}
