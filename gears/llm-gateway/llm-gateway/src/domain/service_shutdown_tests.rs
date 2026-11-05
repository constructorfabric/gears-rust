//! Graceful shutdown cut of the pipeline (DESIGN §4.5 Graceful shutdown, §4.1
//! `gateway_unavailable`, §3.2 Quota Guard settle "cut at shutdown", §1.2
//! availability row): after the stop signal new requests are rejected; calls
//! and streams in flight continue until the drain deadline; at the deadline
//! open streams end with `response.failed` (`gateway_unavailable`) and sync /
//! embeddings calls return `gateway_unavailable`, their leases are settled
//! with the estimate, and only then the Usage Reporter drains.
//!
//! Paused tokio clock. The harness signal cuts `ShutdownConfig::cut_after`
//! (20 s with the defaults) after `begin`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingRequest, LlmGatewayError, ResponseEventStream, StreamingEvent, Usage,
};
use rust_decimal::Decimal;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::config::ShutdownConfig;
use crate::domain::Service;
use crate::domain::ports::UsageProducers;
use crate::domain::test_support::{
    FakeIdentity, FakePdp, FakePlugin, FakeUsageCollector, Harness, PdpMode, StreamScript, body,
    bounded, ctx, entered, ev_in_progress, ev_text_delta, eventually, sample_response, set_cost,
};
use crate::infra::usage_collector::UsageCollectorReporter;

const MODEL: &str = "openai::gpt-4o";
const EMB_MODEL: &str = "openai::text-embedding";

fn cut_after() -> Duration {
    ShutdownConfig::default().cut_after()
}

fn gated_sync() -> (Harness, Arc<FakePlugin>) {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    let plugin = FakePlugin::with(
        |c| {
            let mut response = sample_response(&c.request_id, "gpt-4o");
            response.usage = Some(Usage::reported(10, 5, Decimal::new(5, 1)));
            Ok(response)
        },
        h.log.clone(),
        true,
    );
    h.set_plugin(plugin.clone());
    (h, plugin)
}

fn streaming() -> Harness {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin.set_stream(StreamScript::EventsThenHang(vec![
        ev_in_progress(0, "p", None),
        ev_text_delta(1, "abcdefgh"),
    ]));
    h
}

fn embedding_request() -> EmbeddingRequest {
    EmbeddingRequest::new(EMB_MODEL, EmbeddingInput::Single("hello".to_owned()))
}

fn assert_gateway_unavailable(err: &LlmGatewayError) {
    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(err.code(), "gateway_unavailable");
}

fn terminal_count(events: &[StreamingEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                StreamingEvent::Completed(_)
                    | StreamingEvent::Incomplete(_)
                    | StreamingEvent::Failed(_)
            )
        })
        .count()
}

async fn collect(stream: ResponseEventStream) -> Vec<StreamingEvent> {
    tokio::time::timeout(Duration::from_hours(1), stream.collect::<Vec<_>>())
        .await
        .expect("the stream ends")
}

fn spawn_sync(service: &Arc<Service>) -> tokio::task::JoinHandle<Result<(), LlmGatewayError>> {
    let service = Arc::clone(service);
    tokio::spawn(async move {
        service
            .create_response(&ctx(), body(MODEL))
            .await
            .map(|_| ())
    })
}

