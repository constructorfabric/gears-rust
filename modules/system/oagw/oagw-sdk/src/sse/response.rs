use std::pin::Pin;

use axum::body::Body;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;

use crate::error::StreamingError;
use crate::sse::ServerEvent;

/// Build an axum Response that streams SSE events to the client.
///
/// Sets `Content-Type: text/event-stream`, `Cache-Control: no-cache`,
/// `Connection: keep-alive`, and `X-Accel-Buffering: no` (to prevent
/// reverse-proxy buffering). Each [`ServerEvent`] is serialized into the
/// SSE wire format.
#[allow(clippy::type_complexity)]
pub fn server_events_response(
    events: Pin<Box<dyn Stream<Item = Result<ServerEvent, StreamingError>> + Send>>,
) -> http::Response<Body> {
    let byte_stream = events.map(|result| {
        result
            .map(|event| serialize_event(&event))
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    });

    http::Response::builder()
        .header(http::header::CONTENT_TYPE, "text/event-stream")
        .header(http::header::CACHE_CONTROL, "no-cache")
        .header(http::header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(byte_stream))
        .expect("SSE response builder should not fail")
}

/// Serialize an SSE event into wire format bytes.
fn serialize_event(event: &ServerEvent) -> Bytes {
    let mut buf = String::new();
    if let Some(ref id) = event.id {
        buf.push_str("id: ");
        buf.push_str(id);
        buf.push('\n');
    }
    if let Some(ref event_type) = event.event {
        buf.push_str("event: ");
        buf.push_str(event_type);
        buf.push('\n');
    }
    if let Some(retry) = event.retry {
        buf.push_str("retry: ");
        buf.push_str(&retry.to_string());
        buf.push('\n');
    }
    // Each line of data gets its own "data:" prefix.
    for line in event.data.split('\n') {
        buf.push_str("data: ");
        buf.push_str(line);
        buf.push('\n');
    }
    buf.push('\n'); // Blank line terminates the event.
    Bytes::from(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_data_only() {
        let event = ServerEvent {
            data: "hello".into(),
            ..Default::default()
        };
        let bytes = serialize_event(&event);
        assert_eq!(bytes.as_ref(), b"data: hello\n\n");
    }

    #[test]
    fn serialize_all_fields() {
        let event = ServerEvent {
            id: Some("42".into()),
            event: Some("update".into()),
            data: "payload".into(),
            retry: Some(3000),
        };
        let bytes = serialize_event(&event);
        let expected = "id: 42\nevent: update\nretry: 3000\ndata: payload\n\n";
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), expected);
    }

    #[test]
    fn serialize_multiline_data() {
        let event = ServerEvent {
            data: "line1\nline2\nline3".into(),
            ..Default::default()
        };
        let bytes = serialize_event(&event);
        let expected = "data: line1\ndata: line2\ndata: line3\n\n";
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), expected);
    }
}
