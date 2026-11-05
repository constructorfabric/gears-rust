//! Stream Supervisor terminal hook and incomplete-usage estimate (DESIGN
//! §3.2 Stream Supervisor / Usage Reporter table, `seq-streaming`,
//! `seq-stream-cancellation`): the hook is called exactly once at every
//! terminal path with the quantity and the `complete` / `estimated` flags.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::ResponseStatus;
use llm_gateway_sdk::{ResponseEventStream, StreamingEvent, Usage};
use rust_decimal::Decimal;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{StreamParams, TerminalHook, initial_snapshot, supervise};
use crate::domain::pricing::{Pricing, UsageEstimator};
use crate::domain::test_support::{
    body, ev_completed, ev_error, ev_failed, ev_in_progress, ev_incomplete, ev_text_delta,
};

const ID: &str = "resp_gateway";

fn dec(mantissa: i64, scale: u32) -> Decimal {
    Decimal::new(mantissa, scale)
}

/// Input 100 tokens at 1000 and output at 2000 micro-credits per 1K tokens.
fn estimator() -> UsageEstimator {
    UsageEstimator {
        input_tokens: 100,
        pricing: Pricing {
            input_per_1k_micro: 1000,
            output_per_1k_micro: 2000,
        },
    }
}

type Calls = Arc<Mutex<Vec<Usage>>>;

fn hook() -> (TerminalHook, Calls) {
    let calls: Calls = Arc::default();
    let sink = Arc::clone(&calls);
    (
        Box::new(move |usage, _provider_request_id| sink.lock().expect("calls").push(usage)),
        calls,
    )
}

fn params(estimator: Option<UsageEstimator>) -> (StreamParams, Calls) {
    let (on_terminal, calls) = hook();
    (
        StreamParams {
            response_id: ID.to_owned(),
            snapshot: initial_snapshot(ID, &body("openai::gpt-4o")),
            channel_capacity: 8,
            cancel: CancellationToken::new(),
            output_schema: None,
            ignored_parameters: Vec::new(),
            slot: None,
            estimator,
            on_terminal: Some(on_terminal),
        },
        calls,
    )
}

fn provider_usage(cost: Decimal) -> Usage {
    Usage::reported(100, 50, cost)
}

fn upstream(events: Vec<StreamingEvent>) -> ResponseEventStream {
    Box::pin(futures_util::stream::iter(events))
}

fn hanging(events: Vec<StreamingEvent>) -> ResponseEventStream {
    Box::pin(upstream(events).chain(futures_util::stream::pending()))
}

async fn run(events: Vec<StreamingEvent>, params: StreamParams) -> Vec<StreamingEvent> {
    tokio::time::timeout(
        Duration::from_secs(2),
        supervise(upstream(events), params).collect::<Vec<_>>(),
    )
    .await
    .expect("the supervised stream ends")
}

fn terminal_usage(events: &[StreamingEvent]) -> Usage {
    match events.last() {
        Some(
            StreamingEvent::Completed(e)
            | StreamingEvent::Incomplete(e)
            | StreamingEvent::Failed(e),
        ) => e.response.usage.clone().expect("terminal usage"),
        other => panic!("not a terminal: {other:?}"),
    }
}

/// Wait until the hook was called, then give a double call time to show up.
async fn hook_calls(calls: &Calls) -> Vec<Usage> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while calls.lock().expect("calls").is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the terminal hook was not called"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    calls.lock().expect("calls").clone()
}

/// Hook that records the provider request id it is given.
type RequestIds = Arc<Mutex<Vec<Option<String>>>>;

fn request_id_hook() -> (TerminalHook, RequestIds) {
    let seen: RequestIds = Arc::default();
    let sink = Arc::clone(&seen);
    (
        Box::new(move |_usage, id| sink.lock().expect("seen").push(id)),
        seen,
    )
}

#[tokio::test]
async fn the_hook_carries_the_provider_request_id_of_the_terminal_snapshot() {
    use llm_gateway_sdk::models::core::ProviderDetails;
    let (mut p, _calls) = params(Some(estimator()));
    let (hook, seen) = request_id_hook();
    p.on_terminal = Some(hook);
    let mut done = ev_completed(0, "p", Some(provider_usage(dec(5, 1))));
    if let StreamingEvent::Completed(e) = &mut done {
        e.response.provider_details =
            Some(ProviderDetails::default().with_provider_request_id("req_terminal"));
    }
    run(vec![done], p).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        *seen.lock().expect("seen"),
        [Some("req_terminal".to_owned())]
    );
}

