//! Shutdown quiescence of the pipeline (DESIGN §3.6, `UsageProducers`): it
//! resolves only once every sync and stream in-flight slot is free and every
//! background settle has finished.
//!
//! Paused tokio clock: waits advance virtual time.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;

use crate::domain::Service;
use crate::domain::ports::UsageProducers;
use crate::domain::test_support::{
    FakePlugin, Harness, StreamScript, body, ctx, ev_completed, ev_in_progress, qe_errors,
    sample_response, set_cost,
};

const MODEL: &str = "openai::gpt-4o";
const LONG: Duration = Duration::from_secs(5);

fn gated_sync() -> (Harness, Arc<FakePlugin>) {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    let plugin = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    h.set_plugin(plugin.clone());
    (h, plugin)
}

/// Whether `quiescent` resolves within `LONG` of virtual time.
async fn resolves(service: &Service) -> bool {
    tokio::time::timeout(LONG, service.quiescent())
        .await
        .is_ok()
}

#[tokio::test(start_paused = true)]
async fn an_idle_service_is_quiescent_at_once() {
    let h = Harness::new();
    let service = h.service();
    assert!(resolves(&service).await);
}

#[tokio::test(start_paused = true)]
async fn a_sync_request_in_flight_holds_quiescence_until_it_ends() {
    let (h, plugin) = gated_sync();
    let service = h.service();
    let request = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await })
    };
    tokio::time::timeout(LONG, plugin.entered.notified())
        .await
        .expect("the request reached the plugin");

    assert!(!resolves(&service).await, "a sync slot is held");

    plugin.release.notify_one();
    request.await.expect("request task").expect("response");
    assert!(resolves(&service).await);
}

#[tokio::test(start_paused = true)]
async fn an_open_stream_holds_quiescence_until_it_is_dropped() {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_in_progress(
            1, "resp", None,
        )]));
    let service = h.service();
    let mut stream = service
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = stream.next().await;

    assert!(!resolves(&service).await, "a stream slot is held");

    drop(stream);
    assert!(resolves(&service).await);
}

#[tokio::test(start_paused = true)]
async fn a_completed_stream_is_quiescent_once_its_settle_and_report_are_done() {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin
        .set_stream(StreamScript::Events(vec![ev_completed(1, "resp", None)]));
    let service = h.service();
    let stream = service
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = stream.collect::<Vec<_>>().await;

    assert!(resolves(&service).await);
    assert_eq!(h.quota.commits().len(), 1);
    assert_eq!(h.sink.reports().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn quiescence_waits_for_a_settle_that_outlives_its_request() {
    let (h, plugin) = gated_sync();
    h.quota
        .script_commit((0..2).map(|_| Err(qe_errors::unavailable())));
    plugin.release.notify_one();
    let service = h.service();
    service
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("response");
    assert!(h.sink.reports().is_empty(), "settle still retrying");

    assert!(resolves(&service).await);
    assert_eq!(h.quota.commits().len(), 3);
    assert_eq!(
        h.sink.reports().len(),
        1,
        "usage handed over before it returns"
    );
}
