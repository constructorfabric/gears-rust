use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::StreamExt;
use http::{HeaderMap, StatusCode};

use crate::body::Body;
use crate::codec::Json;
use crate::error::StreamingError;
use crate::sse::{ServerEvent, is_server_events_response, parse_server_events_stream};

/// Trait for types that can be extracted from an SSE event.
///
/// Implement this trait manually only when you need custom parsing logic.
pub trait FromServerEvent: Sized + Send + 'static {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError>;
}

/// Pass-through: raw `ServerEvent` requires no conversion.
impl FromServerEvent for ServerEvent {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        Ok(event)
    }
}

impl<T> FromServerEvent for Json<T>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        event
            .json()
            .map(Json)
            .map_err(|e| StreamingError::ServerEventsParse {
                detail: e.to_string(),
            })
    }
}

/// The result of trying to interpret an HTTP response as a server-sent events stream.
///
/// Both variants are valid outcomes — use `match` to handle the streaming
/// and non-streaming paths:
///
/// ```ignore
/// match ServerEventsStream::from_response::<ServerEvent>(resp) {
///     ServerEventsResponse::Events(mut events) => {
///         while let Some(event) = events.next().await { /* ... */ }
///     }
///     ServerEventsResponse::Response(resp) => {
///         // handle as a regular HTTP response
///     }
/// }
/// ```
pub enum ServerEventsResponse<T: FromServerEvent = ServerEvent> {
    /// The response was `text/event-stream` — consume events from the stream.
    Events(ServerEventsStream<T>),
    /// The response was not SSE — the original response is returned intact.
    Response(http::Response<Body>),
}

/// A stream of server-sent events extracted from an HTTP response.
///
/// Generic over the event type `T`:
/// - `ServerEventsStream<ServerEvent>` (default) — yields raw parsed events.
/// - `ServerEventsStream<YourType>` — yields events deserialized via
///   [`FromServerEvent`].
///
/// Created via [`from_response`](ServerEventsStream::from_response), which
/// checks the `Content-Type` header and returns a [`ServerEventsResponse`]
/// — either an event stream or the original response unchanged.
#[allow(clippy::type_complexity)]
pub struct ServerEventsStream<T: FromServerEvent = ServerEvent> {
    inner: Pin<Box<dyn Stream<Item = Result<T, StreamingError>> + Send>>,
    status: StatusCode,
    headers: HeaderMap,
}

impl<T: FromServerEvent> std::fmt::Debug for ServerEventsStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerEventsStream")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl ServerEventsStream {
    /// Try to interpret an HTTP response as a server-sent events stream.
    ///
    /// Returns [`ServerEventsResponse::Events`] if the response has
    /// `Content-Type: text/event-stream`. Each event's `data` field is
    /// converted via [`FromServerEvent::from_server_event`].
    ///
    /// Returns [`ServerEventsResponse::Response`] with the **original response**
    /// if it's not SSE, so you can fall back to normal processing without
    /// losing the response.
    pub fn from_response<T: FromServerEvent>(
        resp: impl Into<http::Response<Body>>,
    ) -> ServerEventsResponse<T> {
        let resp = resp.into();
        if !is_server_events_response(resp.headers()) {
            return ServerEventsResponse::Response(resp);
        }

        let (parts, body) = resp.into_parts();
        let event_stream = parse_server_events_stream(body.into_stream());
        let mapped = event_stream.map(|r| r.and_then(T::from_server_event));

        ServerEventsResponse::Events(ServerEventsStream {
            inner: Box::pin(mapped),
            status: parts.status,
            headers: parts.headers,
        })
    }
}

impl<T: FromServerEvent> ServerEventsStream<T> {
    /// The HTTP status code of the original response.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The HTTP headers of the original response.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}

#[cfg(feature = "axum")]
impl ServerEventsStream<ServerEvent> {
    /// Convert this stream into an HTTP response suitable for sending to clients.
    ///
    /// Sets appropriate SSE headers:
    /// - `Content-Type: text/event-stream`
    /// - `Cache-Control: no-cache`
    /// - `Connection: keep-alive`
    /// - `X-Accel-Buffering: no` (prevents reverse-proxy buffering)
    pub fn into_response(self) -> http::Response<axum::body::Body> {
        crate::sse::server_events_response(self.inner)
    }
}

impl<T: FromServerEvent> Stream for ServerEventsStream<T> {
    type Item = Result<T, StreamingError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