// ── stop accepting ──────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn after_begin_a_new_sync_request_is_gateway_unavailable_before_any_dependency_call() {
    let h = Harness::new();
    let service = h.service();
    h.shutdown.begin();

    let failure = service
        .create_response_traced(&ctx(), body(MODEL))
        .await
        .expect_err("not accepted while stopping");

    assert_gateway_unavailable(&failure.error);
    assert!(failure.response_id.starts_with("resp_"), "id is allocated");
    assert_eq!(h.pdp.calls(), 0, "no PEP call");
    assert_eq!(h.models.calls(), 0);
    assert!(h.quota.acquires().is_empty());
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn after_begin_a_new_stream_is_gateway_unavailable_before_any_dependency_call() {
    let h = streaming();
    let service = h.service();
    h.shutdown.begin();

    let failure = service
        .create_response_stream_traced(&ctx(), body(MODEL))
        .await
        .err()
        .expect("not accepted while stopping");

    assert_gateway_unavailable(&failure.error);
    assert_eq!(h.pdp.calls(), 0);
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn after_begin_a_new_embeddings_request_is_gateway_unavailable_before_any_dependency_call() {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    let service = h.service();
    h.shutdown.begin();

    let failure = service
        .create_embedding_traced(&ctx(), embedding_request())
        .await
        .expect_err("not accepted while stopping");

    assert_gateway_unavailable(&failure.error);
    assert!(failure.request_id.starts_with("emb_"));
    assert_eq!(h.pdp.calls(), 0);
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn before_begin_requests_are_accepted() {
    let h = Harness::new();
    let service = h.service();
    service
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("accepted");
}

// ── in-flight calls continue until the deadline ─────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_sync_call_in_flight_at_begin_that_finishes_before_the_deadline_succeeds() {
    let (h, plugin) = gated_sync();
    let service = h.service();
    let request = spawn_sync(&service);
    crate::domain::test_support::entered(&plugin.entered).await;

    h.shutdown.begin();
    tokio::time::sleep(cut_after().saturating_sub(Duration::from_secs(1))).await;
    plugin.release.notify_one();

    bounded(request)
        .await
        .expect("task")
        .expect("finished within the drain deadline: no failed request");
    eventually("commit", || h.quota.commits().len() == 1).await;
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(
        !h.sink.reports()[0].usage.estimated,
        "the provider usage, not an estimate"
    );
}

#[tokio::test(start_paused = true)]
async fn an_open_stream_at_begin_keeps_relaying_until_the_deadline() {
    let h = streaming();
    let service = h.service();
    let mut stream = service
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    // created, in_progress, one delta
    for _ in 0..3 {
        stream.next().await.expect("event");
    }

    h.shutdown.begin();
    tokio::time::sleep(cut_after().saturating_sub(Duration::from_secs(1))).await;

    assert!(!h.plugin.stream_dropped(), "still open just before the cut");
    assert!(!h.plugin.contexts()[0].cancel.is_cancelled());
    drop(stream);
}

// ── the cut ─────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_sync_call_still_running_at_the_deadline_is_gateway_unavailable_and_settles_the_estimate()
{
    let (h, plugin) = gated_sync();
    let service = h.service();
    let request = spawn_sync(&service);
    crate::domain::test_support::entered(&plugin.entered).await;

    let begun = Instant::now();
    h.shutdown.begin();
    let err = bounded(request)
        .await
        .expect("task")
        .expect_err("cut at the drain deadline");

    assert_gateway_unavailable(&err);
    assert_eq!(begun.elapsed(), cut_after(), "exactly at the deadline");
    assert!(
        plugin.contexts()[0].cancel.is_cancelled(),
        "the request token is cancelled"
    );
    // Input-only estimate: 2 input tokens x 1000 / 1000.
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    assert!(h.quota.releases().is_empty());
    eventually("report", || h.sink.reports().len() == 1).await;
    let usage = &h.sink.reports()[0].usage;
    assert!(!usage.complete);
    assert!(usage.estimated);
    assert!(logs_contain("shutdown"), "the cut is logged");
}

#[tokio::test(start_paused = true)]
async fn an_embeddings_call_still_running_at_the_deadline_is_gateway_unavailable_and_settles_the_estimate()
 {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    let plugin = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    h.set_plugin(plugin.clone());
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            service
                .create_embedding_traced(&ctx(), embedding_request())
                .await
        })
    };
    crate::domain::test_support::entered(&plugin.entered).await;

    let begun = Instant::now();
    h.shutdown.begin();
    let failure = bounded(request)
        .await
        .expect("task")
        .expect_err("cut at the drain deadline");

    assert_gateway_unavailable(&failure.error);
    assert_eq!(begun.elapsed(), cut_after());
    assert!(plugin.embedding_contexts()[0].cancel.is_cancelled());
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.sink.reports()[0].usage.estimated);
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_stream_open_at_the_deadline_ends_with_one_failed_gateway_unavailable_and_settles() {
    let h = streaming();
    let service = h.service();
    let stream = service
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");

    let begun = Instant::now();
    h.shutdown.begin();
    let events = collect(stream).await;

    assert_eq!(begun.elapsed(), cut_after(), "exactly at the deadline");
    assert_eq!(terminal_count(&events), 1, "exactly one terminal");
    let Some(StreamingEvent::Failed(failed)) = events.last() else {
        panic!("not response.failed: {:?}", events.last());
    };
    assert_eq!(
        failed.response.error.as_ref().expect("error").code,
        "gateway_unavailable"
    );
    let usage = failed.response.usage.as_ref().expect("usage");
    assert!(!usage.complete);
    assert!(usage.estimated);
    assert!(logs_contain(&failed.response.id) && logs_contain("shutdown"));

    // Settled with the estimate (input 2 tokens + 2 relayed output tokens).
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(6));
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.sink.reports()[0].usage.estimated);
    h.plugin.wait_stream_dropped().await;
    assert!(h.plugin.contexts()[0].cancel.is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn once_everything_is_cut_and_settled_the_service_is_quiescent() {
    let (h, plugin) = gated_sync();
    let service = h.service();
    let request = spawn_sync(&service);
    crate::domain::test_support::entered(&plugin.entered).await;
    h.shutdown.begin();
    bounded(request).await.expect("task").expect_err("cut");

    tokio::time::timeout(Duration::from_secs(5), service.quiescent())
        .await
        .expect("the slot is free and the settle done");
    assert_eq!(h.quota.commits().len(), 1);
    assert_eq!(h.sink.reports().len(), 1);
}

// ── the cut reaches a request still in preparation ─────────────────────────

/// A day: the dependency bounds must not fire before the shutdown cut.
const NO_BOUND: Duration = Duration::from_hours(24);

fn assert_nothing_reported(h: &Harness) {
    assert_eq!(h.plugin.calls(), 0, "the plugin is not called");
    assert_eq!(h.plugin.stream_calls(), 0);
    assert!(h.sink.reports().is_empty(), "no usage report");
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_request_parked_in_the_pep_at_the_deadline_is_gateway_unavailable() {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h.pdp = FakePdp::new(PdpMode::Hang, h.log.clone());
    h.timeouts.dependency.authz_resolver = NO_BOUND;
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.create_response_traced(&ctx(), body(MODEL)).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let begun = Instant::now();
    h.shutdown.begin();
    let failure = bounded(request)
        .await
        .expect("task")
        .expect_err("cut in preparation");

    assert_gateway_unavailable(&failure.error);
    assert_eq!(begun.elapsed(), cut_after(), "exactly at the deadline");
    assert!(h.quota.acquires().is_empty());
    assert_nothing_reported(&h);
    assert!(logs_contain("shutdown"), "the cut is logged");
}

#[tokio::test(start_paused = true)]
async fn a_stream_parked_in_the_pep_at_the_deadline_is_gateway_unavailable() {
    let mut h = streaming();
    h.pdp = FakePdp::new(PdpMode::Hang, h.log.clone());
    h.timeouts.dependency.authz_resolver = NO_BOUND;
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.create_response_stream(&ctx(), body(MODEL)).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let begun = Instant::now();
    h.shutdown.begin();
    let err = bounded(request)
        .await
        .expect("task")
        .err()
        .expect("cut in preparation");

    assert_gateway_unavailable(&err);
    assert_eq!(begun.elapsed(), cut_after());
    assert_nothing_reported(&h);
}

#[tokio::test(start_paused = true)]
async fn an_embeddings_request_parked_in_the_pep_at_the_deadline_is_gateway_unavailable() {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    h.pdp = FakePdp::new(PdpMode::Hang, h.log.clone());
    h.timeouts.dependency.authz_resolver = NO_BOUND;
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.create_embedding(&ctx(), embedding_request()).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    h.shutdown.begin();
    let err = bounded(request)
        .await
        .expect("task")
        .expect_err("cut in preparation");

    assert_gateway_unavailable(&err);
    assert_eq!(h.plugin.embedding_calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_request_parked_in_the_quota_admission_at_the_deadline_is_gateway_unavailable() {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h.quota.park_acquire();
    h.timeouts.dependency.quota_enforcement = NO_BOUND;
    let service = h.service();
    let request = spawn_sync(&service);
    entered(&h.quota.acquire_entered).await;

    h.shutdown.begin();
    let err = bounded(request)
        .await
        .expect("task")
        .expect_err("cut in preparation");

    assert_gateway_unavailable(&err);
    assert_nothing_reported(&h);
    assert!(h.quota.commits().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_request_cut_in_preparation_after_the_lease_is_acquired_releases_it() {
    // The lease is held while the media fetch is parked.
    let mut h = Harness::new();
    h.set_model(|v| {
        set_cost(v, Some(1000), Some(2000));
        v["capabilities"]["vision"]["enabled"] = serde_json::json!(true);
    });
    h.plugin.set_prefetch_media(true);
    h.fetcher.set_route(true);
    h.fetcher.gate();
    h.media.fetch_timeout = NO_BOUND;
    h.media.total_timeout = NO_BOUND;
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let mut value = serde_json::to_value(body(MODEL)).expect("body serializes");
            value["input"] = serde_json::json!([{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_image", "image_url": "https://example.com/a.png" }]
            }]);
            let request = serde_json::from_value(value).expect("request deserializes");
            service.create_response(&ctx(), request).await
        })
    };
    entered(&h.fetcher.entered).await;
    eventually("lease acquired", || h.quota.acquires().len() == 1).await;

    h.shutdown.begin();
    let err = bounded(request)
        .await
        .expect("task")
        .expect_err("cut in preparation");

    assert_gateway_unavailable(&err);
    eventually("release", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty(), "nothing was consumed");
    assert_nothing_reported(&h);
}

// ── the Usage Reporter follows the deadline ─────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_reporter_waits_for_the_cut_requests_and_then_drains_their_usage() {
    // One gear stop, as `serve` wires it: the same cancel starts the
    // service's shutdown signal and the reporter's quiescence wait.
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    let plugin = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    plugin.set_streaming_transport(true);
    plugin.set_stream(StreamScript::EventsThenHang(vec![
        ev_in_progress(0, "p", None),
        ev_text_delta(1, "abcdefgh"),
    ]));
    h.set_plugin(plugin.clone());

    let collector = FakeUsageCollector::new();
    let reporter = Arc::new(
        UsageCollectorReporter::new(
            collector.clone(),
            FakeIdentity::new(),
            &crate::config::UsageConfig::default(),
            &ShutdownConfig::default(),
        )
        .expect("reporter")
        .with_backoff_jitter(0.0),
    );
    let service = h.service_with_quota(h.quota_guard_with(reporter.clone()));

    let sync = spawn_sync(&service);
    crate::domain::test_support::entered(&plugin.entered).await;
    let stream = service
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let stream = tokio::spawn(async move { collect(stream).await });

    let stop = CancellationToken::new();
    let begin = {
        let signal = h.shutdown.clone();
        let stop = stop.clone();
        tokio::spawn(async move { signal.begin_on(&stop).await })
    };
    let producers: Arc<dyn UsageProducers> = service.clone();
    let worker = {
        let reporter = Arc::clone(&reporter);
        let stop = stop.clone();
        tokio::spawn(async move { reporter.run(stop, producers).await })
    };

    tokio::time::sleep(Duration::from_secs(1)).await;
    let stopped = Instant::now();
    stop.cancel();
    bounded(begin).await.expect("begin");

    // Both requests are cut at the deadline ...
    let sync = bounded(sync)
        .await
        .expect("sync task")
        .expect_err("sync is cut");
    assert_gateway_unavailable(&sync);
    let events = bounded(stream).await.expect("stream task");
    assert_eq!(terminal_count(&events), 1);
    assert_eq!(
        stopped.elapsed(),
        cut_after(),
        "both cut at the same deadline"
    );
    // The reporter may already have drained, in the same instant, the records
    // of the settled requests; what it must not do is deliver before the cut.
    assert!(
        collector
            .calls()
            .iter()
            .all(|c| c.at - stopped >= cut_after()),
        "nothing is drained before the cut requests settled"
    );

    // ... and the reporter, which waited for them, then drains both records
    // inside its quiescence budget plus the drain reserve.
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("the reporter stops")
        .expect("worker");
    assert!(
        stopped.elapsed() < ShutdownConfig::default().quiescence_budget(),
        "the cut leaves room inside the quiescence budget: {:?}",
        stopped.elapsed()
    );
    assert_eq!(collector.submitted_keys().len(), 2, "both usage records");
    assert_eq!(reporter.pending(), 0);
    assert!(
        collector
            .calls()
            .iter()
            .all(|c| c.at - stopped >= cut_after())
    );
}
