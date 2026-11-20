use std::collections::VecDeque;
use std::pin::Pin;

use futures_core::Stream;
use futures_util::StreamExt;

use crate::body::BodyStream;
use crate::error::StreamingError;
use crate::sse::ServerEvent;

/// Default bound on one SSE event, in bytes: its lines with their line
/// endings, as buffered until the blank line that dispatches it. The same as
/// the api-gateway's default request body limit, so a base64 image fits.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// A buffer this large or smaller keeps its capacity between events.
const KEPT_BUF_CAPACITY: usize = 64 * 1024;

struct ParseState {
    body: BodyStream,
    /// Text since the last event boundary: the partial event.
    buf: String,
    /// Length of `buf` already searched for an event boundary. The next
    /// search starts one byte before it, at the LF a boundary may begin with.
    scanned: usize,
    max_event_bytes: usize,
    /// Events parsed from the current buffer but not yet yielded.
    pending: VecDeque<ServerEvent>,
    /// Trailing bytes from the previous chunk that form an incomplete UTF-8 sequence.
    /// Prepended to the next chunk before decoding.
    utf8_tail: Vec<u8>,
    /// Whether this is the first chunk (for BOM stripping).
    first_chunk: bool,
    /// The previous chunk ended in CR: an LF opening the next chunk belongs
    /// to that CRLF.
    last_was_cr: bool,
    /// Yielded once `pending` is empty; ends the stream.
    error: Option<StreamingError>,
    done: bool,
}

/// Parse a field line within an SSE event block.
///
/// Malformed lines are silently skipped (per W3C spec).
fn parse_line(line: &str, event: &mut ServerEvent) {
    // Comment lines start with ':'
    if line.starts_with(':') {
        return;
    }

    let (field, value) = match line.find(':') {
        Some(pos) => {
            let f = &line[..pos];
            let v = &line[pos + 1..];
            // Strip single leading space from value if present.
            let v = v.strip_prefix(' ').unwrap_or(v);
            (f, v)
        }
        // Line with no colon — field name is the entire line, value is empty.
        None => (line, ""),
    };

    match field {
        "data" => {
            if !event.data.is_empty() {
                event.data.push('\n');
            }
            event.data.push_str(value);
        }
        "event" => {
            event.event = Some(value.to_owned());
        }
        "id" => {
            // Per spec, id must not contain null.
            if !value.contains('\0') {
                event.id = Some(value.to_owned());
            }
        }
        "retry" => {
            if let Ok(ms) = value.parse::<u64>() {
                event.retry = Some(ms);
            }
        }
        _ => {
            // Unknown field — ignore per spec.
            tracing::trace!("ignoring unknown SSE field: {field}");
        }
    }
}

/// Normalize CRLF (`\r\n`) and bare CR (`\r`) to LF (`\n`).
///
/// The W3C EventSource specification requires support for all three line
/// ending styles. We normalize once at buffer-append time so the rest of
/// the parser can work exclusively with `\n`.
///
/// A CR ending one chunk is already an LF in the buffer, so an LF opening the
/// next chunk is dropped: a CRLF split across chunks is one line ending, not
/// a blank line that would dispatch the event early.
fn normalize_line_endings(s: &str, last_was_cr: &mut bool) -> String {
    let s = if *last_was_cr {
        s.strip_prefix('\n').unwrap_or(s)
    } else {
        s
    };
    *last_was_cr = s.ends_with('\r');
    // Replace CRLF first, then any remaining bare CR.
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split buffered text on event boundaries (`\n\n`), appending completed
/// events to `events` and leaving any partial trailing data in the buffer.
///
/// `scanned` is the length of `buf` that holds no boundary: the search
/// resumes there, so an event arriving in many chunks is scanned once.
///
/// Fails on an event over `max_event_bytes`; the events before it are kept.
fn extract_events(
    buf: &mut String,
    scanned: &mut usize,
    max_event_bytes: usize,
    events: &mut VecDeque<ServerEvent>,
) -> Result<(), StreamingError> {
    // Drained once at the end: draining per event would copy the rest of
    // the buffer for every event.
    let mut start = 0;
    let mut from = scanned.saturating_sub(1);
    let result = loop {
        // Bytes, not str: `from` may fall inside a multibyte character.
        let Some(pos) = find_boundary(&buf.as_bytes()[from..]).map(|p| from + p - start) else {
            break Ok(());
        };

        let block = &buf[start..start + pos];
        // + 1 for the LF ending its last line, so the event counts the same
        // as it did while still partial in the buffer.
        if block.len() + 1 > max_event_bytes {
            break Err(StreamingError::EventTooLarge {
                limit: max_event_bytes,
            });
        }
        if !block.is_empty() {
            let mut event = ServerEvent::default();
            for line in block.lines() {
                parse_line(line, &mut event);
            }
            if !event.is_empty() {
                events.push_back(event);
            }
        }

        // Skip the block, the two newlines and any further newlines.
        start += pos + 2;
        start = buf.len() - buf[start..].trim_start_matches('\n').len();
        from = start;
    };
    buf.drain(..start);
    // `drain` keeps the capacity: after a large event, give it back rather
    // than hold it for the rest of the stream.
    if buf.capacity() > KEPT_BUF_CAPACITY && buf.len() < buf.capacity() / 4 {
        buf.shrink_to(buf.len().max(KEPT_BUF_CAPACITY));
    }
    *scanned = buf.len();
    result
}

fn find_boundary(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|w| w == b"\n\n")
}

