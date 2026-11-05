//! Slice 6a pipeline tests: media input. The Media Resolver is the third arm
//! of the concurrent step with quota admission and credentials (DESIGN §3.2
//! Request Pipeline / Media Resolver, seq `media-input`, §4.1).
//!
//! Precedence of the concurrent arms: admission error, then media error,
//! then credentials error (a caller error is reported before a transient
//! server-side one; whichever fails, a lease acquired meanwhile is released).

use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::{CreateResponseBody, LlmGatewayError};
use serde_json::json;

use crate::domain::test_support::{
    FakeBroker, Harness, StreamScript, body, ctx, denied, entered, ev_completed, eventually,
    set_cost,
};

const MODEL: &str = "openai::gpt-4o";
const PNG_URI: &str = "data:image/png;base64,iVBORw0KGgo=";
const FETCHED_URI: &str = "data:image/png;base64,iVBORw0KGgo=";

/// Priced model with vision and file input, a plugin that wants bytes and a
/// media egress route (so URLs reach the resolver).
fn prefetching() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| {
        set_cost(v, Some(1000), Some(2000));
        v["capabilities"]["vision"]["enabled"] = json!(true);
        v["capabilities"]["file_input"]["enabled"] = json!(true);
    });
    h.plugin.set_prefetch_media(true);
    h.fetcher.set_route(true);
    h
}

#[allow(clippy::needless_pass_by_value)] // call sites build the parts inline with `json!`
fn with_parts(parts: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body(MODEL)).expect("body serializes");
    value["input"] = json!([{ "type": "message", "role": "user", "content": parts }]);
    serde_json::from_value(value).expect("request deserializes")
}

fn https_image() -> CreateResponseBody {
    with_parts(json!([
        { "type": "input_text", "text": "what is this" },
        { "type": "input_image", "image_url": "https://example.com/a.png" }
    ]))
}

fn content_of(body: &CreateResponseBody) -> serde_json::Value {
    serde_json::to_value(body).expect("serializes")["input"][0]["content"].clone()
}

async fn call(
    h: &Harness,
    request: CreateResponseBody,
) -> Result<llm_gateway_sdk::ResponseResource, Box<crate::domain::CreateResponseFailure>> {
    h.service().create_response_traced(&ctx(), request).await
}

// ── resolution ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn prefetch_plugin_receives_the_fetched_image_as_a_data_uri() {
    let h = prefetching();
    call(&h, https_image()).await.expect("succeeds");

    assert_eq!(h.fetcher.urls(), ["https://example.com/a.png"]);
    assert_eq!(h.plugin.calls(), 1);
    assert_eq!(
        content_of(&h.plugin.bodies()[0])[1]["image_url"],
        FETCHED_URI
    );
    assert_eq!(h.quota.acquires().len(), 1);
}

#[tokio::test]
async fn prefetch_plugin_receives_inline_file_data_as_a_data_uri() {
    let h = prefetching();
    call(
        &h,
        with_parts(json!([{ "type": "input_file", "file_data": "aGk=", "filename": "a.pdf" }])),
    )
    .await
    .expect("succeeds");

    assert_eq!(h.fetcher.calls(), 0);
    assert_eq!(
        content_of(&h.plugin.bodies()[0])[0]["file_data"],
        "data:application/pdf;base64,aGk="
    );
}

#[tokio::test]
async fn url_forward_plugin_receives_the_parts_as_sent_and_nothing_is_fetched() {
    let h = prefetching();
    h.plugin.set_prefetch_media(false);
    let request = with_parts(json!([
        { "type": "input_image", "image_url": "https://example.com/a.png" },
        { "type": "input_file", "file_data": "aGk=", "filename": "a.pdf" }
    ]));
    h.fetcher.set_route(false);
    call(&h, request.clone()).await.expect("succeeds");

    assert_eq!(h.fetcher.calls(), 0);
    assert_eq!(h.plugin.bodies()[0].input, request.input);
}

#[tokio::test]
async fn a_request_without_media_never_touches_the_fetcher() {
    let h = prefetching();
    call(&h, body(MODEL)).await.expect("succeeds");
    assert_eq!(h.fetcher.calls(), 0);
    assert_eq!(h.plugin.calls(), 1);
}

// ── capability gate (before admission, credentials and the plugin) ──────────

