//! Stream Supervisor tests (DESIGN §3.2 Stream Supervisor, §3.3 wire
//! behavior, `cpt-cf-llm-gateway-seq-streaming-v1`,
//! `cpt-cf-llm-gateway-seq-stream-cancellation-v1`).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::ResponseStatus;
use llm_gateway_sdk::models::streaming::ResponseSnapshotEvent;
use llm_gateway_sdk::{ResponseEventStream, StreamingEvent, Usage};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{StreamParams, initial_snapshot, supervise};
use crate::domain::test_support::{
    body, ev_completed, ev_created, ev_error, ev_failed, ev_in_progress, ev_incomplete,
    ev_text_delta,
};

const ID: &str = "resp_gateway";

/// Provider-final usage; built through serde so the test crate does not
/// depend on `rust_decimal` directly.
fn reported(input: u64, output: u64) -> Usage {
    serde_json::from_value(json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": input + output,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens_details": { "reasoning_tokens": 0 },
        "cost": "0.5",
        "complete": true,
        "estimated": false
    }))
    .expect("usage deserializes")
}

fn upstream(events: Vec<StreamingEvent>) -> ResponseEventStream {
    Box::pin(futures_util::stream::iter(events))
}

fn hanging(events: Vec<StreamingEvent>) -> ResponseEventStream {
    Box::pin(upstream(events).chain(futures_util::stream::pending()))
}

fn params() -> StreamParams {
    StreamParams {
        response_id: ID.to_owned(),
        snapshot: initial_snapshot(ID, &body("openai::gpt-4o")),
        channel_capacity: 8,
        cancel: CancellationToken::new(),
        output_schema: None,
        ignored_parameters: Vec::new(),
        slot: None,
        estimator: None,
        on_terminal: None,
        limits: super::StreamLimits::default(),
    }
}

async fn run(events: Vec<StreamingEvent>, params: StreamParams) -> Vec<StreamingEvent> {
    tokio::time::timeout(
        Duration::from_secs(2),
        supervise(upstream(events), params).collect::<Vec<_>>(),
    )
    .await
    .expect("the supervised stream ends")
}

fn kind(event: &StreamingEvent) -> String {
    serde_json::to_value(event).expect("event serializes")["type"]
        .as_str()
        .expect("type")
        .to_owned()
}

fn kinds(events: &[StreamingEvent]) -> Vec<String> {
    events.iter().map(kind).collect()
}

fn seq(event: &StreamingEvent) -> u64 {
    serde_json::to_value(event).expect("event serializes")["sequence_number"]
        .as_u64()
        .expect("sequence_number")
}

fn snapshot_of(event: &StreamingEvent) -> &ResponseSnapshotEvent {
    match event {
        StreamingEvent::Created(e)
        | StreamingEvent::InProgress(e)
        | StreamingEvent::Completed(e)
        | StreamingEvent::Incomplete(e)
        | StreamingEvent::Failed(e) => e,
        other => panic!("not a snapshot event: {other:?}"),
    }
}

fn is_terminal(event: &StreamingEvent) -> bool {
    matches!(
        event,
        StreamingEvent::Completed(_) | StreamingEvent::Incomplete(_) | StreamingEvent::Failed(_)
    )
}

fn terminals(events: &[StreamingEvent]) -> Vec<&StreamingEvent> {
    events.iter().filter(|e| is_terminal(e)).collect()
}

fn last(events: &[StreamingEvent]) -> &ResponseSnapshotEvent {
    snapshot_of(events.last().expect("at least one event"))
}

// ── initial snapshot ────────────────────────────────────────────────────────

