//! Stream Supervisor bounds (DESIGN §3.2 Stream Supervisor, §4.5
//! `timeouts.stream_idle` / `timeouts.stream_max_duration`, §4.5 Graceful
//! shutdown): idle timeout, maximum duration and the shutdown cut each end the
//! stream with exactly one `response.failed`, incomplete estimate usage, one
//! terminal hook call, a cancelled request token and a dropped upstream.
//!
//! Paused tokio clock: waits advance virtual time, so deadlines are asserted
//! exactly.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::ResponseStatus;
use llm_gateway_sdk::{ResponseEventStream, StreamingEvent, Usage};
use rust_decimal::Decimal;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{StreamLimits, StreamParams, TerminalHook, initial_snapshot, supervise};
use crate::domain::pricing::{Pricing, UsageEstimator};
use crate::domain::test_support::{body, bounded, ev_completed, ev_in_progress, ev_text_delta};

const ID: &str = "resp_gateway";
const IDLE: Duration = Duration::from_secs(30);
const MAX: Duration = Duration::from_secs(100);

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
        Box::new(move |usage, _| sink.lock().expect("calls").push(usage)),
        calls,
    )
}

fn params(limits: StreamLimits) -> (StreamParams, Calls) {
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
            estimator: Some(estimator()),
            on_terminal: Some(on_terminal),
            limits,
        },
        calls,
    )
}

fn idle_limit() -> StreamLimits {
    StreamLimits {
        idle_timeout: Some(IDLE),
        ..StreamLimits::default()
    }
}

/// Upstream wrapper that records its own drop.
struct Tracked {
    inner: ResponseEventStream,
    dropped: Arc<AtomicBool>,
}

