//! Server-Sent Events (SSE) parser used by streaming clients.
//!
//! Translates a byte stream into a stream of typed events. Recognises:
//! - `data: <json>` — emits `Ok(T)` after JSON-deserializing into `T`.
//! - `event: error` — the next `data:` is parsed as a `ProblemDetails`
//!   wrapped in [`TransportError::Problem`].
//! - `event: done` — terminates the stream.
//!
//! All other event types are ignored. Comments (lines starting with `:`) and
//! blank lines are stripped per the SSE spec.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;

use toolkit_canonical_errors::Problem;

use crate::runtime::transport_error::TransportError;

/// Adapter that lifts a `Display`-only error into an `Error + Send + Sync + 'static`
/// so it can be boxed into [`TransportError::Network`] without losing the
/// original message.
#[derive(Debug)]
struct DisplayError(String);
impl std::fmt::Display for DisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DisplayError {}

/// Shared cell holding the latest seen SSE `id:` field. The streaming
/// client clones this handle before constructing the parser; on stream
/// interruption it reads the latest ID and re-issues the request with a
/// `Last-Event-ID` header (per HTML5 `EventSource` spec).
///
/// Wraps `Arc<RwLock<Option<String>>>` as a newtype so the underlying lock
/// implementation isn't part of the public surface — the `parking_lot` vs
/// `tokio::sync` choice can change without a breaking SDK release.
#[derive(Clone, Debug, Default)]
pub struct LastEventId(Arc<RwLock<Option<String>>>);

impl LastEventId {
    /// Create an empty [`LastEventId`] cell.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Snapshot the latest ID value, if any.
    #[must_use]
    pub fn current(&self) -> Option<String> {
        self.0.read().clone()
    }

    /// Replace the latest ID. `None` clears the cell (per HTML5 spec, an
    /// empty `id:` field resets the saved value).
    pub fn set(&self, value: Option<String>) {
        *self.0.write() = value;
    }
}

/// Parse an SSE byte stream into a stream of typed events.
///
/// `bytes` is typically the byte-stream view of
/// `toolkit_http::HttpResponse::into_body()` (adapted via
/// [`crate::runtime::http::body_to_byte_stream`]). Errors from the inner
/// stream are surfaced as [`TransportError::Network`].
///
/// To capture `id:` fields for `Last-Event-ID` reconnect, use
/// [`parse_sse_stream_with_id`] and pass in a shared cell that the
/// streaming client can read from.
pub fn parse_sse_stream<T, S, E>(bytes: S) -> SseStream<T, S>
where
    T: DeserializeOwned + 'static,
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    E: std::fmt::Display,
{
    parse_sse_stream_with_id(bytes, LastEventId::empty())
}

/// Same as [`parse_sse_stream`] but accepts a [`LastEventId`] cell that the
/// parser updates whenever it encounters an `id:` field. Streaming clients
/// hand the cell into the request-factory closure on reconnect to populate
/// the `Last-Event-ID` header — per HTML5 `EventSource` spec.
pub fn parse_sse_stream_with_id<T, S, E>(bytes: S, last_event_id: LastEventId) -> SseStream<T, S>
where
    T: DeserializeOwned + 'static,
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    E: std::fmt::Display,
{
    SseStream {
        inner: bytes,
        buf: BytesMut::with_capacity(4 * 1024),
        pending: VecDeque::new(),
        event_kind: None,
        event_data: String::new(),
        event_id: None,
        done: false,
        last_event_id,
        _marker: std::marker::PhantomData,
    }
}