#[test]
fn initial_snapshot_is_in_progress_with_gateway_id_and_model() {
    let mut request = body("openai::gpt-4o");
    request.temperature = Some(0.4);
    request.instructions = Some("be brief".to_owned());
    let snap = initial_snapshot(ID, &request);

    assert_eq!(snap.id, ID);
    assert_eq!(snap.model, "openai::gpt-4o");
    assert_eq!(snap.status, ResponseStatus::InProgress);
    assert!(snap.output.is_empty());
    assert!(snap.usage.is_none());
    assert!(snap.error.is_none());
    assert!(snap.completed_at.is_none());
    assert!((snap.temperature - 0.4).abs() < f64::EPSILON);
    assert_eq!(snap.instructions.as_deref(), Some("be brief"));
}

// ── framing: created / in_progress first, renumbering, id ───────────────────

#[tokio::test]
async fn created_and_in_progress_come_first_with_in_progress_status_and_gateway_id() {
    let events = run(vec![ev_completed(0, "provider_id", None)], params()).await;

    assert_eq!(
        kinds(&events)[..2],
        ["response.created", "response.in_progress"]
    );
    for event in &events[..2] {
        let snap = snapshot_of(event);
        assert_eq!(snap.response.id, ID);
        assert_eq!(snap.response.status, ResponseStatus::InProgress);
    }
    assert_eq!(seq(&events[0]), 0);
    assert_eq!(seq(&events[1]), 1);
}

#[tokio::test]
async fn provider_sequence_numbers_are_replaced_by_a_gapless_sequence_from_zero() {
    let events = run(
        vec![
            ev_text_delta(100, "a"),
            ev_text_delta(5, "b"),
            ev_text_delta(7, "c"),
            ev_completed(9, "p", None),
        ],
        params(),
    )
    .await;

    let numbers: Vec<u64> = events.iter().map(seq).collect();
    assert_eq!(numbers, (0..numbers.len() as u64).collect::<Vec<_>>());
    assert_eq!(events.len(), 6, "created, in_progress, 3 deltas, terminal");
}

#[tokio::test]
async fn provider_created_and_in_progress_are_not_relayed_twice() {
    let events = run(
        vec![
            ev_created(0, "provider_id"),
            ev_in_progress(1, "provider_id", None),
            ev_text_delta(2, "x"),
            ev_completed(3, "provider_id", None),
        ],
        params(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn gateway_id_is_forced_on_the_terminal_snapshot() {
    let events = run(vec![ev_completed(0, "provider_id", None)], params()).await;
    assert_eq!(last(&events).response.id, ID);
}

#[tokio::test]
async fn delta_events_are_relayed_unchanged_apart_from_the_sequence_number() {
    let events = run(
        vec![ev_text_delta(40, "hello"), ev_completed(41, "p", None)],
        params(),
    )
    .await;

    let StreamingEvent::OutputTextDelta(delta) = &events[2] else {
        panic!("expected a delta, got {:?}", events[2]);
    };
    assert_eq!(delta.text, "hello");
    assert_eq!(delta.item_id, "msg_1");
    assert_eq!(delta.sequence_number, 2);
}

// ── terminal handling ───────────────────────────────────────────────────────

#[tokio::test]
async fn provider_completed_is_relayed_with_its_usage() {
    let usage = reported(10, 20);
    let events = run(
        vec![
            ev_text_delta(0, "hi"),
            ev_completed(1, "p", Some(usage.clone())),
        ],
        params(),
    )
    .await;

    assert_eq!(terminals(&events).len(), 1);
    let terminal = last(&events);
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(terminal.response.status, ResponseStatus::Completed);
    assert_eq!(terminal.response.usage, Some(usage));
}

#[tokio::test]
async fn provider_completed_without_usage_gets_unknown_incomplete_usage() {
    let events = run(vec![ev_completed(0, "p", None)], params()).await;

    assert_eq!(last(&events).response.usage, Some(Usage::unknown()));
}

#[tokio::test]
async fn provider_completed_without_usage_gets_the_last_seen_usage_marked_incomplete() {
    let events = run(
        vec![
            ev_in_progress(0, "p", Some(reported(3, 4))),
            ev_completed(1, "p", None),
        ],
        params(),
    )
    .await;

    let usage = last(&events).response.usage.clone().expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (3, 4));
    assert!(!usage.complete);
    assert!(!usage.estimated);
}

#[tokio::test]
async fn provider_incomplete_without_usage_gets_unknown_incomplete_usage() {
    let events = run(vec![ev_incomplete(0, "p")], params()).await;

    assert_eq!(last(&events).response.usage, Some(Usage::unknown()));
}

#[tokio::test]
async fn provider_failed_without_usage_gets_unknown_incomplete_usage() {
    let events = run(vec![ev_failed(0, "p", "provider_unavailable")], params()).await;

    assert_eq!(last(&events).response.usage, Some(Usage::unknown()));
}

#[tokio::test]
async fn provider_incomplete_and_failed_without_usage_use_the_last_seen_usage() {
    for terminal in [ev_incomplete(1, "p"), ev_failed(1, "p", "x")] {
        let events = run(
            vec![ev_in_progress(0, "p", Some(reported(5, 6))), terminal],
            params(),
        )
        .await;
        let usage = last(&events).response.usage.clone().expect("usage");
        assert_eq!((usage.input_tokens, usage.output_tokens), (5, 6));
        assert!(!usage.complete);
    }
}

#[tokio::test]
async fn provider_incomplete_is_relayed_as_the_terminal() {
    let events = run(vec![ev_incomplete(0, "p")], params()).await;

    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Incomplete(_))));
    assert_eq!(last(&events).response.id, ID);
}

