//! Slice 5a streaming tests: pricing pre-check, admission and lease TTL for
//! streams, and settle at every stream terminal state — completed,
//! incomplete, provider error, consumer disconnect, relay failure — with one
//! hand-off to the usage sink per stream (DESIGN §3.2 Stream Supervisor /
//! Quota Guard, `seq-streaming`, `seq-stream-cancellation`).

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::{
    LlmGatewayError, ProviderErrorCategory, ProviderFailure, ResponseEventStream, StreamingEvent,
    Usage,
};
use rust_decimal::Decimal;
use secrecy::SecretString;
use uuid::Uuid;

use crate::domain::test_support::{
    FakeBroker, FakeQuota, Harness, SERVICE_SUBJECT, StreamScript, body, ctx, denied, ev_completed,
    ev_error, ev_in_progress, ev_incomplete, ev_summary_delta, ev_text_delta, eventually,
    eventually_within, qe_errors, remove_cost, set_cost,
};

const MODEL: &str = "openai::gpt-4o";
/// "hello" is 2 input tokens, the model output limit is 4096 tokens:
/// (2 x 1000 + 4096 x 2000) / 1000.
const RESERVED: i64 = 8194;

fn dec(mantissa: i64, scale: u32) -> Decimal {
    Decimal::new(mantissa, scale)
}

fn usage(cost: Decimal) -> Usage {
    Usage::reported(100, 50, cost)
}

/// Streaming harness priced 1000 / 2000 whose plugin streams `events`.
fn streaming(events: Vec<StreamingEvent>) -> Harness {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin.set_stream(StreamScript::Events(events));
    h
}

async fn start(h: &Harness) -> ResponseEventStream {
    h.service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts")
}

async fn collect(stream: ResponseEventStream) -> Vec<StreamingEvent> {
    tokio::time::timeout(Duration::from_secs(2), stream.collect::<Vec<_>>())
        .await
        .expect("the stream ends")
}

/// Let a double settle or double report show up before counting.
async fn settled(h: &Harness) {
    eventually("usage report", || !h.sink.reports().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// ── pre-stream: pricing and admission ───────────────────────────────────────

#[tokio::test]
async fn a_streaming_model_without_the_required_pricing_is_internal_before_any_lease_or_key() {
    for patch in [
        (|v: &mut serde_json::Value| set_cost(v, Some(1000), None)) as fn(&mut serde_json::Value),
        remove_cost,
    ] {
        let mut h = streaming(vec![ev_completed(0, "p", None)]);
        h.enable_streaming_with(patch);
        let failure = h
            .service()
            .create_response_stream_traced(&ctx(), body(MODEL))
            .await
            .err()
            .expect("fail-closed");
        assert_eq!(failure.error.code(), "internal");
        assert_eq!(h.broker.calls(), 0);
        assert!(h.quota.calls().is_empty());
        assert_eq!(h.plugin.stream_calls(), 0);
    }
}

#[tokio::test]
async fn the_stream_lease_ttl_covers_the_maximum_stream_duration() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(dec(1, 4))))]);
    collect(start(&h).await).await;

    let acquires = h.quota.acquires();
    assert_eq!(acquires.len(), 1);
    assert_eq!(acquires[0].1.amount, RESERVED);
    assert_eq!(
        acquires[0].1.ttl_secs,
        Some((h.timeouts.stream_max_duration + h.quota_cfg.lease_ttl_margin).as_secs())
    );
    assert_ne!(
        h.timeouts.stream_max_duration, h.timeouts.provider_sync,
        "the stream TTL is derived from its own duration"
    );
}