/// Iterator yielded by [`parse_sse_stream`].
pub struct SseStream<T, S> {
    inner: S,
    buf: BytesMut,
    pending: VecDeque<Result<T, TransportError>>,
    /// Last `event:` value seen since the previous dispatch. `None` means
    /// the implicit default `"message"`.
    event_kind: Option<String>,
    /// `data:` payload accumulated for the current event, with multiple
    /// `data:` lines joined by `\n` (per W3C SSE spec).
    event_data: String,
    /// Last `id:` value seen for the current event. Per spec, the
    /// last-event-id persists across dispatches; this field is just the
    /// per-event scratch used to update [`LastEventId`] on dispatch.
    event_id: Option<String>,
    done: bool,
    last_event_id: LastEventId,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T, S> SseStream<T, S> {
    /// Returns a clone of the shared cell that captures the latest `id:`
    /// field seen on the stream. The streaming client uses this to populate
    /// the `Last-Event-ID` header on reconnect.
    #[must_use]
    pub fn last_event_id_handle(&self) -> LastEventId {
        self.last_event_id.clone()
    }
}

// `inner` is bounded by `Unpin` at construction; the rest of the fields are
// trivially `Unpin`. Implement `Unpin` unconditionally so callers can poll
// `Pin<&mut SseStream<...>>` without pinning the type itself.
impl<T, S: Unpin> Unpin for SseStream<T, S> {}

impl<T, S, E> Stream for SseStream<T, S>
where
    T: DeserializeOwned + 'static,
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    E: std::fmt::Display,
{
    type Item = Result<T, TransportError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Some(item) = this.pending.pop_front() {
                return Poll::Ready(Some(item));
            }
            if this.done {
                return Poll::Ready(None);
            }

            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.done = true;
                    // Flush any trailing partial buffer as a final line.
                    drain_remaining(
                        &mut this.buf,
                        &mut this.event_kind,
                        &mut this.event_data,
                        &mut this.event_id,
                        &mut this.pending,
                        &this.last_event_id,
                    );
                }
                Poll::Ready(Some(Err(e))) => {
                    this.done = true;
                    // `E: Display` only — wrap in a small Display->Error
                    // adapter so the source chain stays intact through
                    // `TransportError::network`.
                    return Poll::Ready(Some(Err(TransportError::network(DisplayError(
                        e.to_string(),
                    )))));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buf.extend_from_slice(&chunk);
                    let saw_done = drain_buffer(
                        &mut this.buf,
                        &mut this.event_kind,
                        &mut this.event_data,
                        &mut this.event_id,
                        &mut this.pending,
                        &this.last_event_id,
                    );
                    if saw_done {
                        this.done = true;
                    }
                }
            }
        }
    }
}

/// Drain complete lines from `buf`, mutating the per-event accumulator
/// fields and pushing dispatched events to `out`. Returns `true` iff a
/// `done` event was dispatched (caller terminates the stream).
fn drain_buffer<T: DeserializeOwned + 'static>(
    buf: &mut BytesMut,
    event_kind: &mut Option<String>,
    event_data: &mut String,
    event_id: &mut Option<String>,
    out: &mut VecDeque<Result<T, TransportError>>,
    last_event_id: &LastEventId,
) -> bool {
    let mut saw_done = false;
    while let Some(line_end) = find_line_end(buf) {
        let line_bytes = buf.split_to(line_end.consumed);
        // SSE wire format mandates UTF-8 (RFC 8259 § 8.1, EventSource spec).
        // Surface non-conforming server output as a typed transport error
        // instead of silently dropping the line — invisible data loss is
        // worse than a propagated error.
        match std::str::from_utf8(&line_bytes[..line_end.line_len]) {
            Ok(line) => {
                if process_line(line, event_kind, event_data, event_id, out, last_event_id) {
                    saw_done = true;
                }
            }
            Err(e) => {
                out.push_back(Err(TransportError::sse(format!(
                    "invalid UTF-8 in SSE frame: {e}"
                ))));
            }
        }
    }
    saw_done
}