#[tokio::test]
async fn provider_failed_is_relayed_as_the_only_terminal() {
    let events = run(vec![ev_failed(0, "p", "provider_unavailable")], params()).await;

    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let terminal = last(&events);
    assert_eq!(terminal.response.status, ResponseStatus::Failed);
    assert_eq!(
        terminal.response.error.as_ref().map(|e| e.code.as_str()),
        Some("provider_unavailable")
    );
}

#[tokio::test]
async fn events_after_the_terminal_are_dropped() {
    let events = run(
        vec![
            ev_completed(0, "p", None),
            ev_text_delta(1, "late"),
            ev_failed(2, "p", "provider_unavailable"),
            ev_completed(3, "p", None),
            ev_error(4, "provider_unavailable"),
        ],
        params(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn missing_terminal_is_synthesized_as_completed() {
    let events = run(vec![ev_text_delta(0, "partial")], params()).await;

    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    let terminal = last(&events);
    assert_eq!(terminal.response.id, ID);
    assert_eq!(terminal.response.status, ResponseStatus::Completed);
    assert_eq!(seq(events.last().expect("terminal")), 3);
}

#[tokio::test]
async fn empty_provider_stream_still_yields_created_in_progress_completed() {
    let events = run(Vec::new(), params()).await;

    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn synthesized_terminal_carries_unknown_usage_when_the_provider_gave_none() {
    let events = run(vec![ev_text_delta(0, "x")], params()).await;

    assert_eq!(last(&events).response.usage, Some(Usage::unknown()));
}

#[tokio::test]
async fn synthesized_terminal_carries_the_last_provider_usage_marked_incomplete() {
    let events = run(
        vec![
            ev_in_progress(0, "p", Some(reported(3, 4))),
            ev_in_progress(1, "p", Some(reported(10, 20))),
            ev_text_delta(2, "x"),
        ],
        params(),
    )
    .await;

    let usage = last(&events).response.usage.clone().expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 20));
    assert!(!usage.complete, "no final provider usage: incomplete");
    assert!(!usage.estimated, "estimation belongs to the Usage Reporter");
}

// ── provider error events ───────────────────────────────────────────────────

#[tokio::test]
async fn provider_error_event_becomes_a_single_failed_terminal_with_the_same_code() {
    let events = run(
        vec![ev_text_delta(0, "x"), ev_error(1, "provider_unavailable")],
        params(),
    )
    .await;

    assert!(
        !kinds(&events).iter().any(|k| k == "error"),
        "a standalone error event must never reach the consumer: {:?}",
        kinds(&events)
    );
    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let terminal = last(&events);
    assert_eq!(terminal.response.id, ID);
    assert_eq!(terminal.response.status, ResponseStatus::Failed);
    assert_eq!(
        terminal.response.error.as_ref().map(|e| e.code.as_str()),
        Some("provider_unavailable")
    );
}

#[tokio::test]
async fn failed_terminal_after_an_error_event_has_incomplete_usage_with_last_seen_numbers() {
    let events = run(
        vec![
            ev_in_progress(0, "p", Some(reported(7, 9))),
            ev_error(1, "rate_limited"),
        ],
        params(),
    )
    .await;

    let usage = last(&events).response.usage.clone().expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 9));
    assert!(!usage.complete);
    assert!(!usage.estimated);
}

