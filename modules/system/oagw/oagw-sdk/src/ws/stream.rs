//! High-level WebSocket stream abstraction.

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_core::Stream;
use futures_util::{SinkExt, StreamExt};

use crate::body::{BodyStream, BoxError};
use crate::codec::Json;
use crate::error::StreamingError;
use crate::ws::message::{
    WebSocketMessage, WebSocketReceiver as RawReceiver, WebSocketSink as RawSink,
};

// ---------------------------------------------------------------------------
// FromWebSocketMessage trait
// ---------------------------------------------------------------------------

/// Trait for types that can be converted to/from [`WebSocketMessage`].
///
/// Only Text and Binary messages reach this trait — control frames (Ping, Pong,
/// Close) are handled transparently by [`WebSocketStream`].
pub trait FromWebSocketMessage: Sized + Send + 'static {
    fn from_ws_message(msg: WebSocketMessage) -> Result<Self, StreamingError>;
    fn to_ws_message(&self) -> WebSocketMessage;
}

/// Pass-through: raw [`WebSocketMessage`] requires no conversion.
impl FromWebSocketMessage for WebSocketMessage {
    fn from_ws_message(msg: WebSocketMessage) -> Result<Self, StreamingError> {
        Ok(msg)
    }

    fn to_ws_message(&self) -> WebSocketMessage {
        self.clone()
    }
}

/// JSON serialization/deserialization for WebSocket text messages.
impl<T> FromWebSocketMessage for Json<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    fn from_ws_message(msg: WebSocketMessage) -> Result<Self, StreamingError> {
        match msg {
            WebSocketMessage::Text(text) => {
                serde_json::from_str(&text)
                    .map(Json)
                    .map_err(|e| StreamingError::WebSocketBridge {
                        detail: e.to_string(),
                    })
            }
            _ => Err(StreamingError::WebSocketBridge {
                detail: "expected Text message for JSON deserialization, got Binary".into(),
            }),
        }
    }

    fn to_ws_message(&self) -> WebSocketMessage {
        let json = serde_json::to_string(&self.0).expect("JSON serialization should not fail");
        WebSocketMessage::Text(json)
    }
}

// ---------------------------------------------------------------------------
// WebSocketStream
// ---------------------------------------------------------------------------

/// A bidirectional WebSocket stream with typed messages.
///
/// Generic over the message type `T`:
/// - `WebSocketStream` (default) — raw [`WebSocketMessage`] pass-through.
/// - `WebSocketStream<Json<MyType>>` — automatic JSON serialization.
/// - `WebSocketStream<MyType>` — custom conversion via [`FromWebSocketMessage`].
pub struct WebSocketStream<T: FromWebSocketMessage = WebSocketMessage> {
    sink: RawSink,
    receiver: RawReceiver,
    _marker: PhantomData<fn() -> T>,
}

// --- Construction ---

impl From<(RawSink, RawReceiver)> for WebSocketStream {
    fn from((sink, receiver): (RawSink, RawReceiver)) -> Self {
        Self {
            sink,
            receiver,
            _marker: PhantomData,
        }
    }
}

#[cfg(feature = "axum")]
impl From<axum::extract::ws::WebSocket> for WebSocketStream {
    fn from(socket: axum::extract::ws::WebSocket) -> Self {
        crate::ws::axum_adapter::split(socket).into()
    }
}

// --- Typed operations ---

impl<T: FromWebSocketMessage> WebSocketStream<T> {
    /// Send a typed message.
    pub async fn send(&mut self, msg: &T) -> Result<(), StreamingError> {
        let raw = msg.to_ws_message();
        self.sink
            .send(raw)
            .await
            .map_err(|e| StreamingError::WebSocketBridge {
                detail: e.to_string(),
            })
    }

    /// Receive the next typed message.
    ///
    /// Ping/Pong frames are silently skipped. Returns `None` when the
    /// connection is closed (Close frame or stream end).
    pub async fn recv(&mut self) -> Option<Result<T, StreamingError>> {
        loop {
            match self.receiver.next().await? {
                Ok(msg) => match msg {
                    WebSocketMessage::Ping(_) | WebSocketMessage::Pong(_) => continue,
                    WebSocketMessage::Close(_) => return None,
                    data => return Some(T::from_ws_message(data)),
                },
                Err(e) => return Some(Err(e)),
            }
        }
    }

    /// Close the connection gracefully.
    pub async fn close(mut self) -> Result<(), StreamingError> {
        self.sink
            .send(WebSocketMessage::Close(None))
            .await
            .map_err(|e| StreamingError::WebSocketBridge {
                detail: e.to_string(),
            })
    }