#[tokio::test]
async fn the_hook_gets_no_provider_request_id_when_the_terminal_has_none() {
    let (mut p, _calls) = params(Some(estimator()));
    let (hook, seen) = request_id_hook();
    p.on_terminal = Some(hook);
    run(
        vec![ev_completed(0, "p", Some(provider_usage(dec(5, 1))))],
        p,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(*seen.lock().expect("seen"), [None]);
}

// ── provider terminals with usage ───────────────────────────────────────────

#[tokio::test]
async fn a_provider_completed_with_usage_calls_the_hook_once_with_that_usage() {
    let (p, calls) = params(Some(estimator()));
    let events = run(
        vec![ev_completed(0, "p", Some(provider_usage(dec(5, 1))))],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(5, 1));
    assert!(calls[0].complete);
    assert!(!calls[0].estimated);
    assert_eq!(
        terminal_usage(&events),
        calls[0],
        "the consumer sees the same usage"
    );
}

#[tokio::test]
async fn a_provider_incomplete_with_usage_is_final_usage() {
    // Incomplete by stop reason (e.g. output token limit): provider usage.
    let mut terminal = ev_incomplete(0, "p");
    if let StreamingEvent::Incomplete(e) = &mut terminal {
        e.response.usage = Some(provider_usage(dec(7, 4)));
    }
    let (p, calls) = params(Some(estimator()));
    run(vec![terminal], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(7, 4));
    assert!(calls[0].complete);
    assert!(!calls[0].estimated);
}

#[tokio::test]
async fn a_provider_failed_terminal_with_usage_settles_that_usage() {
    let mut terminal = ev_failed(0, "p", "provider_unavailable");
    if let StreamingEvent::Failed(e) = &mut terminal {
        e.response.usage = Some(provider_usage(dec(9, 5)));
    }
    let (p, calls) = params(Some(estimator()));
    run(vec![terminal], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(9, 5));
    assert!(calls[0].complete);
}

// ── provider error response: plugin cost or zero ────────────────────────────

#[tokio::test]
async fn a_provider_error_event_before_any_output_settles_zero_as_final_usage() {
    let (p, calls) = params(Some(estimator()));
    let events = run(vec![ev_error(0, "provider_unavailable")], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].cost,
        Decimal::ZERO,
        "a provider error carries no cost and nothing was relayed"
    );
    assert!(calls[0].complete, "a provider error response is final");
    assert!(!calls[0].estimated);
    assert_eq!(terminal_usage(&events), calls[0]);
}

#[tokio::test]
async fn a_provider_error_event_after_relayed_output_settles_the_estimate() {
    // 8 relayed bytes = 2 output tokens: (100 x 1000 + 2 x 2000) / 1000 = 104
    // price units = 0.000104 credit.
    let (p, calls) = params(Some(estimator()));
    let events = run(
        vec![
            ev_text_delta(0, "abcdefgh"),
            ev_error(1, "provider_unavailable"),
        ],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(104, 6));
    assert_eq!(calls[0].output_tokens, 2);
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
    assert_eq!(terminal_usage(&events), calls[0]);
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
}

#[tokio::test]
async fn a_provider_failed_terminal_without_usage_before_any_output_settles_zero() {
    let (p, calls) = params(Some(estimator()));
    run(vec![ev_failed(0, "p", "provider_unavailable")], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, Decimal::ZERO);
    assert!(calls[0].complete);
    assert!(!calls[0].estimated);
}

#[tokio::test]
async fn a_provider_failed_terminal_without_usage_after_relayed_output_settles_the_estimate() {
    let (p, calls) = params(Some(estimator()));
    run(
        vec![
            ev_text_delta(0, "abcdefgh"),
            ev_failed(1, "p", "provider_unavailable"),
        ],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(104, 6));
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
}

#[tokio::test]
async fn a_provider_error_after_only_a_partial_usage_and_no_relayed_output_settles_zero() {
    // Nothing reached the consumer: the partial usage alone does not make the
    // error an estimate.
    let (p, calls) = params(Some(estimator()));
    run(
        vec![
            ev_in_progress(0, "p", Some(provider_usage(dec(1, 1)))),
            ev_error(1, "provider_unavailable"),
        ],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls[0].cost, Decimal::ZERO);
    assert!(calls[0].complete);
    assert!(!calls[0].estimated);
}

// ── no final provider usage: the core estimate ──────────────────────────────