#[tokio::test]
async fn failed_terminal_without_any_provider_usage_has_zero_incomplete_usage() {
    let events = run(vec![ev_error(0, "provider_unavailable")], params()).await;

    assert_eq!(last(&events).response.usage, Some(Usage::unknown()));
}

#[tokio::test]
async fn error_event_is_dropped_when_a_terminal_was_already_relayed() {
    let events = run(
        vec![
            ev_completed(0, "p", None),
            ev_error(1, "provider_unavailable"),
        ],
        params(),
    )
    .await;

    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

// ── ignored_parameters ──────────────────────────────────────────────────────

#[tokio::test]
async fn ignored_parameters_are_merged_sorted_and_deduplicated_on_the_terminal() {
    let mut terminal = ev_completed(0, "p", None);
    if let StreamingEvent::Completed(e) = &mut terminal {
        e.response.ignored_parameters = vec!["top_p".to_owned(), "seed".to_owned()];
    }
    let mut p = params();
    p.ignored_parameters = vec!["temperature".to_owned(), "seed".to_owned()];
    let events = run(vec![terminal], p).await;

    assert_eq!(
        last(&events).response.ignored_parameters,
        ["seed", "temperature", "top_p"]
    );
}

#[tokio::test]
async fn ignored_parameters_are_attached_to_a_synthesized_and_to_a_failed_terminal() {
    let mut p = params();
    p.ignored_parameters = vec!["top_p".to_owned()];
    let synthesized = run(Vec::new(), p).await;
    assert_eq!(last(&synthesized).response.ignored_parameters, ["top_p"]);

    let mut p = params();
    p.ignored_parameters = vec!["top_p".to_owned()];
    let failed = run(vec![ev_error(0, "provider_unavailable")], p).await;
    assert_eq!(last(&failed).response.ignored_parameters, ["top_p"]);
}

#[tokio::test]
async fn ignored_parameters_are_attached_to_a_provider_incomplete_terminal() {
    let mut terminal = ev_incomplete(0, "p");
    if let StreamingEvent::Incomplete(e) = &mut terminal {
        e.response.ignored_parameters = vec!["top_p".to_owned()];
    }
    let mut p = params();
    p.ignored_parameters = vec!["seed".to_owned(), "top_p".to_owned()];
    let events = run(vec![terminal], p).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Incomplete(_))));
    assert_eq!(last(&events).response.ignored_parameters, ["seed", "top_p"]);
}

// ── structured output ───────────────────────────────────────────────────────

fn person_schema() -> serde_json::Value {
    json!({ "type": "object", "properties": { "age": { "type": "integer" } }, "required": ["age"] })
}

#[tokio::test]
async fn valid_structured_output_streams_to_completed() {
    let mut p = params();
    p.output_schema = Some(person_schema());
    let events = run(
        vec![
            ev_text_delta(0, "{\"age\":"),
            ev_text_delta(1, "36}"),
            ev_completed(2, "p", None),
        ],
        p,
    )
    .await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(terminals(&events).len(), 1);
}

