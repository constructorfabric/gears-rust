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
//!
//! Accumulated per-line and per-event buffers are bounded by
//! [`MAX_ACCUMULATED_BYTES`] to protect against a peer that streams an
//! unbounded line (no terminating `\n`) or an unbounded run of `data:` lines
//! with no dispatching blank line — otherwise the buffer would grow without
//! limit for the lifetime of a self-healing, indefinitely-reconnecting client.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Maximum bytes the parser accumulates for a not-yet-terminated line ([`SseStream::buf`])
/// or for a not-yet-dispatched event's `data:` payload ([`SseStream::event_data`])
/// before treating the peer as protocol-violating and terminating the stream
/// with [`TransportError::sse`]. Generous enough for any realistic single
/// event; guards against unbounded memory growth from a misbehaving peer.
const MAX_ACCUMULATED_BYTES: usize = 16 * 1024 * 1024;

/// Shared monotonic counter bumped every time the parser receives a byte chunk
/// from the wire — **including** chunks that carry only keepalive comments or
/// other non-dispatching frames. Streaming clients snapshot it around an idle
/// wait so a low-data-rate stream kept alive purely by `:keepalive` comments is
/// recognised as *active* rather than *idle* (the idle timeout must be idle).
///
/// Wraps `Arc<AtomicU64>` as a newtype so the atomic choice isn't part of the
/// public surface.
#[derive(Clone, Debug, Default)]
pub struct SseActivity(Arc<AtomicU64>);

impl SseActivity {
    /// Create a fresh activity counter at generation 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current activity generation. Compare two snapshots to detect whether any
    /// wire chunk arrived in between.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Record that a wire chunk arrived.
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
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
        scan_from: 0,
        pending: VecDeque::new(),
        event_kind: None,
        event_data: String::new(),
        event_id: None,
        done: false,
        explicit_done: false,
        last_event_id,
        activity: SseActivity::new(),
        _marker: std::marker::PhantomData,
    }
}

/// Iterator yielded by [`parse_sse_stream`].
pub struct SseStream<T, S> {
    inner: S,
    buf: BytesMut,
    /// Prefix length of `buf`, in bytes, already confirmed to contain no
    /// unconsumed `\n`. Lets [`find_line_end`] resume scanning from here
    /// instead of rescanning the whole buffer on every poll — without this, a
    /// single long unterminated line delivered over many small chunks costs
    /// `O(total_length²)` instead of `O(total_length)`.
    scan_from: usize,
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
    /// `true` only when the stream ended because an `event: done` frame was
    /// actually dispatched — as opposed to `done` being set because the
    /// underlying byte stream simply closed (peer disconnect, proxy timeout,
    /// etc.). The streaming client uses this to tell "the peer said it's
    /// finished" apart from "the connection just ended", so it can treat the
    /// latter as reconnect-eligible instead of a silent success.
    explicit_done: bool,
    last_event_id: LastEventId,
    activity: SseActivity,
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

    /// `true` iff the stream ended because the peer explicitly dispatched an
    /// `event: done` frame. `false` if the stream ended for any other reason
    /// (byte stream closed, error) — including while still in progress, so
    /// callers should only consult this after the stream has yielded `None`.
    #[must_use]
    pub fn saw_done_event(&self) -> bool {
        self.explicit_done
    }

