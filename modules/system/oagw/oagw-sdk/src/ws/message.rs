//! Framework-agnostic WebSocket message types.

use std::pin::Pin;

use futures_core::Stream;
use futures_util::sink::Sink;

use crate::error::StreamingError;

/// A WebSocket message, independent of any WS library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// UTF-8 text message.
    Text(String),
    /// Binary message.
    Binary(Vec<u8>),
    /// Ping frame (keep-alive).
    Ping(Vec<u8>),
    /// Pong frame (keep-alive response).
    Pong(Vec<u8>),
    /// Close frame with optional code and reason.
    Close(Option<WebSocketCloseFrame>),
}

/// WebSocket close frame with status code and reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketCloseFrame {
    /// Close status code (RFC 6455 section 7.4).
    pub code: u16,
    /// UTF-8 encoded reason string.
    pub reason: String,
}

/// A sink for sending WebSocket messages.
pub type WebSocketSink = Pin<Box<dyn Sink<WebSocketMessage, Error = StreamingError> + Send>>;

/// A stream for receiving WebSocket messages.
pub type WebSocketReceiver =
    Pin<Box<dyn Stream<Item = Result<WebSocketMessage, StreamingError>> + Send>>;