#[tokio::test]
async fn invalid_structured_output_ends_with_failed_output_validation_error() {
    let mut p = params();
    p.output_schema = Some(person_schema());
    let events = run(
        vec![
            ev_text_delta(0, "{\"age\":\"SENTINEL-old\"}"),
            ev_completed(1, "p", None),
        ],
        p,
    )
    .await;

    assert_eq!(terminals(&events).len(), 1);
    assert!(
        !kinds(&events).iter().any(|k| k == "response.completed"),
        "the invalid output must not complete: {:?}",
        kinds(&events)
    );
    let terminal = last(&events);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let error = terminal.response.error.as_ref().expect("error");
    assert_eq!(error.code, "output_validation_error");
    assert!(
        !error.message.contains("SENTINEL"),
        "content leaked: {}",
        error.message
    );
}

#[tokio::test]
async fn non_json_structured_output_fails_validation() {
    let mut p = params();
    p.output_schema = Some(person_schema());
    let events = run(
        vec![ev_text_delta(0, "not json"), ev_completed(1, "p", None)],
        p,
    )
    .await;

    assert_eq!(
        last(&events)
            .response
            .error
            .as_ref()
            .map(|e| e.code.as_str()),
        Some("output_validation_error")
    );
}

#[tokio::test]
async fn structured_output_is_validated_on_a_synthesized_terminal_too() {
    let mut p = params();
    p.output_schema = Some(person_schema());
    let events = run(vec![ev_text_delta(0, "{}")], p).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    assert_eq!(
        last(&events)
            .response
            .error
            .as_ref()
            .map(|e| e.code.as_str()),
        Some("output_validation_error")
    );
}

#[tokio::test]
async fn structured_output_is_not_validated_for_a_failed_terminal() {
    let mut p = params();
    p.output_schema = Some(person_schema());
    let events = run(vec![ev_error(0, "provider_unavailable")], p).await;

    assert_eq!(
        last(&events)
            .response
            .error
            .as_ref()
            .map(|e| e.code.as_str()),
        Some("provider_unavailable")
    );
}

#[tokio::test]
async fn without_a_schema_output_text_is_not_validated() {
    let events = run(
        vec![ev_text_delta(0, "not json"), ev_completed(1, "p", None)],
        params(),
    )
    .await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

// ── cancellation on consumer drop ───────────────────────────────────────────

/// Upstream that records its own drop.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct FlaggedPending {
    _flag: DropFlag,
}

impl futures_core::Stream for FlaggedPending {
    type Item = StreamingEvent;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

#[tokio::test]
async fn dropping_the_consumer_stream_cancels_the_request_token() {
    let p = params();
    let cancel = p.cancel.clone();
    let mut stream = supervise(hanging(vec![ev_text_delta(0, "x")]), p);
    let first = stream.next().await.expect("created");
    assert!(matches!(first, StreamingEvent::Created(_)));
    assert!(!cancel.is_cancelled());

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("the request token is cancelled after the consumer left");
}

#[tokio::test]
async fn dropping_the_consumer_stream_drops_the_upstream_stream() {
    let dropped = Arc::new(AtomicBool::new(false));
    let upstream: ResponseEventStream = Box::pin(FlaggedPending {
        _flag: DropFlag(Arc::clone(&dropped)),
    });
    let mut stream = supervise(upstream, params());
    let _ = stream.next().await.expect("created");

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the upstream stream is dropped after the consumer left");
}

#[tokio::test]
async fn dropping_the_stream_before_the_first_poll_still_cancels() {
    let p = params();
    let cancel = p.cancel.clone();
    let stream = supervise(hanging(Vec::new()), p);

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("the request token is cancelled");
}

#[tokio::test]
async fn a_normally_finished_stream_does_not_report_a_disconnect() {
    let p = params();
    let cancel = p.cancel.clone();
    let events = run(vec![ev_completed(0, "p", None)], p).await;

    assert_eq!(terminals(&events).len(), 1);
    // Give a (wrongly) spawned task a chance to cancel.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !cancel.is_cancelled(),
        "a stream that reached its terminal is not a client disconnect"
    );
}