impl futures_core::Stream for Tracked {
    type Item = StreamingEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn tracked(inner: ResponseEventStream) -> (ResponseEventStream, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    (
        Box::pin(Tracked {
            inner,
            dropped: Arc::clone(&dropped),
        }),
        dropped,
    )
}

/// `events` immediately, then silence forever.
fn hanging(events: Vec<StreamingEvent>) -> ResponseEventStream {
    Box::pin(futures_util::stream::iter(events).chain(futures_util::stream::pending()))
}

/// One event per `gap` (the first one after a gap too), from `events`.
fn spaced(events: Vec<StreamingEvent>, gap: Duration) -> ResponseEventStream {
    Box::pin(futures_util::stream::unfold(
        events.into_iter(),
        move |mut rest| async move {
            let next = rest.next()?;
            tokio::time::sleep(gap).await;
            Some((next, rest))
        },
    ))
}

/// A text delta every `gap`, forever.
fn endless_deltas(gap: Duration) -> ResponseEventStream {
    Box::pin(futures_util::stream::unfold((), move |()| async move {
        tokio::time::sleep(gap).await;
        Some((ev_text_delta(0, "abcd"), ()))
    }))
}

/// Fast deltas, never ending, never waiting.
fn firehose() -> ResponseEventStream {
    Box::pin(futures_util::stream::repeat_with(|| {
        ev_text_delta(0, "abcd")
    }))
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

fn is_terminal(event: &StreamingEvent) -> bool {
    matches!(
        event,
        StreamingEvent::Completed(_) | StreamingEvent::Incomplete(_) | StreamingEvent::Failed(_)
    )
}

fn terminal_count(events: &[StreamingEvent]) -> usize {
    events.iter().filter(|e| is_terminal(e)).count()
}

/// The final `response.failed`: its error code and usage.
fn failed(events: &[StreamingEvent]) -> (String, Usage) {
    match events.last() {
        Some(StreamingEvent::Failed(e)) => {
            assert_eq!(e.response.status, ResponseStatus::Failed);
            assert_eq!(e.response.id, ID);
            (
                e.response
                    .error
                    .as_ref()
                    .expect("failed carries an error")
                    .code
                    .clone(),
                e.response.usage.clone().expect("terminal usage"),
            )
        }
        other => panic!("not response.failed: {other:?}"),
    }
}

fn assert_gapless(events: &[StreamingEvent]) {
    for (n, event) in events.iter().enumerate() {
        assert_eq!(seq(event), n as u64, "sequence_number of {}", kind(event));
    }
}

async fn collect(stream: ResponseEventStream) -> Vec<StreamingEvent> {
    // A generous real bound only to fail a hang; the clock is paused, so a
    // correct stream ends by virtual time.
    tokio::time::timeout(Duration::from_hours(24), stream.collect::<Vec<_>>())
        .await
        .expect("the supervised stream ends")
}

async fn settle_tasks() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

fn assert_incomplete_estimate(usage: &Usage, output_tokens: u64, micro: i64) {
    assert!(!usage.complete, "incomplete");
    assert!(usage.estimated, "an estimate");
    assert_eq!(usage.output_tokens, output_tokens);
    assert_eq!(usage.cost, dec(micro, 6));
}

// ── idle timeout ────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_silent_provider_ends_the_stream_with_one_failed_after_the_idle_timeout() {
    let (p, calls) = params(idle_limit());
    let cancel = p.cancel.clone();
    let (upstream, dropped) = tracked(hanging(vec![ev_text_delta(0, "abcd")]));
    let started = tokio::time::Instant::now();

    let events = collect(supervise(upstream, p)).await;

    assert_eq!(
        started.elapsed(),
        IDLE,
        "measured from the last provider event"
    );
    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.failed"
        ]
    );
    assert_eq!(terminal_count(&events), 1, "exactly one terminal");
    assert_gapless(&events);
    let (code, usage) = failed(&events);
    assert_eq!(code, "provider_unavailable");
    // 1 relayed output token: input 100 x 1000/1K + 1 x 2000/1K micro.
    assert_incomplete_estimate(&usage, 1, 102);

    settle_tasks().await;
    let calls = calls.lock().expect("calls").clone();
    assert_eq!(calls.len(), 1, "the terminal hook ran exactly once");
    assert_eq!(calls[0].cost, usage.cost, "settled with the terminal usage");
    assert!(cancel.is_cancelled(), "the request token is cancelled");
    assert!(dropped.load(Ordering::SeqCst), "the upstream is dropped");
    assert!(
        logs_contain(ID) && logs_contain("idle"),
        "logged with the response id and the reason"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stream_silent_from_the_start_ends_after_the_idle_timeout() {
    let (p, calls) = params(idle_limit());
    let started = tokio::time::Instant::now();

    let events = collect(supervise(hanging(Vec::new()), p)).await;

    assert_eq!(started.elapsed(), IDLE, "measured from the stream start");
    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.failed"
        ]
    );
    let (code, usage) = failed(&events);
    assert_eq!(code, "provider_unavailable");
    assert_incomplete_estimate(&usage, 0, 100);
    settle_tasks().await;
    assert_eq!(calls.lock().expect("calls").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn every_provider_event_restarts_the_idle_timer() {
    let (p, calls) = params(idle_limit());
    // 4 events 20 s apart: 80 s in total, never 30 s of silence.
    let upstream = spaced(
        vec![
            ev_text_delta(0, "ab"),
            ev_text_delta(1, "cd"),
            ev_text_delta(2, "ef"),
            ev_completed(3, "p", Some(Usage::reported(100, 3, Decimal::new(5, 1)))),
        ],
        Duration::from_secs(20),
    );

    let events = collect(supervise(upstream, p)).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(terminal_count(&events), 1);
    settle_tasks().await;
    let calls = calls.lock().expect("calls").clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].complete, "the provider usage, not an estimate");
}