/// Flush any trailing bytes (without a final `\n`) as one last line, then
/// — since end-of-stream implies an event boundary — dispatch any
/// accumulated event.
fn drain_remaining<T: DeserializeOwned + 'static>(
    buf: &mut BytesMut,
    event_kind: &mut Option<String>,
    event_data: &mut String,
    event_id: &mut Option<String>,
    out: &mut VecDeque<Result<T, TransportError>>,
    last_event_id: &LastEventId,
) {
    if !buf.is_empty() {
        match std::str::from_utf8(buf) {
            Ok(s) => {
                let owned = s.to_owned();
                process_line(&owned, event_kind, event_data, event_id, out, last_event_id);
            }
            Err(e) => {
                out.push_back(Err(TransportError::sse(format!(
                    "invalid UTF-8 in SSE frame: {e}"
                ))));
            }
        }
        buf.clear();
    }
    // EOS is an implicit event boundary for any partially accumulated event.
    if !event_data.is_empty() || event_kind.is_some() {
        dispatch_event(event_kind, event_data, event_id, out, last_event_id);
    }
}

/// Process a single (trailing-CR/LF-stripped) SSE line. Returns `true` iff
/// the line caused a `done` event to be dispatched.
fn process_line<T: DeserializeOwned + 'static>(
    raw: &str,
    event_kind: &mut Option<String>,
    event_data: &mut String,
    event_id: &mut Option<String>,
    out: &mut VecDeque<Result<T, TransportError>>,
    last_event_id: &LastEventId,
) -> bool {
    let line = raw.trim_end_matches(['\r', '\n']);

    // Blank line — dispatch boundary.
    if line.is_empty() {
        // Per spec, suppress dispatch when no fields were set since the
        // last dispatch (e.g. stray blank lines / keepalives).
        if event_data.is_empty() && event_kind.is_none() {
            return false;
        }
        return dispatch_event(event_kind, event_data, event_id, out, last_event_id);
    }

    // Comment — ignore.
    if line.starts_with(':') {
        return false;
    }

    if let Some(value) = line.strip_prefix("event:") {
        *event_kind = Some(value.trim().to_owned());
        return false;
    }

    if let Some(value) = line.strip_prefix("data:") {
        let payload = value.strip_prefix(' ').unwrap_or(value);
        if !event_data.is_empty() {
            event_data.push('\n');
        }
        event_data.push_str(payload);
        return false;
    }

    // SSE `id:` field — capture per event; also propagate to the
    // connection-level last-event-id (per HTML5 EventSource spec). Empty
    // `id:` clears the saved value.
    if let Some(value) = line.strip_prefix("id:") {
        let id = value.trim().to_owned();
        if id.is_empty() {
            *event_id = None;
            last_event_id.set(None);
        } else {
            *event_id = Some(id.clone());
            last_event_id.set(Some(id));
        }
        return false;
    }

    // `retry:` and other unknown fields — ignore per spec.
    false
}

/// Drain the accumulated per-event state into `out`. Returns `true` iff
/// the dispatched event was a `done` sentinel.
fn dispatch_event<T: DeserializeOwned + 'static>(
    event_kind: &mut Option<String>,
    event_data: &mut String,
    event_id: &mut Option<String>,
    out: &mut VecDeque<Result<T, TransportError>>,
    _last_event_id: &LastEventId,
) -> bool {
    let kind = event_kind.take().unwrap_or_else(|| "message".to_owned());
    let payload = std::mem::take(event_data);
    // Per spec, last-event-id persists across events — do NOT clear
    // `event_id` here. The connection-level `LastEventId` cell was already
    // updated when the `id:` line was parsed.
    let _ = event_id;

    match kind.as_str() {
        "done" => true,
        "error" => {
            out.push_back(Err(parse_problem(&payload)));
            false
        }
        _ => {
            match serde_json::from_str::<T>(&payload) {
                Ok(v) => out.push_back(Ok(v)),
                Err(e) => out.push_back(Err(TransportError::serialization(e))),
            }
            false
        }
    }
}

fn parse_problem(payload: &str) -> TransportError {
    match serde_json::from_str::<Problem>(payload) {
        Ok(p) => TransportError::Problem(p),
        Err(e) => TransportError::sse(format!("malformed error event: {e}")),
    }
}

struct LineEnd {
    consumed: usize,
    line_len: usize,
}