#[tokio::test]
async fn a_denied_stream_lease_is_quota_exceeded_before_the_stream_starts() {
    let h = streaming(vec![ev_completed(0, "p", None)]);
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let failure = h
        .service()
        .create_response_stream_traced(&ctx(), body(MODEL))
        .await
        .err()
        .expect("denied");
    assert_eq!(failure.error.code(), "quota_exceeded");
    assert_eq!(h.plugin.stream_calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn stream_admission_errors_map_to_their_codes_without_a_provider_call() {
    for (err, expected) in [
        (qe_errors::unavailable(), "quota_check_unavailable"),
        (
            qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
            "quota_check_unavailable",
        ),
        (qe_errors::permission_denied(), "permission_denied"),
        (qe_errors::invalid_argument("TTL_OUT_OF_BOUNDS"), "internal"),
    ] {
        let h = streaming(vec![ev_completed(0, "p", None)]);
        h.quota.script_acquire([Err(err)]);
        let failure = h
            .service()
            .create_response_stream_traced(&ctx(), body(MODEL))
            .await
            .err()
            .expect("admission fails");
        assert_eq!(failure.error.code(), expected);
        assert_eq!(h.plugin.stream_calls(), 0);
    }
}

#[tokio::test]
async fn a_credential_failure_before_the_stream_releases_the_lease() {
    let mut h = streaming(vec![ev_completed(0, "p", None)]);
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("credentials fail");

    eventually("release", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.stream_calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn a_plugin_start_error_releases_the_lease_and_reports_zero() {
    let h = streaming(Vec::new());
    h.plugin.set_stream(StreamScript::StartError(|| {
        LlmGatewayError::provider(
            ProviderErrorCategory::ProviderUnavailable,
            "provider down",
            ProviderFailure::default(),
        )
    }));
    h.service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("start fails");

    eventually("release", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty());
    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 0);
}

#[tokio::test]
async fn zero_pricing_streams_acquire_no_lease_and_do_no_settle() {
    let mut h = Harness::new();
    h.enable_streaming(); // zero rates
    h.plugin.set_stream(StreamScript::Events(vec![ev_completed(
        0,
        "p",
        Some(usage(dec(3, 4))),
    )]));
    collect(start(&h).await).await;

    settled(&h).await;
    assert!(h.quota.calls().is_empty());
    assert_eq!(h.sink.reports().len(), 1);
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 300);
}

#[tokio::test]
async fn no_applicable_quota_streams_report_without_settle() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(dec(3, 4))))]);
    h.quota.script_acquire([Ok(denied("NO_APPLICABLE_QUOTA"))]);
    collect(start(&h).await).await;
    settled(&h).await;
    assert_eq!(h.quota.settle_calls(), 0);
}

// ── settle at stream terminal states ────────────────────────────────────────

#[tokio::test]
async fn a_completed_stream_commits_the_terminal_cost_and_reports_once_after_the_commit() {
    let h = streaming(vec![
        ev_text_delta(0, "hi"),
        ev_completed(1, "p", Some(usage(dec(4, 4)))),
    ]);
    collect(start(&h).await).await;

    settled(&h).await;
    assert_eq!(h.quota.commits().len(), 1);
    assert!(h.quota.releases().is_empty());
    let (subject, commit) = &h.quota.commits()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(commit.actual_amount, Some(400));
    assert_eq!(commit.token, FakeQuota::token(0));

    assert_eq!(h.sink.reports().len(), 1);
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 400);
    assert!(report.usage.complete);
    assert!(!report.usage.estimated);
    assert_eq!(report.response_id, h.plugin.contexts()[0].request_id);
    let order: Vec<&str> = h
        .log
        .entries()
        .into_iter()
        .filter(|e| matches!(*e, "quota_commit" | "usage_sink"))
        .collect();
    assert_eq!(order, ["quota_commit", "usage_sink"]);
}

#[tokio::test]
async fn a_zero_terminal_cost_releases_the_stream_lease() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(Decimal::ZERO)))]);
    collect(start(&h).await).await;
    settled(&h).await;
    assert_eq!(h.quota.releases().len(), 1);
    assert!(h.quota.commits().is_empty());
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_stream_cost_above_the_reservation_commits_the_reservation_and_logs_the_overrun() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(dec(1, 2))))]);
    collect(start(&h).await).await;
    settled(&h).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(RESERVED));
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 10_000);
    assert!(logs_contain("overrun"));
}

