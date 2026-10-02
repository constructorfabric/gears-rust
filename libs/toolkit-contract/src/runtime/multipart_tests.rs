//! Tests for the `multipart/mixed` stream reader.

use super::*;
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;

const BOUNDARY: &str = "cf-test-boundary";

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

fn reader(parts: &[&str]) -> impl Stream<Item = Result<Item, TransportError>> + Unpin + use<> {
    Box::pin(parse_multipart_stream::<Item, _, _>(
        chunks(parts),
        BOUNDARY,
    ))
}

/// One part with an exact `Content-Length` (the length-driven mode).
fn sized_part(id: u32) -> String {
    let json = format!("{{\"id\":{id}}}");
    format!(
        "--{BOUNDARY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}\r\n",
        json.len()
    )
}

/// One part with no `Content-Length` (the delimiter-scan mode).
fn unsized_part(id: u32) -> String {
    format!("--{BOUNDARY}\r\nContent-Type: application/json\r\n\r\n{{\"id\":{id}}}\r\n")
}

fn close() -> String {
    format!("--{BOUNDARY}--\r\n")
}

async fn parse(parts: &[&str]) -> Vec<Result<Item, TransportError>> {
    reader(parts).collect().await
}

async fn parse_ok(parts: &[&str]) -> Vec<Item> {
    parse(parts).await.into_iter().map(|r| r.unwrap()).collect()
}

// ---------------------------------------------------------------------------
// Length-driven framing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reads_one_sized_part() {
    let wire = format!("{}{}", sized_part(1), close());
    assert_eq!(parse_ok(&[wire.as_str()]).await, vec![Item { id: 1 }]);
}

#[tokio::test]
async fn reads_many_sized_parts() {
    let wire = format!(
        "{}{}{}{}",
        sized_part(1),
        sized_part(2),
        sized_part(3),
        close()
    );
    assert_eq!(
        parse_ok(&[wire.as_str()]).await,
        vec![Item { id: 1 }, Item { id: 2 }, Item { id: 3 }]
    );
}

#[tokio::test]
async fn reads_sized_parts_split_across_arbitrary_chunk_boundaries() {
    let wire = format!("{}{}{}", sized_part(1), sized_part(2), close());
    // Split at every offset in turn: the reader must be insensitive to where
    // the transport happens to cut the byte stream.
    for split in 1..wire.len() {
        let (head, tail) = wire.split_at(split);
        assert_eq!(
            parse_ok(&[head, tail]).await,
            vec![Item { id: 1 }, Item { id: 2 }],
            "failed with the stream split at byte {split}"
        );
    }
}

#[tokio::test]
async fn reads_sized_parts_fed_one_byte_at_a_time() {
    let wire = format!("{}{}{}", sized_part(7), sized_part(8), close());
    let bytes: Vec<String> = wire.chars().map(String::from).collect();
    let refs: Vec<&str> = bytes.iter().map(String::as_str).collect();
    assert_eq!(parse_ok(&refs).await, vec![Item { id: 7 }, Item { id: 8 }]);
}

// ---------------------------------------------------------------------------
// Delimiter-driven (length-less) framing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reads_many_unsized_parts() {
    let wire = format!("{}{}{}", unsized_part(1), unsized_part(2), close());
    assert_eq!(
        parse_ok(&[wire.as_str()]).await,
        vec![Item { id: 1 }, Item { id: 2 }]
    );
}

#[tokio::test]
async fn reads_unsized_parts_split_across_arbitrary_chunk_boundaries() {
    let wire = format!("{}{}{}", unsized_part(1), unsized_part(2), close());
    for split in 1..wire.len() {
        let (head, tail) = wire.split_at(split);
        assert_eq!(
            parse_ok(&[head, tail]).await,
            vec![Item { id: 1 }, Item { id: 2 }],
            "failed with the stream split at byte {split}"
        );
    }
}

#[tokio::test]
async fn reads_unsized_parts_fed_one_byte_at_a_time() {
    let wire = format!("{}{}", unsized_part(4), close());
    let bytes: Vec<String> = wire.chars().map(String::from).collect();
    let refs: Vec<&str> = bytes.iter().map(String::as_str).collect();
    assert_eq!(parse_ok(&refs).await, vec![Item { id: 4 }]);
}

#[tokio::test]
async fn mixed_sized_and_unsized_parts_interoperate() {
    // `Content-Length` is a per-part fast path, not a per-stream mode.
    let wire = format!(
        "{}{}{}{}",
        sized_part(1),
        unsized_part(2),
        sized_part(3),
        close()
    );
    assert_eq!(
        parse_ok(&[wire.as_str()]).await,
        vec![Item { id: 1 }, Item { id: 2 }, Item { id: 3 }]
    );
}

