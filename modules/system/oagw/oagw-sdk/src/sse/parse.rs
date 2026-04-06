use std::collections::VecDeque;
use std::pin::Pin;

use futures_core::Stream;
use futures_util::StreamExt;

use crate::body::BodyStream;
use crate::error::StreamingError;
use crate::sse::ServerEvent;

struct ParseState {
    body: BodyStream,
    buf: String,
    /// Events parsed from the current buffer but not yet yielded.
    pending: VecDeque<ServerEvent>,
    /// Trailing bytes from the previous chunk that form an incomplete UTF-8 sequence.
    /// Prepended to the next chunk before decoding.
    utf8_tail: Vec<u8>,
    /// Whether this is the first chunk (for BOM stripping).
    first_chunk: bool,
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
fn normalize_line_endings(s: &str) -> String {
    // Replace CRLF first, then any remaining bare CR.
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split buffered text on event boundaries (`\n\n`), returning completed
/// event blocks and leaving any partial trailing data in the buffer.
fn extract_events(buf: &mut String) -> VecDeque<ServerEvent> {
    let mut events = VecDeque::new();

    // SSE events are separated by blank lines (\n\n).
    // We split on \n\n and process each block.
    loop {
        // Find the next event boundary.
        let boundary = buf.find("\n\n");
        let Some(pos) = boundary else {
            break;
        };

        let block = &buf[..pos];
        if !block.is_empty() {
            let mut event = ServerEvent::default();
            for line in block.lines() {
                parse_line(line, &mut event);
            }
            if !event.is_empty() {
                events.push_back(event);
            }
        }

        // Remove the consumed block + the two newlines.
        let drain_to = pos + 2;
        // There may be more consecutive newlines — skip them.
        let remainder = &buf[drain_to..];
        let trimmed = remainder.trim_start_matches('\n');
        let extra_newlines = remainder.len() - trimmed.len();
        *buf = buf[drain_to + extra_newlines..].to_owned();
    }

    events
}

/// Parse a raw byte stream into a stream of SSE events.
///
/// Chunks are buffered internally and split on blank-line boundaries (`\n\n`).
/// Malformed lines within an event are silently skipped (per W3C EventSource spec).
/// Empty events (comment-only blocks) are not yielded.
#[allow(clippy::type_complexity)]
pub fn parse_server_events_stream(
    body: BodyStream,
) -> Pin<Box<dyn Stream<Item = Result<ServerEvent, StreamingError>> + Send>> {
    let state = ParseState {
        body,
        buf: String::new(),
        pending: VecDeque::new(),
        utf8_tail: Vec::new(),
        first_chunk: true,
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

                if state.done {
                    // Stream is finished. Flush any remaining data in the buffer.
                    if !state.buf.trim().is_empty() {
                        let mut event = ServerEvent::default();
                        for line in state.buf.lines() {
                            parse_line(line, &mut event);
                        }
                        state.buf.clear();
                        if !event.is_empty() {
                            return Some((Ok(event), state));
                        }
                    }
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
                                // Truly invalid UTF-8 byte(s) — unrecoverable.
                                return Some((
                                    Err(StreamingError::ServerEventsParse {
                                        detail: format!("invalid UTF-8: {e}"),
                                    }),
                                    state,
                                ));
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
                            state.buf.push_str(&normalize_line_endings(&text));
                            state.pending = extract_events(&mut state.buf);
                        }
                        // Loop back to yield pending events.
                    }
                    Some(Err(e)) => {
                        state.done = true;
                        return Some((Err(StreamingError::Stream(e)), state));
                    }
                    None => {
                        state.done = true;
                        // Loop back to flush remaining buffer.
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod parse_tests;