#[tokio::test]
async fn an_incomplete_by_stop_reason_stream_settles_the_provider_usage() {
    let mut terminal = ev_incomplete(0, "p");
    if let StreamingEvent::Incomplete(e) = &mut terminal {
        e.response.usage = Some(usage(dec(4, 4)));
    }
    let h = streaming(vec![terminal]);
    collect(start(&h).await).await;
    settled(&h).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(400));
    assert!(h.sink.reports()[0].usage.complete);
}

#[tokio::test]
async fn a_stream_that_ends_without_a_terminal_settles_the_core_estimate() {
    // 2 input tokens + 2 relayed output tokens ("abcdefgh"):
    // (2 x 1000 + 2 x 2000) / 1000 = 6 micro-credits.
    let h = streaming(vec![ev_text_delta(0, "abcdefgh")]);
    collect(start(&h).await).await;

    settled(&h).await;
    assert_eq!(h.quota.commits().len(), 1);
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 6);
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
}

#[tokio::test]
async fn a_provider_error_event_before_any_output_releases_the_lease_and_reports_final_zero_usage()
{
    let h = streaming(vec![ev_error(0, "provider_unavailable")]);
    collect(start(&h).await).await;

    settled(&h).await;
    assert_eq!(h.quota.releases().len(), 1);
    assert!(h.quota.commits().is_empty());
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 0);
    assert!(report.usage.complete);
    assert!(!report.usage.estimated);
}

#[tokio::test]
async fn a_provider_error_event_after_relayed_output_settles_the_estimate() {
    // The output already reached the consumer: 2 input tokens + 2 relayed
    // output tokens ("abcdefgh") = 6 micro-credits, flagged as an estimate.
    let h = streaming(vec![
        ev_text_delta(0, "abcdefgh"),
        ev_error(1, "provider_unavailable"),
    ]);
    collect(start(&h).await).await;

    settled(&h).await;
    assert!(h.quota.releases().is_empty());
    assert_eq!(h.quota.commits().len(), 1);
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 6);
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
}

#[tokio::test]
async fn a_consumer_disconnect_settles_the_estimate_of_what_was_relayed_once() {
    let h = streaming(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_text_delta(
            0, "abcdefgh",
        )]));
    let mut stream = start(&h).await;
    for _ in 0..3 {
        stream.next().await.expect("created, in_progress, delta");
    }
    drop(stream);

    settled(&h).await;
    assert_eq!(h.quota.commits().len(), 1, "settled exactly once");
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    assert_eq!(h.sink.reports().len(), 1);
    let report = &h.sink.reports()[0];
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
    h.plugin.wait_stream_dropped().await;
}

#[tokio::test]
async fn a_disconnect_after_only_summary_deltas_counts_them_in_the_estimate() {
    let h = streaming(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_summary_delta(
            0, "abcdefgh",
        )]));
    let mut stream = start(&h).await;
    for _ in 0..3 {
        stream.next().await.expect("created, in_progress, delta");
    }
    drop(stream);

    settled(&h).await;
    // 2 input tokens + 2 summary output tokens = 6 micro-credits.
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    assert!(h.sink.reports()[0].usage.estimated);
}

#[tokio::test]
async fn a_provider_error_after_only_summary_deltas_settles_the_estimate_not_zero() {
    let h = streaming(vec![
        ev_summary_delta(0, "abcdefgh"),
        ev_error(1, "provider_unavailable"),
    ]);
    collect(start(&h).await).await;

    settled(&h).await;
    assert!(h.quota.releases().is_empty());
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    assert!(h.sink.reports()[0].usage.estimated);
}

#[tokio::test]
async fn a_disconnect_before_any_output_settles_the_input_only_estimate() {
    let h = streaming(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(Vec::new()));
    let stream = start(&h).await;
    drop(stream);

    settled(&h).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    assert_eq!(h.sink.reports().len(), 1);
}

