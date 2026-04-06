use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use futures_util::StreamExt;
use serde::Serialize;
use std::{borrow::Cow, convert::Infallible, time::Duration};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Small typed SSE broadcaster built on `tokio::sync::broadcast`.
/// - T must be `Clone` so multiple subscribers can receive the same payload.
/// - Bounded channel drops oldest events when subscribers lag (by design).
#[derive(Clone)]
pub struct SseBroadcaster<T> {
    tx: broadcast::Sender<T>,
}

impl<T: Clone + Send + 'static> SseBroadcaster<T> {
    /// Create a broadcaster with bounded buffer capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Broadcast a single message to current subscribers.
    /// Errors are ignored to keep the hot path cheap (e.g., no active subscribers).
    pub fn send(&self, value: T) {
        if self.tx.send(value).is_err() {
            tracing::trace!("SSE broadcast: no active receivers");
        }
    }

    /// Subscribe to a typed stream of messages; lag/drop errors are filtered out.
    pub fn subscribe_stream(&self) -> impl Stream<Item = T> + use<T> {
        BroadcastStream::new(self.tx.subscribe()).filter_map(|res| async move { res.ok() })
    }

    /// Convert a typed stream into an SSE stream with JSON payloads (no event name).
    fn wrap_stream_as_sse<U>(stream: U) -> impl Stream<Item = Result<Event, Infallible>>
    where
        U: Stream<Item = T>,
        T: Serialize,
    {
        stream.map(|msg| {
            let ev = Event::default().json_data(&msg).unwrap_or_else(|_| {
                // Fallback to a tiny text marker instead of breaking the stream.
                Event::default().data("serialization_error")
            });
            Ok(ev)
        })
    }

    /// Convert a typed stream into an SSE stream with JSON payloads and a constant `event:` name.
    fn wrap_stream_as_sse_named<U>(
        stream: U,
        event_name: Cow<'static, str>,
    ) -> impl Stream<Item = Result<Event, Infallible>>
    where
        U: Stream<Item = T>,
        T: Serialize,
    {
        stream.map(move |msg| {
            let ev = Event::default()
                .event(&event_name) // <-- set event name
                .json_data(&msg)
                .unwrap_or_else(|_| {
                    Event::default()
                        .event(&event_name)
                        .data("serialization_error")
                });
            Ok(ev)
        })
    }

    // -------------------------
    // Plain (unnamed) variants
    // -------------------------

    /// Plain SSE (no extra headers), unnamed events.
    /// Includes periodic keepalive pings to avoid idle timeouts.
    pub fn sse_response(&self) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<T>>
    where
        T: Serialize,
    {
        let stream = Self::wrap_stream_as_sse(self.subscribe_stream());
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
    }

    /// SSE with custom headers applied on top of the Sse response (unnamed events).
    pub fn sse_response_with_headers<I>(&self, headers: I) -> axum::response::Response
    where
        T: Serialize,
        I: IntoIterator<Item = (axum::http::HeaderName, axum::http::HeaderValue)>,
    {
        let mut resp = self.sse_response().into_response();
        let dst = resp.headers_mut();
        for (name, value) in headers {
            dst.insert(name, value);
        }
        resp
    }

    // -------------------------
    // Named-event variants
    // -------------------------

    /// Plain SSE with a constant `event:` name for all messages (no extra headers).
    pub fn sse_response_named<N>(
        &self,
        event_name: N,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<T, N>>
    where
        T: Serialize,
        N: Into<Cow<'static, str>> + 'static,
    {
        let stream = Self::wrap_stream_as_sse_named(self.subscribe_stream(), event_name.into());
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
    }

    /// SSE with custom headers and a constant `event:` name for all messages.
    pub fn sse_response_named_with_headers<I>(
        &self,
        event_name: impl Into<Cow<'static, str>> + 'static,
        headers: I,
    ) -> axum::response::Response
    where
        T: Serialize,
        I: IntoIterator<Item = (axum::http::HeaderName, axum::http::HeaderValue)>,
    {
        let mut resp = self.sse_response_named(event_name).into_response();
        let dst = resp.headers_mut();
        for (name, value) in headers {
            dst.insert(name, value);
        }
        resp
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "sse_tests.rs"]
mod tests;