#[tokio::test(start_paused = true)]
async fn provider_lifecycle_events_that_are_dropped_still_count_as_activity() {
    let (p, _calls) = params(idle_limit());
    let upstream = spaced(
        vec![
            ev_in_progress(0, "p", None),
            ev_in_progress(1, "p", None),
            ev_in_progress(2, "p", None),
            ev_completed(3, "p", Some(Usage::reported(100, 0, Decimal::ZERO))),
        ],
        Duration::from_secs(20),
    );

    let events = collect(supervise(upstream, p)).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

#[tokio::test(start_paused = true)]
async fn the_idle_timer_does_not_count_the_time_the_consumer_is_not_reading() {
    // Decision: the idle timer measures only the wait on the plugin stream.
    // The consumer pauses 100 s (over three idle timeouts) between reads while
    // the provider events are all immediately available.
    let mut limits = idle_limit();
    limits.max_duration = None;
    let (mut p, calls) = params(limits);
    p.channel_capacity = 1;
    let upstream: ResponseEventStream = Box::pin(futures_util::stream::iter(vec![
        ev_text_delta(0, "abcd"),
        ev_text_delta(1, "efgh"),
        ev_text_delta(2, "ijkl"),
        ev_completed(3, "p", Some(Usage::reported(100, 3, Decimal::new(5, 1)))),
    ]));
    let mut stream = supervise(upstream, p);

    let mut events = Vec::new();
    bounded(async {
        while let Some(event) = stream.next().await {
            events.push(event);
            tokio::time::sleep(Duration::from_secs(100)).await;
        }
    })
    .await;

    assert!(
        matches!(events.last(), Some(StreamingEvent::Completed(_))),
        "a slow consumer is not a provider failure: {:?}",
        kinds(&events)
    );
    assert_eq!(terminal_count(&events), 1);
    settle_tasks().await;
    assert!(calls.lock().expect("calls")[0].complete);
}

// ── maximum duration ────────────────────────────────────────────────────────

fn max_limit() -> StreamLimits {
    StreamLimits {
        idle_timeout: Some(IDLE),
        max_duration: Some(Duration::from_secs(45)),
        shutdown_cut: None,
    }
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_stream_over_the_maximum_duration_ends_with_one_failed_even_while_events_flow() {
    let (p, calls) = params(max_limit());
    let cancel = p.cancel.clone();
    let (upstream, dropped) = tracked(endless_deltas(Duration::from_secs(10)));
    let started = tokio::time::Instant::now();

    let events = collect(supervise(upstream, p)).await;

    assert_eq!(
        started.elapsed(),
        Duration::from_secs(45),
        "from the stream start"
    );
    // Deltas at 10, 20, 30 and 40 s were relayed.
    assert_eq!(terminal_count(&events), 1, "exactly one terminal");
    assert_eq!(
        kinds(&events)
            .iter()
            .filter(|k| k.contains("delta"))
            .count(),
        4
    );
    assert_gapless(&events);
    let (code, usage) = failed(&events);
    assert_eq!(code, "provider_unavailable");
    // 4 output tokens: input 100 + 4 x 2.
    assert_incomplete_estimate(&usage, 4, 108);

    settle_tasks().await;
    assert_eq!(calls.lock().expect("calls").len(), 1, "hook exactly once");
    assert!(cancel.is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        logs_contain(ID) && logs_contain("max_duration"),
        "logged with the response id and the reason"
    );
}

#[tokio::test(start_paused = true)]
async fn the_maximum_duration_also_bounds_a_stream_that_never_got_an_event() {
    let mut limits = max_limit();
    limits.idle_timeout = None;
    let (p, _calls) = params(limits);
    let started = tokio::time::Instant::now();

    let events = collect(supervise(hanging(Vec::new()), p)).await;

    assert_eq!(started.elapsed(), Duration::from_secs(45));
    assert_eq!(failed(&events).0, "provider_unavailable");
}

#[tokio::test(start_paused = true)]
async fn a_stream_completing_before_the_maximum_duration_is_unaffected() {
    let (p, calls) = params(max_limit());
    let upstream = spaced(
        vec![
            ev_text_delta(0, "abcd"),
            ev_completed(1, "p", Some(Usage::reported(100, 1, Decimal::new(5, 1)))),
        ],
        Duration::from_secs(20),
    );

    let events = collect(supervise(upstream, p)).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    settle_tasks().await;
    assert!(calls.lock().expect("calls")[0].complete);
}

#[tokio::test(start_paused = true)]
async fn the_maximum_duration_counts_consumer_time_and_ends_a_stream_with_a_stalled_consumer() {
    let (mut p, calls) = params(max_limit());
    p.channel_capacity = 1;
    let cancel = p.cancel.clone();
    let (upstream, dropped) = tracked(firehose());
    let mut stream = supervise(upstream, p);
    let started = tokio::time::Instant::now();

    // The consumer reads one event every 20 s: the relay sits blocked on the
    // full channel when the 45 s maximum passes.
    let mut events = Vec::new();
    bounded(async {
        while let Some(event) = stream.next().await {
            events.push(event);
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    })
    .await;

    assert!(
        started.elapsed() <= Duration::from_secs(80),
        "the stream does not outlive the maximum by more than one read: {:?}",
        started.elapsed()
    );
    assert_eq!(terminal_count(&events), 1);
    assert_eq!(failed(&events).0, "provider_unavailable");
    settle_tasks().await;
    assert_eq!(calls.lock().expect("calls").len(), 1);
    assert!(cancel.is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
}

// ── shutdown cut ────────────────────────────────────────────────────────────

fn cut_limit(cut: &CancellationToken) -> StreamLimits {
    StreamLimits {
        idle_timeout: Some(IDLE),
        max_duration: Some(MAX),
        shutdown_cut: Some(cut.clone()),
    }
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn the_shutdown_cut_ends_an_open_stream_with_failed_gateway_unavailable() {
    let cut = CancellationToken::new();
    let (p, calls) = params(cut_limit(&cut));
    let cancel = p.cancel.clone();
    let (upstream, dropped) = tracked(hanging(vec![ev_text_delta(0, "abcd")]));
    let trigger = cut.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(7)).await;
        trigger.cancel();
    });
    let started = tokio::time::Instant::now();

    let events = collect(supervise(upstream, p)).await;

    assert_eq!(started.elapsed(), Duration::from_secs(7), "at the cut");
    assert_eq!(terminal_count(&events), 1, "exactly one terminal");
    assert_gapless(&events);
    let (code, usage) = failed(&events);
    assert_eq!(code, "gateway_unavailable");
    assert_incomplete_estimate(&usage, 1, 102);

    settle_tasks().await;
    let calls = calls.lock().expect("calls").clone();
    assert_eq!(calls.len(), 1, "settled exactly once");
    assert_eq!(calls[0].cost, usage.cost);
    assert!(cancel.is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        logs_contain(ID) && logs_contain("shutdown"),
        "logged with the response id and the reason"
    );
}

#[tokio::test(start_paused = true)]
async fn the_shutdown_cut_also_ends_a_stream_whose_provider_keeps_talking() {
    let cut = CancellationToken::new();
    let (p, calls) = params(cut_limit(&cut));
    let trigger = cut.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(25)).await;
        trigger.cancel();
    });

    let events = collect(supervise(endless_deltas(Duration::from_secs(10)), p)).await;

    assert_eq!(failed(&events).0, "gateway_unavailable");
    assert_eq!(terminal_count(&events), 1);
    settle_tasks().await;
    assert_eq!(calls.lock().expect("calls").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_stream_finishing_before_the_cut_is_unaffected_by_a_later_cut() {
    let cut = CancellationToken::new();
    let (p, calls) = params(cut_limit(&cut));
    let events = collect(supervise(
        spaced(
            vec![ev_completed(
                0,
                "p",
                Some(Usage::reported(100, 1, Decimal::new(5, 1))),
            )],
            Duration::from_secs(1),
        ),
        p,
    ))
    .await;
    cut.cancel();
    settle_tasks().await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert_eq!(calls.lock().expect("calls").len(), 1, "no second settle");
}

// ── terminal send bounded by the hard stops ─────────────────────────────────

/// Provider-final usage of the pending terminal.
fn provider_usage() -> Usage {
    Usage::reported(100, 1, Decimal::new(5, 1))
}

/// Which hard bound ends a stalled terminal send.
#[derive(Clone, Copy)]
enum Bound {
    Cut,
    MaxDuration,
}

/// A stream whose consumer never reads: capacity 3 holds created,
/// `in_progress` and one delta, so the terminal (the provider's `completed`
/// with `terminal_usage`, or the synthesized one when `None`) waits on the full
/// channel. After the bound the relay must have settled once, released the
/// slot, cancelled the token and dropped the upstream; the consumer then reads
/// exactly one stop terminal.
async fn stalled_terminal(
    bound: Bound,
    terminal_usage: Option<Usage>,
) -> (Vec<StreamingEvent>, Usage) {
    let cut = CancellationToken::new();
    let limits = match bound {
        Bound::Cut => cut_limit(&cut),
        Bound::MaxDuration => max_limit(),
    };
    let slots = Arc::new(Semaphore::new(1));
    let (mut p, calls) = params(limits);
    p.channel_capacity = 3;
    p.slot = Some(Arc::clone(&slots).try_acquire_owned().expect("slot"));
    let cancel = p.cancel.clone();
    let mut events = vec![ev_text_delta(0, "abcd")];
    if let Some(usage) = terminal_usage {
        events.push(ev_completed(0, "p", Some(usage)));
    }
    let (upstream, dropped) = tracked(Box::pin(futures_util::stream::iter(events)));
    let stream = supervise(upstream, p);

    // The consumer does not read; the relay blocks on the terminal.
    settle_tasks().await;
    assert!(!cancel.is_cancelled(), "still waiting before the bound");
    assert!(calls.lock().expect("calls").is_empty(), "not settled yet");
    let wait = match bound {
        Bound::Cut => {
            cut.cancel();
            Duration::ZERO
        }
        Bound::MaxDuration => Duration::from_secs(100),
    };
    tokio::time::sleep(wait + Duration::from_millis(10)).await;

    assert!(
        cancel.is_cancelled(),
        "request token cancelled at the bound"
    );
    assert!(dropped.load(Ordering::SeqCst), "upstream dropped");
    assert_eq!(slots.available_permits(), 1, "slot released");
    let settled = {
        let calls = calls.lock().expect("calls");
        assert_eq!(calls.len(), 1, "settled exactly once");
        calls[0].clone()
    };

    let events = collect(stream).await;
    settle_tasks().await;
    assert_eq!(calls.lock().expect("calls").len(), 1, "still once");
    (events, settled)
}

#[tokio::test(start_paused = true)]
async fn the_shutdown_cut_ends_a_provider_terminal_stuck_on_a_full_channel() {
    let (events, settled) = stalled_terminal(Bound::Cut, Some(provider_usage())).await;

    assert_eq!(terminal_count(&events), 1);
    assert_eq!(failed(&events).0, "gateway_unavailable");
    assert_eq!(settled.cost, provider_usage().cost, "provider usage");
    assert!(settled.complete && !settled.estimated);
}

#[tokio::test(start_paused = true)]
async fn the_maximum_duration_ends_a_provider_terminal_stuck_on_a_full_channel() {
    let (events, settled) = stalled_terminal(Bound::MaxDuration, Some(provider_usage())).await;

    assert_eq!(terminal_count(&events), 1);
    assert_eq!(failed(&events).0, "provider_unavailable");
    assert_eq!(settled.cost, provider_usage().cost, "provider usage");
    assert!(settled.complete && !settled.estimated);
}

#[tokio::test(start_paused = true)]
async fn the_shutdown_cut_ends_a_synthesized_terminal_stuck_on_a_full_channel() {
    let (events, settled) = stalled_terminal(Bound::Cut, None).await;

    assert_eq!(terminal_count(&events), 1);
    assert_eq!(failed(&events).0, "gateway_unavailable");
    assert_incomplete_estimate(&settled, 1, 102);
}

#[tokio::test(start_paused = true)]
async fn the_maximum_duration_ends_a_synthesized_terminal_stuck_on_a_full_channel() {
    let (events, settled) = stalled_terminal(Bound::MaxDuration, None).await;

    assert_eq!(terminal_count(&events), 1);
    assert_eq!(failed(&events).0, "provider_unavailable");
    assert_incomplete_estimate(&settled, 1, 102);
}

// ── exactly-once and resources ──────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_timeout_releases_the_in_flight_slot() {
    let slots = Arc::new(Semaphore::new(1));
    let (mut p, _calls) = params(idle_limit());
    p.slot = Some(Arc::clone(&slots).try_acquire_owned().expect("slot"));

    let _events = collect(supervise(hanging(Vec::new()), p)).await;
    settle_tasks().await;

    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_consumer_disconnect_after_the_timeout_terminal_does_not_settle_again() {
    let (p, calls) = params(idle_limit());
    let mut stream = supervise(hanging(Vec::new()), p);
    bounded(async {
        while let Some(event) = stream.next().await {
            if is_terminal(&event) {
                break;
            }
        }
    })
    .await;
    drop(stream);
    settle_tasks().await;

    assert_eq!(calls.lock().expect("calls").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn without_limits_a_silent_stream_is_never_cut() {
    let (p, calls) = params(StreamLimits::default());
    let outcome = tokio::time::timeout(
        Duration::from_hours(24),
        supervise(hanging(Vec::new()), p).collect::<Vec<_>>(),
    )
    .await;

    assert!(outcome.is_err(), "no idle or maximum bound by default");
    drop(calls);
}