#[tokio::test]
async fn part_without_any_headers_is_read() {
    // Zero headers is legal MIME: the header block is just its empty line.
    let wire = format!("--{BOUNDARY}\r\n\r\n{{\"id\":5}}\r\n{}", close());
    assert_eq!(parse_ok(&[wire.as_str()]).await, vec![Item { id: 5 }]);
}

#[tokio::test]
async fn unknown_part_headers_are_ignored() {
    let wire = format!(
        "--{BOUNDARY}\r\nX-Trace: abc\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n{{\"id\":6}}\r\n{}",
        close()
    );
    assert_eq!(parse_ok(&[wire.as_str()]).await, vec![Item { id: 6 }]);
}

// ---------------------------------------------------------------------------
// Latency — the property the whole length-driven design exists to buy (D3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sized_part_is_yielded_before_the_next_parts_bytes_exist() {
    // D3's reason for being: with a `Content-Length` the reader knows a part
    // is complete from that part's own bytes, so it never runs a part behind a
    // live producer. Feed exactly one part and NOTHING else — no following
    // delimiter, no close delimiter, no further bytes at all — and the item
    // must still arrive.
    let wire = sized_part(1);
    let mut stream = reader(&[wire.as_str()]);
    assert_eq!(
        stream.next().await.map(Result::unwrap),
        Some(Item { id: 1 }),
        "a sized part must be yielded from its own bytes alone"
    );
}

#[tokio::test]
async fn unsized_part_is_not_yielded_until_its_closing_delimiter_arrives() {
    // The contrast that makes the assertion above mean something: the same
    // bytes without a `Content-Length` yield nothing, because the part's
    // extent is only known once the next delimiter lands. That is the "one
    // part behind" behavior D3 exists to avoid, pinned here so a future change
    // cannot quietly make the length-driven path behave this way too.
    let wire = unsized_part(1);
    let mut stream = reader(&[wire.as_str()]);
    // The feed is finite, so the reader reaches a graceful EOF with the part
    // still incomplete and ends cleanly, discarding it (Q4).
    assert!(
        stream.next().await.is_none(),
        "an unsized part must not be emitted before its closing delimiter"
    );
}

// ---------------------------------------------------------------------------
// Preamble and close semantics (Q4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preamble_is_discarded() {
    let wire = format!(
        "This is a multipart message in MIME format.\r\n{}{}",
        sized_part(1),
        close()
    );
    assert_eq!(parse_ok(&[wire.as_str()]).await, vec![Item { id: 1 }]);
}

#[tokio::test]
async fn close_delimiter_ends_the_stream_cleanly() {
    let wire = format!("{}{}", sized_part(1), close());
    let mut stream = Box::pin(parse_multipart_stream::<Item, _, _>(
        chunks(&[wire.as_str()]),
        BOUNDARY,
    ));
    let collected: Vec<_> = stream.by_ref().collect().await;
    assert!(collected.iter().all(Result::is_ok), "got {collected:?}");
    assert!(stream.saw_close_delimiter());
}

#[tokio::test]
async fn graceful_eof_without_close_delimiter_ends_the_stream_cleanly() {
    // Q4: no missing-terminator error is manufactured, deliberately unlike
    // SSE. `saw_close_delimiter()` still reports the framing fact, so a
    // consumer that cares can see it — but the stream itself ends clean.
    let wire = format!("{}{}", sized_part(1), sized_part(2));
    let mut stream = Box::pin(parse_multipart_stream::<Item, _, _>(
        chunks(&[wire.as_str()]),
        BOUNDARY,
    ));
    let collected: Vec<_> = stream.by_ref().collect().await;
    let items: Vec<Item> = collected.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(items, vec![Item { id: 1 }, Item { id: 2 }]);
    assert!(
        !stream.saw_close_delimiter(),
        "the peer sent no close delimiter, and that must stay visible"
    );
}

#[tokio::test]
async fn empty_body_ends_the_stream_cleanly() {
    assert!(parse(&[]).await.is_empty());
}

#[tokio::test]
async fn upstream_byte_stream_error_is_a_network_error() {
    // The aborted-body counterpart to a graceful EOF: this is how a server
    // framer's mid-stream failure reaches the consumer (a truncated chunked
    // encoding), and unlike a graceful EOF it must be loud.
    let feed: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from(sized_part(1))),
        Err(std::io::Error::other("connection reset")),
    ];
    let stream = parse_multipart_stream::<Item, _, _>(Box::pin(stream::iter(feed)), BOUNDARY);
    let results: Vec<_> = stream.collect().await;
    assert_eq!(results.len(), 2, "got {results:?}");
    assert!(results[0].is_ok());
    assert!(
        matches!(results[1], Err(TransportError::Network(_))),
        "expected Network, got {:?}",
        results[1]
    );
}