#[tokio::test]
async fn a_provider_terminal_without_usage_gets_the_estimate() {
    // 8 relayed bytes = 2 output tokens: (100 x 1000 + 2 x 2000) / 1000 = 104
    // price units = 0.000104 credit.
    let (p, calls) = params(Some(estimator()));
    let events = run(
        vec![ev_text_delta(0, "abcdefgh"), ev_completed(1, "p", None)],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(104, 6));
    assert_eq!(calls[0].input_tokens, 100);
    assert_eq!(calls[0].output_tokens, 2);
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
    assert_eq!(
        terminal_usage(&events),
        calls[0],
        "the terminal carries the estimate"
    );
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

#[tokio::test]
async fn an_incomplete_terminal_without_usage_gets_the_estimate() {
    let (p, calls) = params(Some(estimator()));
    run(vec![ev_incomplete(0, "p")], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(
        calls[0].cost,
        dec(100, 6),
        "input only: nothing was relayed"
    );
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
}

#[tokio::test]
async fn a_stream_that_ends_without_a_terminal_gets_the_estimate() {
    let (p, calls) = params(Some(estimator()));
    let events = run(vec![ev_text_delta(0, "abcdefgh")], p).await;

    assert!(
        matches!(events.last(), Some(StreamingEvent::Completed(_))),
        "synthesized completed"
    );
    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(104, 6));
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
    assert_eq!(terminal_usage(&events), calls[0]);
}

#[tokio::test]
async fn the_estimate_takes_the_last_partial_usage_when_it_is_higher_than_the_relayed_output() {
    // Partial usage of 50 output tokens beats 2 relayed tokens (8 bytes).
    let (p, calls) = params(Some(estimator()));
    run(
        vec![
            ev_in_progress(0, "p", Some(provider_usage(dec(1, 1)))),
            ev_text_delta(1, "abcdefgh"),
            ev_completed(2, "p", None),
        ],
        p,
    )
    .await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls[0].output_tokens, 50);
    // (100 x 1000 + 50 x 2000) / 1000 = 200 price units = 0.0002 credit.
    assert_eq!(calls[0].cost, dec(200, 6));
}

#[tokio::test]
async fn the_estimate_takes_the_relayed_output_when_it_is_higher_than_the_partial_usage() {
    let (p, calls) = params(Some(estimator()));
    let text = "a".repeat(400); // 100 tokens
    run(
        vec![
            ev_in_progress(0, "p", Some(provider_usage(dec(1, 1)))),
            ev_text_delta(1, &text),
            ev_completed(2, "p", None),
        ],
        p,
    )
    .await;
    assert_eq!(hook_calls(&calls).await[0].output_tokens, 100);
}

#[tokio::test]
async fn relayed_output_is_counted_over_every_text_delta_utf8_bytes() {
    let (p, calls) = params(Some(estimator()));
    run(
        vec![
            ev_text_delta(0, "\u{e9}\u{e9}"), // 4 bytes
            ev_text_delta(1, "abcd"),         // 4 bytes
            ev_completed(2, "p", None),
        ],
        p,
    )
    .await;
    assert_eq!(hook_calls(&calls).await[0].output_tokens, 2, "8 bytes");
}

#[tokio::test]
async fn zero_pricing_estimates_cost_nothing_but_stays_flagged() {
    let zero = UsageEstimator {
        input_tokens: 100,
        pricing: Pricing::default(),
    };
    let (p, calls) = params(Some(zero));
    run(vec![ev_completed(0, "p", None)], p).await;
    let calls = hook_calls(&calls).await;
    assert_eq!(calls[0].cost, Decimal::ZERO);
    assert!(calls[0].estimated);
}

#[tokio::test]
async fn without_an_estimator_there_is_no_estimate_and_the_quantity_is_zero() {
    // "No estimate can be made": 0 quantity, complete = false, estimated = false.
    let (p, calls) = params(None);
    run(vec![ev_completed(0, "p", None)], p).await;

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, Decimal::ZERO);
    assert!(!calls[0].complete);
    assert!(!calls[0].estimated);
}

#[tokio::test]
async fn a_failed_structured_output_validation_still_settles_the_provider_usage() {
    let (mut p, calls) = params(Some(estimator()));
    p.output_schema = Some(json!({ "type": "object", "required": ["age"] }));
    let events = run(
        vec![
            ev_text_delta(0, "not json"),
            ev_completed(1, "p", Some(provider_usage(dec(4, 4)))),
        ],
        p,
    )
    .await;

    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(4, 4), "the tokens were spent");
    assert!(calls[0].complete);
}