/// Parse a raw byte stream into a stream of SSE events.
///
/// Chunks are buffered internally and split on blank-line boundaries (`\n\n`).
/// Malformed lines within an event are silently skipped (per W3C EventSource spec).
/// Empty events (comment-only blocks) are not yielded, nor is an event the
/// body ends before its blank line, at a clean end too (W3C). Invalid UTF-8
/// ends the stream with
/// [`StreamingError::ServerEventsParse`].
/// An event over `max_event_bytes` ends the stream with
/// [`StreamingError::EventTooLarge`].
#[allow(clippy::type_complexity)]
pub fn parse_server_events_stream(
    body: BodyStream,
    max_event_bytes: usize,
) -> Pin<Box<dyn Stream<Item = Result<ServerEvent, StreamingError>> + Send>> {
    let state = ParseState {
        body,
        buf: String::new(),
        scanned: 0,
        max_event_bytes,
        pending: VecDeque::new(),
        utf8_tail: Vec::new(),
        first_chunk: true,
        last_was_cr: false,
        error: None,
        done: false,
    };

    Box::pin(futures_util::stream::unfold(
        state,
        |mut state| async move {
            loop {
                // If we have pending events from a previous chunk, yield them first.
                if let Some(event) = state.pending.pop_front() {
                    return Some((Ok(event), state));
                }

                if let Some(err) = state.error.take() {
                    // Free the buffer and release the upstream now, not
                    // when the caller drops the stream.
                    state.buf = String::new();
                    state.utf8_tail = Vec::new();
                    state.body = Box::pin(futures_util::stream::empty());
                    state.done = true;
                    return Some((Err(err), state));
                }

                if state.done {
                    return None;
                }

                // Read the next chunk from the body stream.
                match state.body.next().await {
                    Some(Ok(chunk)) => {
                        // Prepend any leftover bytes from a split multibyte sequence.
                        let bytes = if state.utf8_tail.is_empty() {
                            chunk.to_vec()
                        } else {
                            let mut combined = std::mem::take(&mut state.utf8_tail);
                            combined.extend_from_slice(&chunk);
                            combined
                        };

                        let text = match std::str::from_utf8(&bytes) {
                            Ok(t) => t.to_owned(),
                            Err(e) if e.error_len().is_none() => {
                                // Incomplete multibyte sequence at the end — buffer
                                // the trailing bytes and decode the valid prefix.
                                let valid_up_to = e.valid_up_to();
                                state.utf8_tail = bytes[valid_up_to..].to_vec();
                                // Safety: valid_up_to is guaranteed to be valid UTF-8.
                                String::from_utf8(bytes[..valid_up_to].to_vec()).unwrap()
                            }
                            Err(e) => {
                                // Invalid UTF-8 ends the stream: the events
                                // around it can no longer be trusted.
                                state.error = Some(StreamingError::ServerEventsParse {
                                    detail: format!("invalid UTF-8: {e}"),
                                });
                                continue;
                            }
                        };

                        if !text.is_empty() {
                            // Strip UTF-8 BOM from the very first chunk (per W3C spec).
                            let text = if state.first_chunk {
                                state.first_chunk = false;
                                text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_owned()
                            } else {
                                text
                            };
                            state
                                .buf
                                .push_str(&normalize_line_endings(&text, &mut state.last_was_cr));
                            if let Err(e) = extract_events(
                                &mut state.buf,
                                &mut state.scanned,
                                state.max_event_bytes,
                                &mut state.pending,
                            ) {
                                state.error = Some(e);
                                continue;
                            }
                        }
                        // The partial event, with the bytes of a character
                        // split across chunks, is bounded too: without a
                        // blank line it would grow with every chunk.
                        if state.buf.len() + state.utf8_tail.len() > state.max_event_bytes {
                            state.error = Some(StreamingError::EventTooLarge {
                                limit: state.max_event_bytes,
                            });
                        }
                        // Loop back to yield pending events.
                    }
                    Some(Err(e)) => {
                        // The unterminated tail is part of a broken body:
                        // nothing may follow the error as an event.
                        state.buf.clear();
                        state.pending.clear();
                        state.done = true;
                        return Some((Err(StreamingError::from(e)), state));
                    }
                    None => {
                        // An event without its blank line is discarded (W3C):
                        // a body delimited by connection close looks the same
                        // when the connection drops mid-event.
                        state.buf = String::new();
                        state.done = true;
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BoxError;
    use bytes::Bytes;
    use futures_util::{FutureExt, StreamExt};

    /// The parser with the default event size bound.
    #[allow(clippy::type_complexity)]
    fn parse_server_events_stream(
        body: BodyStream,
    ) -> Pin<Box<dyn Stream<Item = Result<ServerEvent, StreamingError>> + Send>> {
        super::parse_server_events_stream(body, DEFAULT_MAX_EVENT_BYTES)
    }

    async fn parse_with_limit(
        chunks: Vec<Vec<u8>>,
        limit: usize,
    ) -> Vec<Result<ServerEvent, StreamingError>> {
        let owned: Vec<Result<Bytes, BoxError>> =
            chunks.into_iter().map(|c| Ok(Bytes::from(c))).collect();
        super::parse_server_events_stream(Box::pin(futures_util::stream::iter(owned)), limit)
            .collect()
            .await
    }

    fn is_too_large(item: &Result<ServerEvent, StreamingError>, limit: usize) -> bool {
        matches!(item, Err(StreamingError::EventTooLarge { limit: l }) if *l == limit)
    }

    /// A body that never sends a line break ends with `EventTooLarge` once
    /// the bound is crossed, and the parser stops reading it.
    #[tokio::test]
    async fn a_body_without_line_breaks_ends_with_event_too_large() {
        use crate::sse::{ServerEventsResponse, ServerEventsStream};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&pulled);
        let body: BodyStream = Box::pin(
            futures_util::stream::repeat_with(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(Bytes::from_static(b"xxxxxxxx"))
            })
            .take(1000),
        );
        let resp = http::Response::builder()
            .header(http::header::CONTENT_TYPE, "text/event-stream")
            .body(crate::body::Body::from(body))
            .unwrap();
        let ServerEventsResponse::Events(mut events) =
            ServerEventsStream::from_response_with_max_event_bytes::<ServerEvent>(resp, 64)
        else {
            panic!("expected an SSE stream");
        };

        assert!(matches!(
            events.next().await,
            Some(Err(StreamingError::EventTooLarge { limit: 64 }))
        ));
        assert!(events.next().await.is_none());
        // 64 bytes fit in 8 chunks; the 9th crosses the bound.
        assert_eq!(pulled.load(Ordering::Relaxed), 9);
    }

    /// `from_response` bounds an event at [`DEFAULT_MAX_EVENT_BYTES`]: the
    /// default every SDK reader gets keeps the buffer bounded.
    #[tokio::test]
    async fn from_response_bounds_an_event_at_the_default() {
        use crate::sse::{ServerEventsResponse, ServerEventsStream};

        const CHUNK: usize = 64 * 1024;
        let chunks = DEFAULT_MAX_EVENT_BYTES / CHUNK + 1;
        let body: BodyStream = Box::pin(
            futures_util::stream::repeat_with(|| Ok(Bytes::from(vec![b'x'; CHUNK]))).take(chunks),
        );
        let resp = http::Response::builder()
            .header(http::header::CONTENT_TYPE, "text/event-stream")
            .body(crate::body::Body::from(body))
            .unwrap();
        let ServerEventsResponse::Events(mut events) =
            ServerEventsStream::from_response::<ServerEvent>(resp)
        else {
            panic!("expected an SSE stream");
        };

        assert!(matches!(
            events.next().await,
            Some(Err(StreamingError::EventTooLarge {
                limit: DEFAULT_MAX_EVENT_BYTES
            }))
        ));
        assert!(events.next().await.is_none());
    }

    /// The buffer gives back the memory of a large event once it is
    /// dispatched, instead of holding it for the rest of the stream.
    #[test]
    fn the_buffer_shrinks_after_a_large_event() {
        let mut buf = format!("data: {}\n\ndata: small", "x".repeat(1024 * 1024));
        let mut scanned = 0;
        let mut events = VecDeque::new();
        extract_events(&mut buf, &mut scanned, DEFAULT_MAX_EVENT_BYTES, &mut events).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(buf, "data: small");
        assert!(buf.capacity() <= KEPT_BUF_CAPACITY, "{}", buf.capacity());
    }

    /// An event of exactly the bound passes and one byte more fails, wherever
    /// the transport splits it, a multibyte character included.
    #[tokio::test]
    async fn an_event_at_the_bound_passes_and_one_byte_more_fails() {
        let stream = "data: a\u{20ac}b\n\n".as_bytes();
        // The event's line with its line ending, without the blank line.
        let size = stream.len() - 1;
        let expected = ServerEvent {
            data: "a\u{20ac}b".into(),
            ..ServerEvent::default()
        };

        let mut splits: Vec<Vec<Vec<u8>>> = (0..=stream.len())
            .map(|at| vec![stream[..at].to_vec(), stream[at..].to_vec()])
            .collect();
        splits.push(stream.iter().map(|b| vec![*b]).collect());
        for chunks in splits {
            let at_bound = parse_with_limit(chunks.clone(), size).await;
            assert!(
                matches!(&at_bound[..], [Ok(e)] if *e == expected),
                "{chunks:?}: {at_bound:?}"
            );
            let over = parse_with_limit(chunks.clone(), size - 1).await;
            assert!(
                matches!(&over[..], [e] if is_too_large(e, size - 1)),
                "{chunks:?}: {over:?}"
            );
        }
    }

    /// Bytes of a character split across chunks count toward the bound, so
    /// a body that stalls after them still ends with the error.
    #[tokio::test]
    async fn a_split_character_counts_toward_the_bound() {
        let euro = "\u{20ac}".as_bytes();
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from_static(b"data: a")),
            Ok(Bytes::copy_from_slice(&euro[..2])),
        ];
        let body = futures_util::stream::iter(items).chain(futures_util::stream::pending());
        // "data: a" is 7 bytes; with the 2 held bytes the event is over 8.
        let mut events = super::parse_server_events_stream(Box::pin(body), 8);
        let first = events.next().now_or_never();
        assert!(
            matches!(&first, Some(Some(e)) if is_too_large(e, 8)),
            "{first:?}"
        );
    }

    /// The count spans chunks: chunks each below the bound add up over it.
    /// Complete events before the oversized one are still yielded, and
    /// nothing after it.
    #[tokio::test]
    async fn the_bound_spans_chunks() {
        let line = b"data: aaaaaaaa\n".to_vec();
        assert!(line.len() < 32);
        let chunks = vec![
            b"data: first\n\ndata: aaaaaaaa\n".to_vec(),
            line.clone(),
            line.clone(),
            b"\ndata: after\n\n".to_vec(),
        ];
        let items = parse_with_limit(chunks, 32).await;
        assert!(
            matches!(&items[..], [Ok(e), err] if e.data == "first" && is_too_large(err, 32)),
            "{items:?}"
        );

        // The same with the oversized event complete in one chunk.
        let mut chunk = b"data: first\n\n".to_vec();
        chunk.extend_from_slice(&line.repeat(3));
        chunk.extend_from_slice(b"\ndata: after\n\n");
        let items = parse_with_limit(vec![chunk], 32).await;
        assert!(
            matches!(&items[..], [Ok(e), err] if e.data == "first" && is_too_large(err, 32)),
            "{items:?}"
        );
    }

    /// The count resets after each event: many events below the bound pass
    /// although together they are far over it.
    #[tokio::test]
    async fn the_bound_resets_after_each_event() {
        let event = b"data: 0123456789abcdef\n\n";
        assert!(event.len() < 32);
        let stream = event.repeat(100);

        let whole = parse_with_limit(vec![stream.clone()], 32).await;
        let halves = parse_with_limit(
            stream.chunks(event.len() / 2).map(<[u8]>::to_vec).collect(),
            32,
        )
        .await;
        for items in [whole, halves] {
            assert_eq!(items.len(), 100);
            assert!(
                items
                    .iter()
                    .all(|r| matches!(r, Ok(e) if e.data == "0123456789abcdef")),
                "{items:?}"
            );
        }
    }

    /// Helper: create a BodyStream from a list of string chunks.
    fn body_from_chunks(chunks: Vec<&str>) -> BodyStream {
        let owned: Vec<Result<Bytes, BoxError>> = chunks
            .into_iter()
            .map(|s| Ok(Bytes::from(s.to_owned())))
            .collect();
        Box::pin(futures_util::stream::iter(owned))
    }

    /// A stream the gateway cut ends with `ClosedByGateway` for SDK readers.
    #[tokio::test]
    async fn a_stream_cut_by_the_gateway_ends_with_closed_by_gateway() {
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from("data: one\n\n")),
            Err(Box::new(crate::StreamClosedByGateway::new())),
        ];
        let mut events = parse_server_events_stream(Box::pin(futures_util::stream::iter(items)));
        assert_eq!(events.next().await.unwrap().unwrap().data, "one");
        assert!(matches!(
            events.next().await,
            Some(Err(StreamingError::ClosedByGateway))
        ));
    }

    /// A stream cut on the gateway's idle timeout ends with `IdleTimeout`,
    /// not as a complete stream.
    #[tokio::test]
    async fn a_stream_cut_on_idle_ends_with_idle_timeout() {
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from("data: one\n\n")),
            Err(Box::new(crate::StreamIdleTimeout::new())),
        ];
        let mut events = parse_server_events_stream(Box::pin(futures_util::stream::iter(items)));
        assert_eq!(events.next().await.unwrap().unwrap().data, "one");
        assert!(matches!(
            events.next().await,
            Some(Err(StreamingError::IdleTimeout))
        ));
        assert!(events.next().await.is_none());
    }

    /// An unterminated event before a body error is not yielded after the
    /// error: the stream ends with the error alone.
    #[tokio::test]
    async fn no_event_follows_a_body_error() {
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from("data: partial")),
            Err("upstream failed mid-body".into()),
        ];
        let mut events = parse_server_events_stream(Box::pin(futures_util::stream::iter(items)));
        assert!(
            matches!(events.next().await, Some(Err(StreamingError::Stream(_)))),
            "expected the stream error first"
        );
        assert!(events.next().await.is_none(), "an event followed the error");
    }

    #[tokio::test]
    async fn parse_single_event() {
        let body = body_from_chunks(vec!["data: hello world\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello world");
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].id, None);
    }

    #[tokio::test]
    async fn parse_multiple_events() {
        let body = body_from_chunks(vec!["data: first\n\ndata: second\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[tokio::test]
    async fn parse_multi_chunk_event() {
        // Event split across two chunks.
        let body = body_from_chunks(vec!["data: hel", "lo\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[tokio::test]
    async fn parse_all_fields() {
        let body = body_from_chunks(vec![
            "id: 42\nevent: update\nretry: 3000\ndata: payload\n\n",
        ]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].event.as_deref(), Some("update"));
        assert_eq!(events[0].retry, Some(3000));
        assert_eq!(events[0].data, "payload");
    }

    #[tokio::test]
    async fn parse_multiline_data() {
        let body = body_from_chunks(vec!["data: line1\ndata: line2\ndata: line3\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2\nline3");
    }

    #[tokio::test]
    async fn skip_comments() {
        let body = body_from_chunks(vec![": this is a comment\ndata: real data\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real data");
    }

    /// An event the body ends before its blank line is discarded (W3C): it
    /// may be cut short.
    #[tokio::test]
    async fn an_event_without_its_blank_line_at_eof_is_discarded() {
        for tail in ["data: {\"partial", "data: trailing\n"] {
            let body = body_from_chunks(vec!["data: one\n\n", tail]);
            let events: Vec<_> = parse_server_events_stream(body)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .map(|r| r.unwrap().data)
                .collect();
            assert_eq!(events, ["one"], "tail {tail:?}");
        }
    }

    #[tokio::test]
    async fn ignore_unknown_fields() {
        let body = body_from_chunks(vec!["foo: bar\ndata: value\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "value");
    }

    #[tokio::test]
    async fn empty_stream() {
        let body = body_from_chunks(vec![]);
        let events: Vec<_> = parse_server_events_stream(body).collect::<Vec<_>>().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn parse_crlf_line_endings() {
        let body = body_from_chunks(vec!["data: hello\r\n\r\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[tokio::test]
    async fn parse_bare_cr_line_endings() {
        let body = body_from_chunks(vec!["data: hello\r\r"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[tokio::test]
    async fn parse_mixed_line_endings() {
        // Mix CRLF and LF in the same stream.
        let body = body_from_chunks(vec!["data: first\r\n\r\ndata: second\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[tokio::test]
    async fn parse_multibyte_utf8_split_across_chunks() {
        // Euro sign € is 3 bytes: 0xE2 0x82 0xAC
        // Split it across two chunks.
        let euro = "€";
        let euro_bytes = euro.as_bytes(); // [0xE2, 0x82, 0xAC]
        assert_eq!(euro_bytes.len(), 3);

        let mut chunk1 = b"data: price ".to_vec();
        chunk1.push(euro_bytes[0]); // incomplete: 0xE2

        let mut chunk2 = vec![euro_bytes[1], euro_bytes[2]]; // 0x82 0xAC
        chunk2.extend_from_slice(b"99\n\n");

        let owned: Vec<Result<Bytes, crate::body::BoxError>> =
            vec![Ok(Bytes::from(chunk1)), Ok(Bytes::from(chunk2))];
        let body: BodyStream = Box::pin(futures_util::stream::iter(owned));

        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "price €99");
    }

    #[tokio::test]
    async fn parse_truly_invalid_utf8() {
        // 0xFF is never valid in UTF-8.
        let owned: Vec<Result<Bytes, crate::body::BoxError>> =
            vec![Ok(Bytes::from(vec![0xFF, 0xFE]))];
        let body: BodyStream = Box::pin(futures_util::stream::iter(owned));

        let events: Vec<_> = parse_server_events_stream(body).collect::<Vec<_>>().await;

        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
    }

    /// Invalid UTF-8 ends the stream: no event follows the error.
    #[tokio::test]
    async fn invalid_utf8_ends_the_stream() {
        let items = parse_with_limit(
            vec![
                b"data: a\n".to_vec(),
                b"x\xff\n".to_vec(),
                b"data: b\n\n".to_vec(),
            ],
            DEFAULT_MAX_EVENT_BYTES,
        )
        .await;
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(matches!(
            items[0],
            Err(StreamingError::ServerEventsParse { .. })
        ));
    }

    /// An event arriving in many chunks is found once its blank line comes,
    /// including a blank line split across chunks after a multibyte
    /// character.
    #[tokio::test]
    async fn an_event_in_many_chunks_is_found_at_its_blank_line() {
        let mut chunks: Vec<Vec<u8>> = vec![b"data: ".to_vec()];
        chunks.extend((0..1000).map(|_| "\u{20ac}".as_bytes().to_vec()));
        chunks.push(b"\n".to_vec());
        chunks.push(b"\ndata: next\n\n".to_vec());
        let events = parse_byte_chunks(chunks).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "\u{20ac}".repeat(1000));
        assert_eq!(events[1].data, "next");
    }

    // -- W3C spec: value space stripping -----------------------------------

    #[tokio::test]
    async fn data_no_space_after_colon() {
        // "data:hello" — no space to strip.
        let body = body_from_chunks(vec!["data:hello\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events[0].data, "hello");
    }

    #[tokio::test]
    async fn data_double_space_after_colon() {
        // "data:  hello" — one space stripped, one preserved.
        let body = body_from_chunks(vec!["data:  hello\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events[0].data, " hello");
    }

    #[tokio::test]
    async fn field_name_without_colon() {
        // Bare "data" line (no colon) — field name is "data", value is "".
        // Empty pushes are no-ops; only the non-empty "real" contributes.
        let body = body_from_chunks(vec!["data\ndata\ndata: real\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real");
    }

    #[tokio::test]
    async fn empty_data_value() {
        // "data:" with no value after colon — empty string appended to data buffer.
        // First empty push is a no-op; second line appends "hello".
        let body = body_from_chunks(vec!["data:\ndata: hello\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    // -- W3C spec: id field with null byte ---------------------------------

    #[tokio::test]
    async fn id_with_null_byte_ignored() {
        // Per spec: if the id value contains U+0000 NULL, ignore the field.
        let body = body_from_chunks(vec!["id: a\0b\ndata: test\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, None);
        assert_eq!(events[0].data, "test");
    }

    // -- W3C spec: retry field validation ----------------------------------

    #[tokio::test]
    async fn retry_non_numeric_ignored() {
        // Non-numeric retry value is silently ignored.
        let body = body_from_chunks(vec!["retry:1000x\ndata: test\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events[0].retry, None);
        assert_eq!(events[0].data, "test");
    }

    #[tokio::test]
    async fn retry_empty_ignored() {
        let body = body_from_chunks(vec!["retry:\ndata: test\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events[0].retry, None);
    }

    // -- W3C spec: comment-only block → no event ---------------------------

    #[tokio::test]
    async fn comment_only_block_no_event() {
        // A block with only comments should not dispatch an event.
        let body = body_from_chunks(vec![": comment\n\ndata: real\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real");
    }

    // -- W3C spec: extra blank lines between events are harmless -----------

    #[tokio::test]
    async fn extra_blank_lines_between_events() {
        let body = body_from_chunks(vec!["data: first\n\n\n\n\ndata: second\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    // -- W3C spec: case-sensitive field names ------------------------------

    #[tokio::test]
    async fn field_names_are_case_sensitive() {
        // "Data" (capital D) is not "data" — ignored per spec.
        let body = body_from_chunks(vec!["Data: ignored\ndata: kept\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "kept");
    }

    // -- W3C spec: no-data block does not dispatch -------------------------

    #[tokio::test]
    async fn metadata_only_block_yields_event() {
        // Block with id/event/retry but no data — SDK yields it (consumers decide).
        let body = body_from_chunks(vec!["id: 1\nevent: ping\nretry: 5000\n\ndata: real\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id.as_deref(), Some("1"));
        assert_eq!(events[0].event.as_deref(), Some("ping"));
        assert_eq!(events[0].retry, Some(5000));
        assert_eq!(events[0].data, "");
        assert_eq!(events[1].data, "real");
    }

    // -- W3C spec: BOM at stream start stripped ----------------------------

    #[tokio::test]
    async fn bom_at_stream_start_stripped() {
        // UTF-8 BOM (0xEF 0xBB 0xBF) at the very beginning should be stripped.
        let body_bytes: Vec<Result<Bytes, BoxError>> =
            vec![Ok(Bytes::from(b"\xEF\xBB\xBFdata: hello\n\n".to_vec()))];
        let body: BodyStream = Box::pin(futures_util::stream::iter(body_bytes));

        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    // -- Chunked delivery edge cases --------------------------------------

    #[tokio::test]
    async fn event_boundary_split_across_chunks() {
        // The \n\n boundary is split: first \n in chunk 1, second \n in chunk 2.
        let body = body_from_chunks(vec!["data: hello\n", "\ndata: world\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }

    #[tokio::test]
    async fn four_byte_emoji_split_across_chunks() {
        // Thumbs up 👍 is 4 bytes: F0 9F 91 8D — split 2+2.
        let emoji = "👍";
        let b = emoji.as_bytes();
        assert_eq!(b.len(), 4);

        let mut chunk1 = b"data: ".to_vec();
        chunk1.extend_from_slice(&b[..2]);

        let mut chunk2 = b[2..].to_vec();
        chunk2.extend_from_slice(b"\n\n");

        let owned: Vec<Result<Bytes, BoxError>> =
            vec![Ok(Bytes::from(chunk1)), Ok(Bytes::from(chunk2))];
        let body: BodyStream = Box::pin(futures_util::stream::iter(owned));

        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "👍");
    }

    #[tokio::test]
    async fn multiple_events_split_across_chunks() {
        let body = body_from_chunks(vec!["data: hel", "lo\n\ndata:", " world\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }

    // -- W3C format-field-parsing composite test ---------------------------

    #[tokio::test]
    async fn w3c_format_field_parsing() {
        // Adapted from the W3C EventSource spec test suite.
        // Tests multiple field parsing rules in a single stream.
        let body = body_from_chunks(vec![
            "data:\0\n",  // null byte in data → value is "\0"
            "data:  2\n", // double space → value is " 2"
            "Data:1\n",   // capital D → unknown, ignored
            "data:1\n",   // normal
            "da-ta:3\n",  // hyphenated field → unknown, ignored
            "data:3\n",   // normal
            "data:\n",    // empty value → ""
            "data:4\n\n", // normal, then dispatch
        ]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "\0\n 2\n1\n3\n\n4");
    }

    // -- Line endings split across chunks -----------------------------------

    #[tokio::test]
    async fn crlf_split_across_chunks_is_one_line_ending() {
        // CR ends the first chunk, LF opens the second: one line ending, so
        // both data lines belong to the same event.
        let body = body_from_chunks(vec!["data: a\r", "\ndata: b\r\n\r\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a\nb");
    }

    #[tokio::test]
    async fn bare_cr_then_lf_chunk_still_ends_the_event() {
        // "\r" + "\n\r\n": the LF completes the first CRLF, the second CRLF
        // is the blank line that dispatches.
        let body = body_from_chunks(vec!["data: a\r", "\n\r\n", "data: b\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "a");
        assert_eq!(events[1].data, "b");
    }

    async fn parse_byte_chunks(chunks: Vec<Vec<u8>>) -> Vec<ServerEvent> {
        let owned: Vec<Result<Bytes, BoxError>> =
            chunks.into_iter().map(|c| Ok(Bytes::from(c))).collect();
        parse_server_events_stream(Box::pin(futures_util::stream::iter(owned)))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect()
    }

    /// The events do not depend on where the transport splits the bytes:
    /// CRLF, bare CR, LF and a multibyte character, cut at every offset and
    /// byte by byte.
    #[tokio::test]
    async fn every_chunk_boundary_yields_the_same_events() {
        let stream = "id: 1\r\nevent: tick\r\ndata: a\r\ndata: \u{20ac}\r\n\r\n\
                      : note\r\ndata: b\r\r\
                      data: c\n\n"
            .as_bytes();
        let expected = vec![
            ServerEvent {
                id: Some("1".into()),
                event: Some("tick".into()),
                data: "a\n\u{20ac}".into(),
                retry: None,
            },
            ServerEvent {
                data: "b".into(),
                ..ServerEvent::default()
            },
            ServerEvent {
                data: "c".into(),
                ..ServerEvent::default()
            },
        ];

        for at in 0..=stream.len() {
            let chunks = vec![stream[..at].to_vec(), stream[at..].to_vec()];
            assert_eq!(parse_byte_chunks(chunks).await, expected, "split at {at}");
        }
        let bytewise = stream.iter().map(|b| vec![*b]).collect();
        assert_eq!(parse_byte_chunks(bytewise).await, expected, "byte by byte");
    }

    // -- Tab is not stripped (only space is) --------------------------------

    #[tokio::test]
    async fn tab_after_colon_not_stripped() {
        // Per spec, only a single U+0020 SPACE after the colon is removed.
        let body = body_from_chunks(vec!["data:\ttest\n\n"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events[0].data, "\ttest");
    }
}