// ---------------------------------------------------------------------------
// Error surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_json_in_a_part_is_a_serialization_error_and_terminates() {
    // A correctly framed part (its length agrees with its delimiter) whose
    // payload will not decode: the payload is at fault, and the stream stops
    // rather than silently dropping data.
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: 5\r\n\r\n{{\"id\"\r\n{}{}",
        sized_part(2),
        close()
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(
        results.len(),
        1,
        "a malformed part must terminate the stream, got {results:?}"
    );
    assert!(
        matches!(results[0], Err(TransportError::Serialization(_))),
        "expected Serialization, got {:?}",
        results[0]
    );
}

#[tokio::test]
async fn malformed_json_in_an_unsized_part_is_a_serialization_error() {
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Type: application/json\r\n\r\n{{\"id\"\r\n{}",
        close()
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Serialization(_))));
}

#[tokio::test]
async fn malformed_json_survives_a_graceful_eof() {
    // A sized part's decode failure is held until its delimiter proves whose
    // fault it was. If the stream simply ends instead, the part was still
    // complete (its length said so) and still did not decode — that is data
    // loss, not truncation, so Q4's clean end must not swallow it.
    let wire = format!("--{BOUNDARY}\r\nContent-Length: 5\r\n\r\n{{\"id\"\r\n");
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Serialization(_))));
}

#[tokio::test]
async fn content_length_overrunning_the_delimiter_is_a_framing_error() {
    // Declared length is longer than the body written, so the reader consumes
    // into the delimiter and what follows the body is not one. The length is
    // the root cause and is reported as such, rather than as the decode
    // failure it happens to produce.
    let json = "{\"id\":1}";
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: {}\r\n\r\n{json}\r\n--{BOUNDARY}--\r\n",
        json.len() + 4
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(
        matches!(
            results[0],
            Err(TransportError::Framing {
                framing: StreamFraming::MultipartMixed,
                ..
            })
        ),
        "expected Framing, got {:?}",
        results[0]
    );
}

#[test]
fn display_value_escapes_control_chars_and_truncates() {
    // #4740: an untrusted header value must not carry control bytes or megabytes
    // into an error string.
    assert_eq!(display_value("12\r34"), "12\\r34");
    let long = "9".repeat(200);
    let rendered = display_value(&long);
    assert!(rendered.ends_with("..."), "got {rendered:?}");
    assert!(
        rendered.chars().count() <= 67,
        "got {} chars",
        rendered.chars().count()
    );
}

#[tokio::test]
async fn a_bad_content_length_escapes_the_value_and_keeps_the_source() {
    // #4740: a `Content-Length` with an interior CR must be reported with the
    // control byte escaped (no raw CR in the message), and the underlying
    // `ParseIntError` must stay reachable via `source()`.
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: 12\r34\r\n\r\n{{\"id\":1}}\r\n{}",
        close()
    );
    let results = parse(&[wire.as_str()]).await;
    let err = results
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("a non-numeric Content-Length must surface a framing error");
    let TransportError::Framing { source, .. } = err else {
        panic!("expected a framing error, got {err:?}");
    };
    let rendered = source.to_string();
    assert!(
        rendered.contains("12\\r34"),
        "control byte must be escaped: {rendered:?}"
    );
    assert!(
        !rendered.contains('\r'),
        "no raw CR in the message: {rendered:?}"
    );
    assert!(
        std::error::Error::source(source.as_ref()).is_some(),
        "the underlying parse error must be reachable via source()",
    );
}

#[tokio::test]
async fn content_length_falling_short_of_the_delimiter_is_a_framing_error() {
    // Declared length is shorter than the body written, leaving payload bytes
    // where a delimiter must be. A length disagreeing with its delimiter is
    // one fault reported one way, whichever direction it disagrees in.
    let json = "{\"id\":1}";
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: {}\r\n\r\n{json}\r\n--{BOUNDARY}--\r\n",
        json.len() - 2
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(
        matches!(results[0], Err(TransportError::Framing { .. })),
        "expected Framing, got {:?}",
        results[0]
    );
}

#[tokio::test]
async fn unparseable_content_length_is_a_framing_error() {
    let wire = format!("--{BOUNDARY}\r\nContent-Length: many\r\n\r\n{{\"id\":1}}\r\n");
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Framing { .. })));
}

