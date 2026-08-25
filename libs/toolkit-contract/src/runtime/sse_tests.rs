//! Tests for the SSE stream parser.

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
async fn finds_newline_after_many_chunks_with_none() {
    // Regression test for the incremental `scan_from` optimization (#15):
    // several chunks arrive with NO newline at all before one finally
    // completes the line. If `scan_from` bookkeeping were wrong (e.g.
    // never advanced, or advanced past the eventual `\n`), this would
    // either loop scanning the same bytes forever or miss the newline.
    let s = chunks(&["data: {\"i", "d\"", ":", "9", "}", "\n\nevent: done\n\n"]);
    let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
    let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(parsed, vec![Item { id: 9 }]);
}

#[tokio::test]
async fn finds_multiple_newlines_within_one_chunk_after_split() {
    // After a successful split, `scan_from` must reset to 0 for the
    // remainder — otherwise a `\n` arriving in the SAME chunk as the one
    // that completed the prior event would be missed until a later poll
    // (or never, if no more chunks arrive). Two full events delivered in
    // ONE chunk exercises exactly that: both must be found and dispatched
    // within this single `drain_buffer` call.
    let s = chunks(&["data: {\"id\":1}\n\ndata: {\"id\":2}\n\nevent: done\n\n"]);
    let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
    let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(parsed, vec![Item { id: 1 }, Item { id: 2 }]);
}

#[tokio::test]
async fn aborts_on_unterminated_line_exceeding_max_size() {
    // A single line with no terminating `\n` that exceeds the cap must
    // abort the stream with a typed error rather than growing `buf`
    // without bound (#3 — OOM/DoS guard).
    let huge = "a".repeat(MAX_ACCUMULATED_BYTES + 1);
    let s = chunks(&[huge.as_str()]);
    let parsed: Vec<Result<Item, TransportError>> =
        parse_sse_stream::<Item, _, _>(s).collect().await;
    assert_eq!(parsed.len(), 1, "expected exactly one terminal error item");
    assert!(
        matches!(parsed[0], Err(TransportError::Sse(_))),
        "expected TransportError::Sse, got {:?}",
        parsed[0]
    );
}

#[tokio::test]
async fn saw_done_event_is_false_when_stream_ends_without_done() {
    // The stream ends cleanly (no error) but WITHOUT an `event: done`
    // frame — the streaming client relies on `saw_done_event()` to tell
    // this apart from an explicit done so it can reconnect instead of
    // silently treating a bare disconnect as success.
    let s = chunks(&["data: {\"id\":1}\n\n"]);
    let mut stream = parse_sse_stream::<Item, _, _>(s);
    let first = stream.next().await;
    assert!(matches!(first, Some(Ok(Item { id: 1 }))));
    let second = stream.next().await;
    assert!(second.is_none(), "expected clean end, got {second:?}");
    assert!(
        !stream.saw_done_event(),
        "stream ended without an explicit `done` event"
    );
}

#[tokio::test]
async fn saw_done_event_is_true_when_done_dispatched() {
    let s = chunks(&["data: {\"id\":1}\n\n", "event: done\n\n"]);
    let mut stream = parse_sse_stream::<Item, _, _>(s);
    assert!(matches!(stream.next().await, Some(Ok(Item { id: 1 }))));
    assert!(stream.next().await.is_none());
    assert!(stream.saw_done_event());
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
        Err(TransportError::Problem { problem: p, .. }) => {
            assert_eq!(p.detail, "broke");
            assert!(p.problem_type.contains("internal"));
        }
        other => panic!("expected Problem, got {other:?}"),
    }
}

#[tokio::test]
async fn surfaces_a_minimal_spec_compliant_error_event_as_problem() {
    // RFC 9457 §3.1 makes `detail`/`context` optional - a foreign peer's
    // error event can omit both and still be fully spec-compliant. It
    // must still surface as `TransportError::Problem` (preserving the
    // real `type`/`title`), not degrade to a "malformed error event"
    // serialization failure.
    let problem = serde_json::json!({
        "type": "https://example.com/probs/out-of-credit",
        "title": "You do not have enough credit.",
        "status": 409
    });
    let body = format!("event: error\ndata: {problem}\n\nevent: done\n\n");
    let s = chunks(&[&body]);
    let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
    assert_eq!(parsed.len(), 1);
    match parsed.into_iter().next().unwrap() {
        Err(TransportError::Problem { problem: p, .. }) => {
            assert_eq!(p.problem_type, "https://example.com/probs/out-of-credit");
            assert_eq!(p.title, "You do not have enough credit.");
            assert_eq!(p.status, Some(409));
            assert_eq!(p.detail, "");
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
async fn ignores_named_control_events() {
    // A named event (`ping`) with a JSON body must NOT be decoded as the
    // typed item; only the implicit `message` channel yields items.
    let s = chunks(&[
        "event: ping\ndata: {\"id\":9}\n\n",
        "data: {\"id\":1}\n\n",
        "event: done\n\n",
    ]);
    let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
    let parsed: Vec<Item> = parsed.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(parsed, vec![Item { id: 1 }]);
}

#[tokio::test]
async fn discards_truncated_trailing_event_at_eof() {
    // Stream ends mid-event (no terminating blank line): the incomplete
    // event is discarded per spec — no item and no serialization error.
    let s = chunks(&["data: {\"id\":", "5"]);
    let parsed: Vec<_> = parse_sse_stream::<Item, _, _>(s).collect().await;
    assert!(parsed.is_empty(), "expected no items, got {parsed:?}");
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