fn find_line_end(buf: &[u8]) -> Option<LineEnd> {
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            return Some(LineEnd {
                consumed: i + 1,
                line_len: i,
            });
        }
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use futures_util::stream::{self, StreamExt};
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Item {
        id: u32,
    }

    fn chunks(parts: &[&str]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin + use<> {
        let owned: Vec<Result<Bytes, std::io::Error>> = parts
            .iter()
            .map(|s| Ok(Bytes::from(s.to_string())))
            .collect();
        Box::pin(stream::iter(owned))
    }

    #[tokio::test]
    async fn parses_data_events() {
        let s = chunks(&[
            "data: {\"id\":1}\n\n",
            "data: {\"id\":2}\n\n",
            "event: done\n\n",
        ]);
        let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
        let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(parsed, vec![Item { id: 1 }, Item { id: 2 }]);
    }

    #[tokio::test]
    async fn handles_data_split_across_chunks() {
        let s = chunks(&["data: {\"i", "d\":7}\n\nevent: done\n\n"]);
        let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
        let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(parsed, vec![Item { id: 7 }]);
    }

    #[tokio::test]
    async fn surfaces_error_event_as_problem() {
        // Canonical RFC 9457 Problem on the `event: error` channel.
        let problem = serde_json::json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "broke",
            "context": {}
        });
        let body = format!("event: error\ndata: {problem}\n\nevent: done\n\n");
        let s = chunks(&[&body]);
        let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
        assert_eq!(parsed.len(), 1);
        match parsed.into_iter().next().unwrap() {
            Err(TransportError::Problem(p)) => {
                assert_eq!(p.detail, "broke");
                assert!(p.problem_type.contains("internal"));
            }
            other => panic!("expected Problem, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ignores_comments_and_blank_lines() {
        let s = chunks(&[":heartbeat\n\ndata: {\"id\":3}\n\nevent: done\n\n"]);
        let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
        let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(parsed, vec![Item { id: 3 }]);
    }

    #[tokio::test]
    async fn malformed_json_yields_serialization_error() {
        let s = chunks(&["data: not-json\n\nevent: done\n\n"]);
        let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
        assert_eq!(parsed.len(), 1);
        match parsed.into_iter().next().unwrap() {
            Err(TransportError::Serialization(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn captures_id_field_for_reconnect() {
        let cell = LastEventId::empty();
        let s = chunks(&[
            "id: 42\ndata: {\"id\":1}\n\n",
            "id: 43\ndata: {\"id\":2}\n\n",
            "event: done\n\n",
        ]);
        let stream = parse_sse_stream_with_id::<Item, _, _>(s, cell.clone());
        let parsed: Vec<_> = stream.collect().await;
        let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(parsed, vec![Item { id: 1 }, Item { id: 2 }]);
        // After all events parsed, the cell holds the last seen id.
        assert_eq!(cell.current().as_deref(), Some("43"));
    }

    #[tokio::test]
    async fn joins_multiple_data_lines_with_newline() {
        // Two `data:` lines combine to form a single valid JSON object.
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Multi {
            text: String,
        }
        // Wire:
        //   data: {"text":
        //   data:  "hi"}
        //   <blank>
        // Joined payload: `{"text":\n "hi"}` — valid JSON.
        let body = "data: {\"text\":\ndata:  \"hi\"}\n\nevent: done\n\n";
        let s = chunks(&[body]);
        let parsed: Vec<_> = parse_sse_stream::<Multi, _, _>(s).collect().await;
        let parsed: Vec<Multi> = parsed.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(
            parsed,
            vec![Multi {
                text: "hi".to_owned()
            }]
        );
    }

    #[tokio::test]
    async fn empty_id_field_clears_saved_value() {
        // Per HTML5 EventSource spec, an empty `id:` resets the
        // Last-Event-ID to None (won't be sent on reconnect).
        let cell = LastEventId::empty();
        let s = chunks(&[
            "id: 7\ndata: {\"id\":1}\n\n",
            "id: \ndata: {\"id\":2}\n\n",
            "event: done\n\n",
        ]);
        let stream = parse_sse_stream_with_id::<Item, _, _>(s, cell.clone());
        let _: Vec<_> = stream.collect().await;
        assert!(cell.current().is_none());
    }
}