#[tokio::test]
async fn content_length_beyond_the_accumulation_guard_is_rejected_up_front() {
    // Rejected from the header, not by buffering until the guard trips: a peer
    // must not be able to make the reader allocate its way to the limit.
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: {}\r\n\r\n",
        MAX_ACCUMULATED_BYTES + 1
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Framing { .. })));
}

#[tokio::test]
async fn a_boundary_prefix_that_is_not_a_delimiter_is_a_framing_error() {
    // `--<boundary>x` is a different token, not a delimiter: neither `CRLF`
    // nor `--` follows the boundary.
    let wire = format!("--{BOUNDARY}x\r\nContent-Length: 8\r\n\r\n{{\"id\":1}}\r\n");
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Framing { .. })));
}

#[tokio::test]
async fn transport_padding_before_a_part_is_accepted() {
    // #4740: RFC 2046 permits transport padding (spaces/tabs) between a boundary
    // and its CRLF. Only the reject path (`--<boundary>x`) was tested; a padded
    // delimiter — here `--<boundary>  \t\r\n` before the second part — must
    // still read the part.
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Length: 8\r\n\r\n{{\"id\":1}}\r\n\
         --{BOUNDARY}  \t\r\nContent-Length: 8\r\n\r\n{{\"id\":2}}\r\n\
         --{BOUNDARY}--\r\n"
    );
    assert_eq!(
        parse_ok(&[wire.as_str()]).await,
        vec![Item { id: 1 }, Item { id: 2 }]
    );
}

#[tokio::test]
async fn oversized_part_headers_trip_the_guard() {
    // One header line longer than the cap, never terminated by an empty line.
    let wire = format!(
        "--{BOUNDARY}\r\nX-Pad: {}",
        "a".repeat(MAX_ACCUMULATED_BYTES + 1)
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(
        matches!(results[0], Err(TransportError::Framing { .. })),
        "expected Framing, got {:?}",
        results[0]
    );
}

#[tokio::test]
async fn oversized_length_less_body_trips_the_guard() {
    let wire = format!(
        "--{BOUNDARY}\r\nContent-Type: application/json\r\n\r\n{}",
        "a".repeat(MAX_ACCUMULATED_BYTES + 1)
    );
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(
        matches!(results[0], Err(TransportError::Framing { .. })),
        "expected Framing, got {:?}",
        results[0]
    );
}

#[tokio::test]
async fn oversized_preamble_trips_the_guard() {
    let wire = "x".repeat(MAX_ACCUMULATED_BYTES + 1);
    let results = parse(&[wire.as_str()]).await;
    assert_eq!(results.len(), 1, "got {results:?}");
    assert!(matches!(results[0], Err(TransportError::Framing { .. })));
}

// ---------------------------------------------------------------------------
// `boundary_from_content_type`
// ---------------------------------------------------------------------------

#[test]
fn boundary_is_extracted_unquoted() {
    assert_eq!(
        boundary_from_content_type("multipart/mixed; boundary=abc123").unwrap(),
        "abc123"
    );
}

#[test]
fn boundary_is_extracted_quoted() {
    assert_eq!(
        boundary_from_content_type("multipart/mixed; boundary=\"abc123\"").unwrap(),
        "abc123"
    );
}

#[test]
fn boundary_extraction_is_case_and_whitespace_insensitive() {
    assert_eq!(
        boundary_from_content_type("Multipart/Mixed ;  Boundary =  abc123").unwrap(),
        "abc123"
    );
}

#[test]
fn boundary_extraction_skips_other_parameters() {
    assert_eq!(
        boundary_from_content_type("multipart/mixed; charset=utf-8; boundary=abc123").unwrap(),
        "abc123"
    );
}

#[test]
fn boundary_extraction_rejects_the_wrong_media_type() {
    assert!(matches!(
        boundary_from_content_type("text/event-stream").unwrap_err(),
        TransportError::Framing { .. }
    ));
    assert!(matches!(
        boundary_from_content_type("multipart/form-data; boundary=abc").unwrap_err(),
        TransportError::Framing { .. }
    ));
}

#[test]
fn boundary_extraction_rejects_a_missing_or_malformed_parameter() {
    for header in [
        "multipart/mixed",
        "multipart/mixed; boundary=",
        "multipart/mixed; boundary=\"abc",
    ] {
        assert!(
            matches!(
                boundary_from_content_type(header).unwrap_err(),
                TransportError::Framing { .. }
            ),
            "expected Framing for `{header}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Activity counter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn activity_advances_on_a_chunk_that_dispatches_no_item() {
    // Mirrors the SSE parser's keepalive property: the idle deadline must stay
    // idle, so a chunk carrying only a part's header block still counts.
    let head = format!("--{BOUNDARY}\r\nContent-Length: 8\r\n\r\n");
    let feed: Vec<Result<Bytes, std::io::Error>> =
        vec![Ok(Bytes::from(head)), Ok(Bytes::from_static(b"{\"id\":1}"))];
    let mut stream = Box::pin(parse_multipart_stream::<Item, _, _>(
        Box::pin(stream::iter(feed)),
        BOUNDARY,
    ));
    let activity = stream.activity_handle();
    let before = activity.generation();
    assert_eq!(
        stream.next().await.map(Result::unwrap),
        Some(Item { id: 1 })
    );
    assert!(
        activity.generation() >= before + 2,
        "both chunks must count as activity"
    );
}

// ---------------------------------------------------------------------------
// Typed error parts (#4740 3B): a part whose `Content-Type` is
// `application/problem+json` decodes as a `Problem` and is surfaced as a typed
// `Err` item — non-terminally — so the stream reads on to a clean close.
// ---------------------------------------------------------------------------

fn problem(status: u16) -> Problem {
    let mut p = Problem::from(
        toolkit_canonical_errors::CanonicalError::internal("mid-stream boom").create(),
    );
    p.status = Some(status);
    p
}

/// A sized error part carrying a serialized [`Problem`].
fn error_part(problem: &Problem, content_type: &str) -> String {
    let json = serde_json::to_string(problem).unwrap();
    format!(
        "--{BOUNDARY}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{json}\r\n",
        json.len()
    )
}

#[tokio::test]
async fn an_error_part_becomes_a_typed_err_then_a_clean_close() {
    // The framer's shape: a data part, then a problem+json error part, then the
    // close delimiter. The reader must yield `[Ok, Err(Problem)]` and end
    // cleanly — no manufactured framing/truncation error after the `Err`.
    let wire = format!(
        "{}{}{}",
        sized_part(1),
        error_part(&problem(500), "application/problem+json"),
        close(),
    );
    let items = parse(&[wire.as_str()]).await;
    assert_eq!(
        items.len(),
        2,
        "one data item, one typed error, no extra item"
    );
    assert_eq!(items[0].as_ref().unwrap(), &Item { id: 1 });
    match items[1].as_ref().unwrap_err() {
        TransportError::Problem { problem, .. } => assert_eq!(problem.status, Some(500)),
        other => panic!("expected a typed Problem error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_error_part_content_type_is_matched_ignoring_parameters() {
    // `application/problem+json; charset=utf-8` still marks the error part.
    let wire = format!(
        "{}{}",
        error_part(&problem(409), "application/problem+json; charset=utf-8"),
        close(),
    );
    let items = parse(&[wire.as_str()]).await;
    assert_eq!(items.len(), 1);
    match items[0].as_ref().unwrap_err() {
        TransportError::Problem { problem, .. } => assert_eq!(problem.status, Some(409)),
        other => panic!("expected a typed Problem error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_error_part_is_terminal_and_trailing_parts_are_not_surfaced() {
    // F5: a non-conforming peer sends a data part AFTER the error part. The
    // reader must stop at the error — the trailing data must never reach the
    // caller as a valid item, even though the framer would never emit it.
    let wire = format!(
        "{}{}{}{}",
        sized_part(1),
        error_part(&problem(500), "application/problem+json"),
        sized_part(2), // hostile trailing data, no close before it
        close(),
    );
    let items = parse(&[wire.as_str()]).await;
    assert_eq!(
        items.len(),
        2,
        "the error must terminate the stream: {items:?}"
    );
    assert_eq!(items[0].as_ref().unwrap(), &Item { id: 1 });
    assert!(matches!(
        items[1].as_ref().unwrap_err(),
        TransportError::Problem { .. }
    ));
    assert!(
        !items.iter().any(|r| matches!(r, Ok(Item { id: 2 }))),
        "a trailing data part after an error must not be surfaced: {items:?}"
    );
}

#[tokio::test]
async fn an_unsized_error_part_is_decoded_too() {
    // The delimiter-scan mode (no `Content-Length`) must branch on the error
    // content-type just like the sized mode.
    let json = serde_json::to_string(&problem(503)).unwrap();
    let part = format!("--{BOUNDARY}\r\nContent-Type: application/problem+json\r\n\r\n{json}\r\n");
    let wire = format!("{part}{}", close());
    let items = parse(&[wire.as_str()]).await;
    assert_eq!(items.len(), 1);
    assert!(matches!(
        items[0].as_ref().unwrap_err(),
        TransportError::Problem { .. }
    ));
}