// ── relay death ─────────────────────────────────────────────────────────────

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
async fn a_relay_failure_calls_the_hook_once_with_the_estimate() {
    let (p, calls) = params(Some(estimator()));
    let stream: ResponseEventStream = Box::pin(PanicsAfter {
        events: vec![ev_text_delta(0, "abcdefgh")].into_iter(),
    });
    let events = tokio::time::timeout(
        Duration::from_secs(2),
        supervise(stream, p).collect::<Vec<_>>(),
    )
    .await
    .expect("the supervised stream ends");

    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1, "exactly once");
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
    assert_eq!(
        calls[0].output_tokens, 2,
        "what was relayed before the failure"
    );
    assert!(terminal_usage(&events).estimated);
}

// ── consumer disconnect ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_disconnect_mid_stream_calls_the_hook_once_with_the_estimate_of_what_was_relayed() {
    let (p, calls) = params(Some(estimator()));
    let cancel = p.cancel.clone();
    let mut stream = supervise(
        hanging(vec![ev_text_delta(0, "abcd"), ev_text_delta(1, "efgh")]),
        p,
    );
    for _ in 0..4 {
        // created, in_progress, two deltas
        stream.next().await.expect("event");
    }
    drop(stream);

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1, "exactly once");
    assert_eq!(calls[0].output_tokens, 2);
    assert_eq!(calls[0].cost, dec(104, 6));
    assert!(!calls[0].complete);
    assert!(calls[0].estimated);
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn a_disconnect_before_any_event_was_read_calls_the_hook_once() {
    let (p, calls) = params(Some(estimator()));
    let stream = supervise(hanging(Vec::new()), p);
    drop(stream);

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(100, 6), "input estimate only");
    assert!(calls[0].estimated);
}

#[tokio::test]
async fn dropping_the_stream_after_the_terminal_does_not_call_the_hook_again() {
    let (p, calls) = params(Some(estimator()));
    let mut stream = supervise(
        upstream(vec![ev_completed(0, "p", Some(provider_usage(dec(5, 1))))]),
        p,
    );
    while stream.next().await.is_some() {}
    drop(stream);

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].complete,
        "provider usage, not a disconnect estimate"
    );
    assert_eq!(calls[0].cost, dec(5, 1));
}

#[tokio::test]
async fn dropping_the_stream_with_the_terminal_still_buffered_is_not_a_disconnect() {
    // The relay task sent the terminal event into the channel before the
    // consumer left: the terminal was sent, the provider usage stands.
    let (p, calls) = params(Some(estimator()));
    let stream = supervise(
        upstream(vec![ev_completed(0, "p", Some(provider_usage(dec(5, 1))))]),
        p,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(stream);

    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cost, dec(5, 1));
    assert!(calls[0].complete);
    assert!(!calls[0].estimated);
}

#[tokio::test]
async fn a_cancelled_request_token_alone_is_not_a_consumer_disconnect() {
    // Disconnect detection follows "terminal sent", not the cancel token: the
    // token can be cancelled by the gateway itself (timeouts) while the
    // consumer is still attached and the provider still delivers.
    let (p, calls) = params(Some(estimator()));
    let cancel = p.cancel.clone();
    let mut stream = supervise(
        upstream(vec![
            ev_text_delta(0, "abcd"),
            ev_completed(1, "p", Some(provider_usage(dec(5, 1)))),
        ]),
        p,
    );
    cancel.cancel();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    let calls = hook_calls(&calls).await;
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].complete,
        "the provider's terminal usage, not an estimate"
    );
    assert_eq!(calls[0].cost, dec(5, 1));
}

#[tokio::test]
async fn every_path_calls_the_hook_exactly_once() {
    // Completed, incomplete, failed, error event, synthesized and relay
    // failure each produce exactly one call.
    let scripts: Vec<Vec<StreamingEvent>> = vec![
        vec![ev_completed(0, "p", Some(provider_usage(dec(1, 1))))],
        vec![ev_incomplete(0, "p")],
        vec![ev_failed(0, "p", "x")],
        vec![ev_error(0, "x")],
        vec![],
        vec![ev_completed(0, "p", None), ev_completed(1, "p", None)],
        vec![ev_completed(0, "p", None), ev_error(1, "x")],
    ];
    for script in scripts {
        let (p, calls) = params(Some(estimator()));
        let desc = format!("{script:?}");
        run(script, p).await;
        assert_eq!(hook_calls(&calls).await.len(), 1, "{desc}");
    }
}

#[tokio::test]
async fn a_stream_without_a_hook_behaves_as_before() {
    let (mut p, _calls) = params(Some(estimator()));
    p.on_terminal = None;
    let events = run(vec![ev_completed(0, "p", None)], p).await;
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(
        match events.last() {
            Some(StreamingEvent::Completed(e)) => e.response.status,
            _ => ResponseStatus::Failed,
        },
        ResponseStatus::Completed
    );
}