// ── in-flight slot ──────────────────────────────────────────────────────────

#[tokio::test]
async fn slot_is_held_while_streaming_and_released_when_the_stream_ends() {
    let sem = Arc::new(Semaphore::new(1));
    let mut p = params();
    p.slot = Some(Arc::clone(&sem).try_acquire_owned().expect("slot"));
    let mut stream = supervise(hanging(Vec::new()), p);
    let _ = stream.next().await.expect("created");
    assert_eq!(sem.available_permits(), 0, "held mid-stream");

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), async {
        while sem.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the slot is released after the stream was dropped");
}

#[tokio::test]
async fn slot_is_released_after_the_terminal_without_dropping_the_stream() {
    let sem = Arc::new(Semaphore::new(1));
    let mut p = params();
    p.slot = Some(Arc::clone(&sem).try_acquire_owned().expect("slot"));
    let mut stream = supervise(upstream(vec![ev_completed(0, "p", None)]), p);
    while stream.next().await.is_some() {}

    tokio::time::timeout(Duration::from_secs(2), async {
        while sem.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the slot is released once the stream is exhausted");
}

// ── relay task death ────────────────────────────────────────────────────────

/// Yields `events`, then panics on the next poll.
struct PanicsAfter {
    events: std::vec::IntoIter<StreamingEvent>,
}

impl futures_core::Stream for PanicsAfter {
    type Item = StreamingEvent;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.events.next() {
            Some(event) => Poll::Ready(Some(event)),
            None => panic!("upstream stream panicked (test)"),
        }
    }
}

#[tokio::test]
async fn a_panicking_upstream_yields_one_internal_failed_cancels_and_releases_the_slot() {
    let sem = Arc::new(Semaphore::new(1));
    let mut p = params();
    p.slot = Some(Arc::clone(&sem).try_acquire_owned().expect("slot"));
    p.ignored_parameters = vec!["top_k".to_owned()];
    let cancel = p.cancel.clone();
    let stream: ResponseEventStream = Box::pin(PanicsAfter {
        events: vec![
            ev_in_progress(0, "p", Some(reported(1, 2))),
            ev_text_delta(1, "a"),
            ev_text_delta(2, "b"),
        ]
        .into_iter(),
    });

    let events = tokio::time::timeout(
        Duration::from_secs(2),
        supervise(stream, p).collect::<Vec<_>>(),
    )
    .await
    .expect("the supervised stream ends");

    assert_eq!(terminals(&events).len(), 1);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let terminal = last(&events);
    assert_eq!(terminal.response.id, ID);
    assert_eq!(terminal.response.status, ResponseStatus::Failed);
    assert_eq!(
        terminal.response.error.as_ref().map(|e| e.code.as_str()),
        Some("internal")
    );
    assert_eq!(terminal.response.ignored_parameters, vec!["top_k"]);
    let usage = terminal.response.usage.clone().expect("usage");
    assert!(!usage.complete);
    let numbers: Vec<u64> = events.iter().map(seq).collect();
    assert_eq!(numbers, (0..events.len() as u64).collect::<Vec<_>>());
    assert!(cancel.is_cancelled(), "the request token is cancelled");
    tokio::time::timeout(Duration::from_secs(2), async {
        while sem.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the slot is released");
}

#[tokio::test]
async fn a_normally_finished_stream_is_not_failed_nor_cancelled() {
    let p = params();
    let cancel = p.cancel.clone();
    let events = run(vec![ev_completed(0, "p", None)], p).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(terminals(&events).len(), 1);
    assert!(!cancel.is_cancelled());
}

// ── backpressure ────────────────────────────────────────────────────────────

struct Counting {
    remaining: usize,
    polled: Arc<AtomicUsize>,
}

impl futures_core::Stream for Counting {
    type Item = StreamingEvent;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        self.remaining -= 1;
        self.polled.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(ev_text_delta(0, "x")))
    }
}

#[tokio::test]
async fn a_slow_consumer_applies_backpressure_to_the_upstream() {
    let polled = Arc::new(AtomicUsize::new(0));
    let upstream: ResponseEventStream = Box::pin(Counting {
        remaining: 1_000,
        polled: Arc::clone(&polled),
    });
    let mut p = params();
    p.channel_capacity = 2;
    let mut stream = supervise(upstream, p);
    let _ = stream.next().await.expect("created");

    // Let the supervisor run as far as the bounded channel allows.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let drained = polled.load(Ordering::SeqCst);
    assert!(
        drained < 20,
        "upstream was drained ({drained} of 1000 events) despite a capacity-2 channel"
    );
    drop(stream);
}

#[tokio::test]
async fn a_slow_consumer_still_receives_every_event_in_order() {
    let polled = Arc::new(AtomicUsize::new(0));
    let upstream: ResponseEventStream = Box::pin(Counting {
        remaining: 50,
        polled,
    });
    let mut p = params();
    p.channel_capacity = 1;
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = supervise(upstream, p);
        let mut out = Vec::new();
        while let Some(event) = stream.next().await {
            tokio::task::yield_now().await;
            out.push(event);
        }
        out
    })
    .await
    .expect("stream ends");

    assert_eq!(events.len(), 2 + 50 + 1);
    let numbers: Vec<u64> = events.iter().map(seq).collect();
    assert_eq!(numbers, (0..53).collect::<Vec<_>>());
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

