//! Axum adapter for the WebSocket abstraction.
//!
//! Provides conversion between `axum::extract::ws::Message` and `WebSocketMessage`,
//! and a `split` function returning abstract `(WebSocketSink, WebSocketReceiver)`.

use axum::extract::ws::{self, WebSocket};
use futures_util::{SinkExt, StreamExt};

use crate::error::StreamingError;
use crate::ws::message::{WebSocketCloseFrame, WebSocketMessage, WebSocketReceiver, WebSocketSink};

/// Convert an `axum::extract::ws::Message` to `WebSocketMessage`.
pub fn from_axum(msg: ws::Message) -> WebSocketMessage {
    match msg {
        ws::Message::Text(text) => WebSocketMessage::Text(text.to_string()),
        ws::Message::Binary(data) => WebSocketMessage::Binary(data.to_vec()),
        ws::Message::Ping(data) => WebSocketMessage::Ping(data.to_vec()),
        ws::Message::Pong(data) => WebSocketMessage::Pong(data.to_vec()),
        ws::Message::Close(frame) => WebSocketMessage::Close(frame.map(|f| WebSocketCloseFrame {
            code: f.code,
            reason: f.reason.to_string(),
        })),
    }
}

/// Convert a `WebSocketMessage` to `axum::extract::ws::Message`.
pub fn to_axum(msg: WebSocketMessage) -> ws::Message {
    match msg {
        WebSocketMessage::Text(text) => ws::Message::Text(text.into()),
        WebSocketMessage::Binary(data) => ws::Message::Binary(data.into()),
        WebSocketMessage::Ping(data) => ws::Message::Ping(data.into()),
        WebSocketMessage::Pong(data) => ws::Message::Pong(data.into()),
        WebSocketMessage::Close(frame) => ws::Message::Close(frame.map(|f| ws::CloseFrame {
            code: f.code,
            reason: f.reason.into(),
        })),
    }
}

/// Split an axum WebSocket into abstract `(WebSocketSink, WebSocketReceiver)`.
pub fn split(socket: WebSocket) -> (WebSocketSink, WebSocketReceiver) {
    let (tx, rx) = socket.split();

    // Wrap the sink: map errors and convert WebSocketMessage → axum::ws::Message
    let sink: WebSocketSink = Box::pin(
        tx.sink_map_err(|e| StreamingError::WebSocketBridge {
            detail: e.to_string(),
        })
        .with(|msg: WebSocketMessage| async move { Ok(to_axum(msg)) }),
    );

    // Wrap the receiver to convert axum::ws::Message → WebSocketMessage
    let receiver: WebSocketReceiver = Box::pin(rx.map(|result| {
        result
            .map(from_axum)
            .map_err(|e| StreamingError::WebSocketBridge {
                detail: e.to_string(),
            })
    }));

    (sink, receiver)
}
