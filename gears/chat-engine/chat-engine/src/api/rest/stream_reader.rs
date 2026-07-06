//! SSE buffer-reader for the resumable delta stream (FR-024, true live-tail).
//!
//! In the live-tail model the streaming driver runs detached and writes every
//! wire event to the [`StreamEventBuffer`]; HTTP connections (the initial
//! `POST` response *and* `Last-Event-ID` reconnects) are independent **readers**
//! that poll the buffer from a starting `seq` and emit SSE frames until a
//! terminal event (`complete`/`error`). A client disconnect cancels the reader
//! only — the driver keeps writing, so a reconnect resumes seamlessly.
//
// @cpt-cf-chat-engine-design-stream-resume:p2
// @cpt-cf-chat-engine-adr-stream-resumability:p2

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use futures::stream::{self, Stream, StreamExt};
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::ports::StreamEventBuffer;

/// How often the reader polls the buffer for newly-appended events.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Consecutive empty polls before the reader gives up and closes the stream
/// (the client then reconciles the final document via `GET /messages/{id}`).
/// `MAX_IDLE_POLLS * POLL_INTERVAL` ≈ the live-tail wait budget.
const MAX_IDLE_POLLS: u32 = 1200; // ~60s

/// `true` for the terminal wire events that end a stream.
fn is_terminal(event: &JsonValue) -> bool {
    matches!(
        event.get("type").and_then(JsonValue::as_str),
        Some("message.complete") | Some("message.error")
    )
}

/// Serialize a buffered event into an SSE frame:
/// `id: <seq>\nevent: <type>\ndata: <json>\n\n`.
fn sse_frame(seq: u64, event: &JsonValue) -> Vec<u8> {
    let name = event
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("message.delta");
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    format!("id: {seq}\nevent: {name}\ndata: {data}\n\n").into_bytes()
}

struct ReaderState {
    buffer: Arc<dyn StreamEventBuffer>,
    message_id: Uuid,
    last_seq: Option<u64>,
    done: bool,
    idle_polls: u32,
    cancel: CancellationToken,
}

/// Build the byte stream of SSE frames by polling the buffer from `from_seq`.
/// Each yielded item is a chunk of one or more concatenated SSE frames. The
/// stream ends on a terminal event, the idle cap, or client cancellation.
pub fn buffer_reader_stream(
    buffer: Arc<dyn StreamEventBuffer>,
    message_id: Uuid,
    from_seq: Option<u64>,
    cancel: CancellationToken,
) -> impl Stream<Item = Vec<u8>> {
    let state = ReaderState {
        buffer,
        message_id,
        last_seq: from_seq,
        done: false,
        idle_polls: 0,
        cancel,
    };

    stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        loop {
            if st.cancel.is_cancelled() {
                return None;
            }
            match st.buffer.read_since(st.message_id, st.last_seq).await {
                Ok(batch) if !batch.is_empty() => {
                    st.idle_polls = 0;
                    let mut bytes = Vec::new();
                    for ev in &batch {
                        st.last_seq = Some(ev.seq);
                        bytes.extend_from_slice(&sse_frame(ev.seq, &ev.event));
                        if is_terminal(&ev.event) {
                            st.done = true;
                        }
                    }
                    return Some((bytes, st));
                }
                Ok(_) => {
                    st.idle_polls += 1;
                    if st.idle_polls >= MAX_IDLE_POLLS {
                        return None;
                    }
                    tokio::select! {
                        () = st.cancel.cancelled() => return None,
                        () = tokio::time::sleep(POLL_INTERVAL) => {}
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, message_id = %st.message_id,
                        "stream resume buffer read failed; closing reader");
                    return None;
                }
            }
        }
    })
}

/// Wrap [`buffer_reader_stream`] in a `text/event-stream` response. Used by the
/// initial streaming response and the `Last-Event-ID` reconnect endpoint.
pub fn sse_buffer_reader_response(
    buffer: Arc<dyn StreamEventBuffer>,
    message_id: Uuid,
    from_seq: Option<u64>,
    cancel: CancellationToken,
) -> Response {
    // A reader is just a *view* of the buffer — when the client disconnects we
    // stop polling (cancel the reader) but the driver keeps writing, so a later
    // reconnect resumes. This cancels reading only, never generation.
    let guard = cancel.clone().drop_guard();
    let body = buffer_reader_stream(buffer, message_id, from_seq, cancel).map(move |b| {
        let _keep = &guard;
        Ok::<Vec<u8>, Infallible>(b)
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .header("x-accel-buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(body))
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "failed to build SSE reader response");
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

#[cfg(test)]
#[path = "stream_reader_tests.rs"]
mod stream_reader_tests;