    /// Returns a clone of the shared wire-activity counter. The streaming client
    /// snapshots it around an idle-timeout wait so keepalive-only traffic keeps
    /// the stream alive (see [`SseActivity`]).
    #[must_use]
    pub fn activity_handle(&self) -> SseActivity {
        self.activity.clone()
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
                    // Any wire chunk counts as activity — even one carrying only
                    // keepalive comments that dispatch no item — so the idle
                    // timeout in the streaming driver can tell "quiet but alive"
                    // from "truly idle".
                    this.activity.bump();
                    this.buf.extend_from_slice(&chunk);
                    let saw_done = drain_buffer(
                        &mut this.buf,
                        &mut this.scan_from,
                        &mut this.event_kind,
                        &mut this.event_data,
                        &mut this.event_id,
                        &mut this.pending,
                        &this.last_event_id,
                    );
                    if saw_done {
                        this.done = true;
                        this.explicit_done = true;
                    }
                    // Guard against unbounded memory growth: a peer streaming a
                    // line with no terminating `\n`, or a run of `data:` lines
                    // with no dispatching blank line, would otherwise grow
                    // `buf`/`event_data` without limit for the life of a
                    // self-healing, indefinitely-reconnecting client.
                    if this.buf.len() > MAX_ACCUMULATED_BYTES
                        || this.event_data.len() > MAX_ACCUMULATED_BYTES
                    {
                        this.done = true;
                        return Poll::Ready(Some(Err(TransportError::sse(
                            "SSE frame exceeds maximum accumulated size; aborting stream",
                        ))));
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
    scan_from: &mut usize,
    event_kind: &mut Option<String>,
    event_data: &mut String,
    event_id: &mut Option<String>,
    out: &mut VecDeque<Result<T, TransportError>>,
    last_event_id: &LastEventId,
) -> bool {
    let mut saw_done = false;
    while let Some(line_end) = find_line_end(buf, *scan_from) {
        let line_bytes = buf.split_to(line_end.consumed);
        // Bytes after the found `\n` were never examined by this call (the
        // scan stops at the first match) — resume from scratch for them.
        *scan_from = 0;
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
    // Nothing left to find: everything currently in `buf` is confirmed
    // newline-free. Remember that so the next poll's `extend_from_slice` only
    // needs `find_line_end` to scan the newly appended tail.
    *scan_from = buf.len();
    saw_done
}

/// Flush any trailing bytes (without a final `\n`) as one last line, then
/// — since end-of-stream implies an event boundary — dispatch any
/// accumulated event.
fn drain_remaining<T: DeserializeOwned + 'static>(
    buf: &mut BytesMut,
    event_kind: &mut Option<String>,
    event_data: &mut String,
    _event_id: &mut Option<String>,
    _out: &mut VecDeque<Result<T, TransportError>>,
    _last_event_id: &LastEventId,
) {
    // Per the W3C EventSource spec, an event that is not terminated by a blank
    // line before end-of-stream is INCOMPLETE and MUST be discarded. Forcing a
    // dispatch here would surface a truncated final frame as a (non-transient)
    // `Serialization` error — both a spec violation and a defeat of reconnect.
    // A properly framed final event was already dispatched on its blank line,
    // so anything left in the buffers is a partial event: drop it.
    buf.clear();
    event_kind.take();
    event_data.clear();
    // Note: `scan_from` is intentionally left untouched here — the stream is
    // marked `done` by the caller right after this returns, so it is never
    // consulted again.
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
        // The implicit default channel carries the typed payload. An event with
        // no `event:` field defaults to `"message"` here.
        "message" => {
            match serde_json::from_str::<T>(&payload) {
                Ok(v) => out.push_back(Ok(v)),
                Err(e) => out.push_back(Err(TransportError::serialization(e))),
            }
            false
        }
        // Named control events (`heartbeat`, `ping`, custom kinds) are not the
        // typed data channel — ignore them rather than trying to decode `T`
        // (which would yield spurious items or serialization errors).
        _ => false,
    }
}

fn parse_problem(payload: &str) -> TransportError {
    match serde_json::from_str::<Problem>(payload) {
        Ok(p) => TransportError::problem(p),
        Err(e) => TransportError::sse(format!("malformed error event: {e}")),
    }
}

struct LineEnd {
    consumed: usize,
    line_len: usize,
}

/// Scan for the next `\n`, starting from byte offset `start` (bytes before
/// `start` are assumed already confirmed newline-free by the caller — see
/// [`SseStream::scan_from`]).
fn find_line_end(buf: &[u8], start: usize) -> Option<LineEnd> {
    for (i, b) in buf[start..].iter().enumerate() {
        if *b == b'\n' {
            let abs = start + i;
            return Some(LineEnd {
                consumed: abs + 1,
                line_len: abs,
            });
        }
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
#[path = "sse_tests.rs"]
mod tests;
