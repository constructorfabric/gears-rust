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

use crate::infra::db::repo::stream_event_repo::StreamEventBuffer;

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
        Some("complete") | Some("error")
    )
}

/// Serialize a buffered event into an SSE frame:
/// `id: <seq>\nevent: <type>\ndata: <json>\n\n`.
fn sse_frame(seq: u64, event: &JsonValue) -> Vec<u8> {
    let name = event
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("delta");
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
mod tests {
    use super::*;
    use crate::domain::error::ChatEngineError;
    use crate::infra::db::repo::stream_event_repo::BufferedEvent;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use serde_json::json;
    use time::OffsetDateTime;

    /// In-memory fake buffer for deterministic reader tests.
    #[derive(Default)]
    struct FakeBuffer {
        events: Mutex<Vec<(Uuid, u64, JsonValue)>>,
    }

    #[async_trait]
    impl StreamEventBuffer for FakeBuffer {
        async fn append(
            &self,
            message_id: Uuid,
            seq: u64,
            event: JsonValue,
            _expires_at: OffsetDateTime,
        ) -> Result<(), ChatEngineError> {
            self.events.lock().push((message_id, seq, event));
            Ok(())
        }

        async fn read_since(
            &self,
            message_id: Uuid,
            after_seq: Option<u64>,
        ) -> Result<Vec<BufferedEvent>, ChatEngineError> {
            let evs = self.events.lock();
            Ok(evs
                .iter()
                .filter(|(mid, seq, _)| *mid == message_id && after_seq.is_none_or(|a| *seq > a))
                .map(|(_, seq, event)| BufferedEvent {
                    seq: *seq,
                    event: event.clone(),
                })
                .collect())
        }

        async fn delete_expired(&self, _now: OffsetDateTime) -> Result<u64, ChatEngineError> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn reader_drains_buffered_stream_until_terminal() {
        let mid = Uuid::new_v4();
        let buf = Arc::new(FakeBuffer::default());
        let ttl = OffsetDateTime::now_utc();
        buf.append(mid, 0, json!({"type": "start", "message_id": mid, "seq": 0}), ttl)
            .await
            .unwrap();
        buf.append(
            mid,
            1,
            json!({"type": "delta", "seq": 1, "op": "append", "path": "parts/0/content/text", "value": "hi"}),
            ttl,
        )
        .await
        .unwrap();
        buf.append(mid, 2, json!({"type": "complete", "seq": 2}), ttl)
            .await
            .unwrap();

        // Terminal event is already buffered → the reader drains in one batch
        // and ends without any live polling.
        let chunks: Vec<Vec<u8>> = buffer_reader_stream(buf, mid, None, CancellationToken::new())
            .collect()
            .await;
        let text: String = chunks
            .into_iter()
            .map(|b| String::from_utf8(b).unwrap())
            .collect();
        assert!(text.contains("event: start"));
        assert!(text.contains("event: delta"));
        assert!(text.contains("event: complete"));
        assert!(text.contains("id: 2"));
    }

    #[tokio::test]
    async fn reader_replays_only_after_last_event_id() {
        let mid = Uuid::new_v4();
        let buf = Arc::new(FakeBuffer::default());
        let ttl = OffsetDateTime::now_utc();
        for seq in 0..3u64 {
            let ty = if seq == 2 { "complete" } else { "delta" };
            buf.append(mid, seq, json!({"type": ty, "seq": seq}), ttl)
                .await
                .unwrap();
        }
        // Resume from seq 0 → only seq 1 and 2 (terminal) are replayed.
        let chunks: Vec<Vec<u8>> =
            buffer_reader_stream(buf, mid, Some(0), CancellationToken::new())
                .collect()
                .await;
        let text: String = chunks
            .into_iter()
            .map(|b| String::from_utf8(b).unwrap())
            .collect();
        assert!(!text.contains("id: 0\n"));
        assert!(text.contains("id: 1"));
        assert!(text.contains("id: 2"));
    }

    #[tokio::test]
    async fn cancelled_reader_stops() {
        let mid = Uuid::new_v4();
        let buf = Arc::new(FakeBuffer::default());
        // No terminal event buffered; a pre-cancelled token must stop the
        // reader immediately instead of polling forever.
        let cancel = CancellationToken::new();
        cancel.cancel();
        let chunks: Vec<Vec<u8>> = buffer_reader_stream(buf, mid, None, cancel).collect().await;
        assert!(chunks.is_empty());
    }
}
