//! `timeouts.provider_sync` on the sync and embeddings dispatch (DESIGN §3.2
//! Request Pipeline, §4.5): a call over the bound is cancelled, answered
//! `provider_unavailable`, and its lease is settled with the input-only
//! estimate (the quota lease TTL relies on this bound).

use std::time::Duration;

use llm_gateway_sdk::{EmbeddingInput, EmbeddingRequest, LlmGatewayError, ProviderErrorCategory};
use tracing::Instrument as _;

use crate::domain::test_support::{
    FakePlugin, Harness, body, ctx, eventually, sample_response, set_cost,
};

const MODEL: &str = "openai::gpt-4o";
const EMB_MODEL: &str = "openai::text-embedding";
const BOUND: Duration = Duration::from_secs(30);
/// One second short of [`BOUND`].
const JUST_BEFORE: Duration = Duration::from_secs(29);

/// Priced sync harness (1000 / 2000 micro-credits per 1K tokens) with a 30 s
/// `provider_sync` and a plugin that parks every call.
fn hanging_sync() -> (Harness, std::sync::Arc<FakePlugin>) {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h.timeouts.provider_sync = BOUND;
    h.limits.max_inflight_sync = 1;
    let plugin = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    h.set_plugin(plugin.clone());
    (h, plugin)
}

fn hanging_embeddings() -> (Harness, std::sync::Arc<FakePlugin>) {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    h.timeouts.provider_sync = BOUND;
    h.limits.max_inflight_sync = 1;
    let gated = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    h.set_plugin(gated.clone());
    (h, gated)
}

fn embedding_request() -> EmbeddingRequest {
    EmbeddingRequest::new(EMB_MODEL, EmbeddingInput::Single("hello".to_owned()))
}

fn assert_provider_unavailable(err: &LlmGatewayError) {
    assert!(
        matches!(
            err,
            LlmGatewayError::Provider {
                category: ProviderErrorCategory::ProviderUnavailable,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(err.code(), "provider_unavailable");
    assert_eq!(
        err.to_response_error().message,
        "provider_unavailable: provider call timed out",
        "fixed message, no provider content"
    );
}

// ── sync ────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_sync_call_over_provider_sync_is_provider_unavailable_and_settles_the_estimate() {
    let (h, plugin) = hanging_sync();
    let svc = h.service();

    let started = tokio::time::Instant::now();
    let failure = svc
        .create_response_traced(&ctx(), body(MODEL))
        .await
        .expect_err("the plugin never answers");

    assert_provider_unavailable(&failure.error);
    assert_eq!(started.elapsed(), BOUND, "cut exactly at the bound");
    assert!(plugin.contexts()[0].cancel.is_cancelled());
    assert!(
        logs_contain("exceeded timeouts.provider_sync") && logs_contain(&failure.response_id),
        "the timeout is logged with the response id"
    );

    // Input-only estimate: 2 input tokens x 1000 / 1000.
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    assert!(h.quota.releases().is_empty());
    eventually("report", || h.sink.reports().len() == 1).await;
    let usage = &h.sink.reports()[0].usage;
    assert!(!usage.complete);
    assert!(usage.estimated);

    // The in-flight slot (limit 1) was released.
    plugin.release.notify_one();
    svc.create_response(&ctx(), body(MODEL))
        .await
        .expect("the slot is free again");
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_sync_call_finishing_just_before_the_bound_is_unaffected() {
    let (h, plugin) = hanging_sync();
    let svc = h.service();
    let releaser = plugin.clone();
    tokio::spawn(
        async move {
            tokio::time::sleep(JUST_BEFORE).await;
            releaser.release.notify_one();
        }
        .in_current_span(),
    );

    let resource = svc
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("answered in time");

    assert!(resource.id.starts_with("resp_"));
    assert!(!logs_contain("exceeded timeouts.provider_sync"));
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert!(h.quota.releases().is_empty(), "settled, not released");
}

// ── embeddings ──────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn an_embeddings_call_over_provider_sync_is_provider_unavailable_and_settles_the_estimate() {
    let (h, plugin) = hanging_embeddings();
    let svc = h.service();

    let started = tokio::time::Instant::now();
    let failure = svc
        .create_embedding_traced(&ctx(), embedding_request())
        .await
        .expect_err("the plugin never answers");

    assert_provider_unavailable(&failure.error);
    assert_eq!(started.elapsed(), BOUND);
    assert!(plugin.embedding_contexts()[0].cancel.is_cancelled());
    assert!(logs_contain("exceeded timeouts.provider_sync") && logs_contain(&failure.request_id));

    // Input-only estimate: 2 input tokens x 1000 / 1000.
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    eventually("report", || h.sink.reports().len() == 1).await;
    let usage = &h.sink.reports()[0].usage;
    assert!(!usage.complete);
    assert!(usage.estimated);

    plugin.release.notify_one();
    svc.create_embedding(&ctx(), embedding_request())
        .await
        .expect("the slot is free again");
}

#[tokio::test(start_paused = true)]
async fn an_embeddings_call_finishing_just_before_the_bound_is_unaffected() {
    let (h, plugin) = hanging_embeddings();
    let svc = h.service();
    let releaser = plugin.clone();
    tokio::spawn(async move {
        tokio::time::sleep(JUST_BEFORE).await;
        releaser.release.notify_one();
    });

    svc.create_embedding(&ctx(), embedding_request())
        .await
        .expect("answered in time");
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(
        !h.sink.reports()[0].usage.estimated,
        "the provider usage, not an estimate"
    );
}