#[tokio::test]
async fn a_disconnect_with_zero_priced_input_and_output_would_release() {
    // Mixed rates: input free, nothing relayed -> estimate 0 -> release.
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(0), Some(2000)));
    h.plugin
        .set_stream(StreamScript::EventsThenHang(Vec::new()));
    let stream = start(&h).await;
    drop(stream);

    settled(&h).await;
    assert_eq!(h.quota.releases().len(), 1);
    assert!(h.quota.commits().is_empty());
}

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
async fn a_relay_failure_settles_the_estimate_once() {
    // The fake plugin cannot script a panicking stream, so drive the
    // supervisor the way the service does.
    use crate::domain::pricing::{Pricing, UsageEstimator};
    use crate::domain::quota_guard::{Operation, Quote, RequestMeta};
    use crate::domain::stream_supervisor::{StreamParams, initial_snapshot, supervise};

    let h = streaming(Vec::new());
    let reservation = Quote {
        pricing: Pricing {
            input_per_1k_micro: 1000,
            output_per_1k_micro: 2000,
        },
        input_tokens: 2,
        reserved_units: RESERVED,
    };
    let meta = RequestMeta {
        response_id: "resp_relay".to_owned(),
        model: MODEL.to_owned(),
        provider_type: crate::domain::test_support::OPENAI_TYPE.to_owned(),
    };
    let mut quota = h
        .quota_guard()
        .admit(&ctx(), meta, Operation::Stream, &reservation)
        .await
        .expect("admitted");
    quota.mark_dispatched();
    let estimator: UsageEstimator = quota.estimator();
    let upstream: ResponseEventStream = Box::pin(PanicsAfter {
        events: vec![ev_in_progress(0, "p", None), ev_text_delta(1, "abcdefgh")].into_iter(),
    });
    let stream = supervise(
        upstream,
        StreamParams {
            response_id: "resp_relay".to_owned(),
            snapshot: initial_snapshot("resp_relay", &body(MODEL)),
            channel_capacity: 8,
            cancel: tokio_util::sync::CancellationToken::new(),
            output_schema: None,
            ignored_parameters: Vec::new(),
            slot: None,
            estimator: Some(estimator),
            on_terminal: Some(Box::new(move |usage| quota.settle(usage))),
        },
    );
    let events = collect(stream).await;
    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));

    settled(&h).await;
    assert_eq!(h.quota.commits().len(), 1, "settled exactly once");
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    assert!(h.sink.reports()[0].usage.estimated);
}

#[tokio::test]
async fn one_stream_settles_and_reports_exactly_once_even_when_dropped_after_the_end() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(dec(4, 4))))]);
    let mut stream = start(&h).await;
    while stream.next().await.is_some() {}
    drop(stream);

    settled(&h).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.quota.settle_calls(), 1);
    assert_eq!(h.sink.reports().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_stream_is_never_delayed_by_settle() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage(dec(4, 4))))]);
    h.quota
        .script_commit((0..100).map(|_| Err(qe_errors::unavailable())));
    let events = collect(start(&h).await).await;
    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
    assert!(
        h.sink.reports().is_empty(),
        "the usage waits for the settle"
    );

    eventually_within(Duration::from_secs(60), "retried settle", || {
        h.quota.commits().len() >= 3
    })
    .await;
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn a_secret_key_reaches_the_plugin_while_the_lease_is_held() {
    // Admission and credentials both succeeded: dispatch has the key.
    let mut h = streaming(vec![ev_completed(0, "p", Some(usage(dec(1, 4))))]);
    h.broker = FakeBroker::with(|| Ok(Some(SecretString::from("sk-stream"))), h.log.clone());
    collect(start(&h).await).await;
    assert!(h.plugin.contexts()[0].api_key.is_some());
    assert_eq!(h.quota.acquires().len(), 1);
}