#[tokio::test]
async fn unsupported_media_is_rejected_before_admission_credentials_and_dispatch() {
    let cases: Vec<(CreateResponseBody, &str)> = vec![
        (
            with_parts(json!([{ "type": "input_image", "file_id": "f" }])),
            "file_id",
        ),
        (
            with_parts(json!([{ "type": "input_audio", "data": "aGk=" }])),
            "audio_input",
        ),
        (
            with_parts(json!([{ "type": "input_video", "url": "https://example.com/v.mp4" }])),
            "video_input",
        ),
    ];
    for (request, capability) in cases {
        let h = prefetching();
        let failure = call(&h, request).await.expect_err("rejected");
        assert!(
            matches!(&failure.error, LlmGatewayError::CapabilityNotSupported { capability: c, .. } if c == capability),
            "{:?}",
            failure.error
        );
        assert!(h.quota.calls().is_empty());
        assert_eq!(h.broker.calls(), 0);
        assert_eq!(h.fetcher.calls(), 0);
        assert_eq!(h.plugin.calls(), 0);
    }
}

#[tokio::test]
async fn prefetch_plugin_without_a_media_route_rejects_urls_as_media_url() {
    let h = prefetching();
    h.fetcher.set_route(false);
    let failure = call(&h, https_image()).await.expect_err("no route");
    assert!(
        matches!(&failure.error, LlmGatewayError::CapabilityNotSupported { capability, .. } if capability == "media_url"),
        "{:?}",
        failure.error
    );
    assert!(h.quota.calls().is_empty());
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn invalid_media_is_rejected_before_admission() {
    let h = prefetching();
    let failure = call(
        &h,
        with_parts(json!([{ "type": "input_image", "image_url": "http://example.com/a.png" }])),
    )
    .await
    .expect_err("scheme rule");
    assert!(
        matches!(&failure.error, LlmGatewayError::MediaNotFound { param: Some(p), .. } if p == "input[0].content[0]"),
        "{:?}",
        failure.error
    );
    assert!(h.quota.calls().is_empty());
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn oversized_inline_media_is_a_validation_error_before_admission() {
    let mut h = prefetching();
    h.limits.max_inline_media_bytes = 4;
    let failure = call(
        &h,
        with_parts(json!([{ "type": "input_image", "image_url": PNG_URI }])),
    )
    .await
    .expect_err("8 bytes > 4");
    assert!(
        matches!(&failure.error, LlmGatewayError::Validation { param: Some(p), .. } if p == "input"),
        "{:?}",
        failure.error
    );
    assert!(h.quota.calls().is_empty());
    assert_eq!(h.plugin.calls(), 0);
}

// ── concurrency with admission and credentials ──────────────────────────────

#[tokio::test]
async fn media_is_fetched_while_admission_is_still_pending() {
    let h = prefetching();
    h.quota.park_acquire();
    let service = h.service();
    let call = tokio::spawn(async move { service.create_response(&ctx(), https_image()).await });

    entered(&h.quota.acquire_entered).await;
    eventually("media fetched while acquire_lease is parked", || {
        h.fetcher.calls() == 1
    })
    .await;
    assert_eq!(h.plugin.calls(), 0, "dispatch waits for every arm");

    h.quota.acquire_release.notify_one();
    call.await.expect("joined").expect("succeeds");
    assert_eq!(h.plugin.calls(), 1);
}

#[tokio::test]
async fn admission_runs_while_the_media_fetch_is_pending() {
    let h = prefetching();
    h.fetcher.gate();
    let service = h.service();
    let call = tokio::spawn(async move { service.create_response(&ctx(), https_image()).await });

    entered(&h.fetcher.entered).await;
    eventually("acquire_lease while the fetch is parked", || {
        h.quota.acquires().len() == 1
    })
    .await;
    eventually("credentials while the fetch is parked", || {
        h.broker.calls() == 1
    })
    .await;
    assert_eq!(h.plugin.calls(), 0);

    h.fetcher.release.notify_one();
    call.await.expect("joined").expect("succeeds");
    assert_eq!(h.plugin.calls(), 1);
}

// ── failure after the lease ─────────────────────────────────────────────────

#[tokio::test]
async fn a_media_failure_releases_the_lease_without_a_plugin_call_or_usage_report() {
    let h = prefetching();
    h.fetcher
        .script(|_| Err(LlmGatewayError::media_not_found()));
    let failure = call(&h, https_image()).await.expect_err("fetch fails");

    assert!(
        matches!(&failure.error, LlmGatewayError::MediaNotFound { param: Some(p), .. } if p == "input[0].content[1]"),
        "{:?}",
        failure.error
    );
    assert_eq!(h.quota.acquires().len(), 1, "admission ran concurrently");
    eventually("lease released", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty());
    assert_eq!(h.plugin.calls(), 0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.sink.reports().is_empty(), "nothing was dispatched");
}

#[tokio::test(start_paused = true)]
async fn a_media_fetch_timeout_releases_the_lease_and_never_dispatches() {
    let mut h = prefetching();
    h.media.fetch_timeout = Duration::from_secs(5);
    h.fetcher.gate(); // never released
    let failure = call(&h, https_image()).await.expect_err("timed out");

    assert_eq!(failure.error.code(), "media_not_found");
    eventually("lease released", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_admission_failure_aborts_a_parked_media_fetch() {
    let h = prefetching();
    h.fetcher.gate(); // never released
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);

    // The default `media.fetch_timeout` is 10s: answering within 1s of the
    // (auto-advanced) clock means the parked fetch did not hold the error back.
    let failure = tokio::time::timeout(Duration::from_secs(1), call(&h, https_image()))
        .await
        .expect("the admission error is not delayed by the media fetch")
        .expect_err("denied");

    assert_eq!(failure.error.code(), "quota_exceeded");
    assert_eq!(h.plugin.calls(), 0);
}

// ── precedence ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_admission_error_wins_over_a_media_error() {
    let h = prefetching();
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    h.fetcher
        .script(|_| Err(LlmGatewayError::media_not_found()));
    let failure = call(&h, https_image()).await.expect_err("denied");

    assert_eq!(failure.error.code(), "quota_exceeded");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn an_admission_error_wins_over_a_credentials_error_with_media_present() {
    let mut h = prefetching();
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let failure = call(&h, https_image()).await.expect_err("denied");
    assert_eq!(failure.error.code(), "quota_exceeded");
}

#[tokio::test]
async fn a_media_error_wins_over_a_credentials_error_and_releases_the_lease() {
    let mut h = prefetching();
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.fetcher
        .script(|_| Err(LlmGatewayError::media_not_found()));
    let failure = call(&h, https_image()).await.expect_err("media fails");

    assert_eq!(failure.error.code(), "media_not_found");
    eventually("lease released", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn a_credentials_error_still_releases_the_lease_when_the_media_is_fine() {
    let mut h = prefetching();
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    let failure = call(&h, https_image()).await.expect_err("credentials fail");

    assert_eq!(failure.error.code(), "credentials_unavailable");
    eventually("lease released", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.calls(), 0);
}

// ── streaming ───────────────────────────────────────────────────────────────

fn streaming() -> Harness {
    let mut h = prefetching();
    h.enable_streaming_with(|v| {
        set_cost(v, Some(1000), Some(2000));
        v["capabilities"]["vision"]["enabled"] = json!(true);
    });
    h.plugin
        .set_stream(StreamScript::Events(vec![ev_completed(0, "p", None)]));
    h
}

#[tokio::test]
async fn the_stream_path_resolves_media_too() {
    let h = streaming();
    let stream = h
        .service()
        .create_response_stream(&ctx(), https_image())
        .await
        .expect("stream starts");
    drop(stream.collect::<Vec<_>>().await);

    assert_eq!(h.fetcher.calls(), 1);
    assert_eq!(h.plugin.stream_calls(), 1);
    assert_eq!(
        content_of(&h.plugin.bodies()[0])[1]["image_url"],
        FETCHED_URI
    );
}

#[tokio::test]
async fn a_media_failure_before_the_stream_releases_the_lease_and_never_starts_it() {
    let h = streaming();
    h.fetcher
        .script(|_| Err(LlmGatewayError::media_not_found()));
    let failure = h
        .service()
        .create_response_stream_traced(&ctx(), https_image())
        .await
        .err()
        .expect("media fails");

    assert_eq!(failure.error.code(), "media_not_found");
    eventually("lease released", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.stream_calls(), 0);
    assert!(h.sink.reports().is_empty());
}

// ── embeddings ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn embeddings_have_no_media_step() {
    let mut h = Harness::new();
    h.enable_embeddings();
    h.fetcher.set_route(true);
    let request = llm_gateway_sdk::EmbeddingRequest::new(
        "openai::text-embedding",
        llm_gateway_sdk::EmbeddingInput::Single("https://example.com/a.png".to_owned()),
    );
    h.service()
        .create_embedding(&ctx(), request)
        .await
        .expect("a URL-looking text is just text");
    assert_eq!(h.fetcher.calls(), 0);
}

// ── logging ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[tracing_test::traced_test]
async fn media_urls_and_content_are_never_logged() {
    let h = prefetching();
    h.fetcher.script(|_| {
        Err(LlmGatewayError::internal(
            "fetch exploded: https://secret-host.example/token-9f3a",
        ))
    });
    drop(
        call(
            &h,
            with_parts(json!([
                { "type": "input_image", "image_url": "https://secret-host.example/token-9f3a.png" }
            ])),
        )
        .await
        .expect_err("fetch fails"),
    );
    drop(
        call(
            &h,
            with_parts(json!([
                { "type": "input_image", "image_url": "data:image/svg+xml;base64,U0VDUkVUUEFZTE9BRA==" }
            ])),
        )
        .await
        .expect_err("type not allowed"),
    );

    assert!(logs_contain("gateway request failed"));
    assert!(!logs_contain("secret-host"));
    assert!(!logs_contain("token-9f3a"));
    assert!(!logs_contain("U0VDUkVUUEFZTE9BRA"));
}