    /// Split into separate send/receive halves for concurrent use.
    pub fn split(self) -> (WebSocketSender<T>, WebSocketStreamReceiver<T>) {
        (
            WebSocketSender {
                sink: self.sink,
                _marker: PhantomData,
            },
            WebSocketStreamReceiver {
                receiver: self.receiver,
                _marker: PhantomData,
            },
        )
    }
}

impl<T: FromWebSocketMessage> Stream for WebSocketStream<T> {
    type Item = Result<T, StreamingError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.receiver.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(msg))) => match msg {
                    WebSocketMessage::Ping(_) | WebSocketMessage::Pong(_) => continue,
                    WebSocketMessage::Close(_) => return Poll::Ready(None),
                    data => return Poll::Ready(Some(T::from_ws_message(data))),
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocketSender / WebSocketStreamReceiver (split halves)
// ---------------------------------------------------------------------------

/// The send half of a split [`WebSocketStream`].
pub struct WebSocketSender<T: FromWebSocketMessage = WebSocketMessage> {
    sink: RawSink,
    _marker: PhantomData<fn() -> T>,
}

impl<T: FromWebSocketMessage> WebSocketSender<T> {
    /// Send a typed message.
    pub async fn send(&mut self, msg: &T) -> Result<(), StreamingError> {
        let raw = msg.to_ws_message();
        self.sink
            .send(raw)
            .await
            .map_err(|e| StreamingError::WebSocketBridge {
                detail: e.to_string(),
            })
    }
}

impl WebSocketSender {
    /// Forward a [`BodyStream`] as WebSocket text messages.
    ///
    /// Each `Bytes` chunk from the stream is sent as a `Text` message.
    /// Completes when the stream ends or an error occurs.
    pub async fn forward_body_stream(
        &mut self,
        mut stream: BodyStream,
    ) -> Result<(), StreamingError> {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let msg = match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => WebSocketMessage::Text(text),
                        Err(e) => WebSocketMessage::Binary(e.into_bytes()),
                    };
                    self.sink.send(msg).await?;
                }
                Err(e) => return Err(StreamingError::Stream(e)),
            }
        }
        Ok(())
    }
}

/// The receive half of a split [`WebSocketStream`].
pub struct WebSocketStreamReceiver<T: FromWebSocketMessage = WebSocketMessage> {
    receiver: RawReceiver,
    _marker: PhantomData<fn() -> T>,
}

impl<T: FromWebSocketMessage> WebSocketStreamReceiver<T> {
    /// Receive the next typed message.
    ///
    /// Ping/Pong frames are silently skipped. Returns `None` on close.
    pub async fn recv(&mut self) -> Option<Result<T, StreamingError>> {
        loop {
            match self.receiver.next().await? {
                Ok(msg) => match msg {
                    WebSocketMessage::Ping(_) | WebSocketMessage::Pong(_) => continue,
                    WebSocketMessage::Close(_) => return None,
                    data => return Some(T::from_ws_message(data)),
                },
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl WebSocketStreamReceiver {
    /// Convert this receiver into a [`BodyStream`] for use as a proxy request body.
    ///
    /// Text and Binary messages become `Bytes` chunks. Control frames (Ping, Pong)
    /// are filtered. The stream terminates on Close or end-of-stream.
    pub fn into_body_stream(self) -> BodyStream {
        Box::pin(futures_util::stream::unfold(
            self.receiver,
            |mut rx| async {
                loop {
                    match rx.next().await? {
                        Ok(WebSocketMessage::Text(text)) => {
                            return Some((Ok(Bytes::from(text)), rx));
                        }
                        Ok(WebSocketMessage::Binary(data)) => {
                            return Some((Ok(Bytes::from(data)), rx));
                        }
                        Ok(WebSocketMessage::Close(_)) => return None,
                        Ok(_) => continue,
                        Err(e) => {
                            return Some((Err(Box::new(e) as BoxError), rx));
                        }
                    }
                }
            },
        ))
    }
}

impl<T: FromWebSocketMessage> Stream for WebSocketStreamReceiver<T> {
    type Item = Result<T, StreamingError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.receiver.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(msg))) => match msg {
                    WebSocketMessage::Ping(_) | WebSocketMessage::Pong(_) => continue,
                    WebSocketMessage::Close(_) => return Poll::Ready(None),
                    data => return Poll::Ready(Some(T::from_ws_message(data))),
                },
            }
        }
    }
}
