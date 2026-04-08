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
mod tests {
    use super::*;
    use crate::body::BoxError;
    use bytes::Bytes;
    use futures_util::StreamExt;

    /// Helper: create a BodyStream from a list of string chunks.
    fn body_from_chunks(chunks: Vec<&str>) -> BodyStream {
        let owned: Vec<Result<Bytes, BoxError>> = chunks
            .into_iter()
            .map(|s| Ok(Bytes::from(s.to_owned())))
            .collect();
        Box::pin(futures_util::stream::iter(owned))
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

    #[tokio::test]
    async fn flush_trailing_event_without_final_newlines() {
        // Some servers don't send the trailing \n\n for the last event.
        let body = body_from_chunks(vec!["data: trailing"]);
        let events: Vec<_> = parse_server_events_stream(body)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "trailing");
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