/// The relay sets the stop and drops the sender between the consumer's stop
/// check and its channel read: the closed-channel path must deliver the stop
/// terminal, not the "relay died" `internal` failure.
#[test]
fn a_stop_set_while_the_sender_closes_is_delivered_not_reported_as_a_dead_relay() {
    use super::{Relay, State, Stop, StopCell, StopReason, UsageBasis};
    use futures_core::Stream as _;
    use futures_util::task::noop_waker_ref;

    let params = params();
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamingEvent>(1);
    let stop = StopCell::default();
    let mut relay = Box::pin(Relay {
        rx,
        cancel: params.cancel.clone(),
        stop: stop.clone(),
        fallback: State::new(&params, UsageBasis::new(None)),
        received: 0,
        finished: false,
        ended: false,
    });
    let mut cx = Context::from_waker(noop_waker_ref());

    assert!(relay.as_mut().poll_next(&mut cx).is_pending());
    stop.set(Stop {
        reason: StopReason::Shutdown,
        usage: reported(1, 1),
    });
    drop(tx);
    // The closed-channel path itself (what a poll reaches when its stop check
    // ran just before the relay set the stop).
    let Some(StreamingEvent::Failed(e)) = relay.as_mut().get_mut().on_channel_closed() else {
        panic!("expected the stop terminal");
    };
    assert_eq!(e.response.error.expect("error").code, "gateway_unavailable");
    assert!(matches!(
        relay.as_mut().poll_next(&mut cx),
        Poll::Ready(None)
    ));
    assert!(!params.cancel.is_cancelled(), "no dead-relay cancel");
}

/// A closed channel with neither stop nor terminal is a dead relay.
#[test]
fn a_closed_channel_without_stop_or_terminal_is_still_a_dead_relay() {
    use super::{Relay, State, StopCell, UsageBasis};

    let params = params();
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamingEvent>(1);
    let mut relay = Relay {
        rx,
        cancel: params.cancel.clone(),
        stop: StopCell::default(),
        fallback: State::new(&params, UsageBasis::new(None)),
        received: 0,
        finished: false,
        ended: false,
    };
    drop(tx);
    let Some(StreamingEvent::Failed(e)) = relay.on_channel_closed() else {
        panic!("expected the internal failure");
    };
    assert_eq!(e.response.error.expect("error").code, "internal");
    assert!(params.cancel.is_cancelled());
}
