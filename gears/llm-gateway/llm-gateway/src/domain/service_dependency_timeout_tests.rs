//! `timeouts.dependency.<name>` on the dependency calls of the pipeline
//! (DESIGN §4.5, §4.4, §4.1): a call over its bound fails with that
//! dependency's fail-closed code; settle retries. Adapter-level bounds (Model
//! Registry, Type Registry, Usage Collector) are tested next to their
//! adapters.
//!
//! Paused tokio clock: expiry is asserted at the exact bound.

use std::time::Duration;

use llm_gateway_sdk::{EmbeddingInput, EmbeddingRequest, LlmGatewayError};
use tokio::time::Instant;

use crate::domain::test_support::{
    FakeBroker, FakePdp, Harness, PdpMode, StreamScript, body, bounded, ctx, ev_completed,
    eventually, eventually_within, set_cost,
};

const MODEL: &str = "openai::gpt-4o";
const EMB_MODEL: &str = "openai::text-embedding";
const BOUND: Duration = Duration::from_secs(5);
/// The retry comes one bound plus the backoff after the first call.
const RETRY_WITHIN: Duration = Duration::from_secs(10);

fn priced() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h
}

// ── authz_resolver ──────────────────────────────────────────────────────────

fn hanging_pdp() -> Harness {
    let mut h = priced();
    h.pdp = FakePdp::new(PdpMode::Hang, h.log.clone());
    h
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_hanging_pdp_fails_a_sync_request_closed_as_gateway_unavailable_at_the_bound() {
    let h = hanging_pdp();
    let service = h.service();
    let started = Instant::now();

    let failure = bounded(Box::pin(
        service.create_response_traced(&ctx(), body(MODEL)),
    ))
    .await
    .expect_err("fails closed");

    assert!(
        matches!(failure.error, LlmGatewayError::GatewayUnavailable),
        "{}",
        failure.error
    );
    assert_eq!(started.elapsed(), BOUND);
    assert_eq!(h.models.calls(), 0, "no dependency call after the PEP");
    assert_eq!(h.plugin.calls(), 0);
    assert!(logs_contain(&failure.response_id));
}

#[tokio::test(start_paused = true)]
async fn a_hanging_pdp_fails_a_stream_start_closed_as_gateway_unavailable() {
    let mut h = hanging_pdp();
    h.enable_streaming();
    let service = h.service();
    let started = Instant::now();

    let err = bounded(Box::pin(
        service.create_response_stream(&ctx(), body(MODEL)),
    ))
    .await
    .err()
    .expect("fails closed");

    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(started.elapsed(), BOUND);
}

#[tokio::test(start_paused = true)]
async fn a_hanging_pdp_fails_an_embeddings_request_closed_as_gateway_unavailable() {
    let mut h = hanging_pdp();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    let service = h.service();
    let started = Instant::now();

    let err = service
        .create_embedding(
            &ctx(),
            EmbeddingRequest::new(EMB_MODEL, EmbeddingInput::Single("hello".to_owned())),
        )
        .await
        .expect_err("fails closed");

    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(started.elapsed(), BOUND);
}

#[tokio::test(start_paused = true)]
async fn the_authz_bound_is_configurable() {
    let mut h = hanging_pdp();
    h.timeouts.dependency.authz_resolver = Duration::from_secs(2);
    let service = h.service();
    let started = Instant::now();

    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect_err("fails closed");

    assert_eq!(started.elapsed(), Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn a_pdp_that_answers_is_not_cut() {
    let h = priced();
    let service = h.service();
    let started = Instant::now();
    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect("answered");
    assert!(started.elapsed() < BOUND);
}

// ── quota_enforcement: acquire ──────────────────────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_hanging_acquire_is_quota_check_unavailable_at_the_bound_and_the_provider_is_not_called()
{
    let h = priced();
    h.quota.park_acquire();
    let service = h.service();
    let started = Instant::now();

    let failure = bounded(Box::pin(
        service.create_response_traced(&ctx(), body(MODEL)),
    ))
    .await
    .expect_err("fails closed");

    assert!(
        matches!(failure.error, LlmGatewayError::QuotaCheckUnavailable),
        "{}",
        failure.error
    );
    assert_eq!(failure.error.code(), "quota_check_unavailable");
    assert_eq!(started.elapsed(), BOUND);
    assert_eq!(h.plugin.calls(), 0, "the provider is not called");
    assert!(h.quota.commits().is_empty() && h.quota.releases().is_empty());
    assert!(logs_contain("quota") && logs_contain(&failure.response_id));
}

#[tokio::test(start_paused = true)]
async fn a_hanging_acquire_of_a_stream_is_quota_check_unavailable_at_the_bound() {
    let mut h = priced();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.quota.park_acquire();
    let service = h.service();
    let started = Instant::now();

    let err = bounded(Box::pin(
        service.create_response_stream(&ctx(), body(MODEL)),
    ))
    .await
    .err()
    .expect("fails closed");

    assert!(
        matches!(err, LlmGatewayError::QuotaCheckUnavailable),
        "{err}"
    );
    assert_eq!(started.elapsed(), BOUND);
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_acquire_answering_just_before_the_bound_is_admitted() {
    let h = priced();
    h.quota.park_acquire();
    let service = h.service();
    let quota = h.quota.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        quota.acquire_release.notify_one();
    });

    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect("admitted");
}

// ── quota_enforcement: settle ───────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_hanging_commit_times_out_and_is_retried_after_the_backoff() {
    let h = priced();
    h.quota.hang_settles(1);
    let service = h.service();
    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect("the caller is not held up by the settle");

    eventually_within(RETRY_WITHIN, "two commits", || h.quota.commits().len() == 2).await;
    let times = h.quota.settle_times();
    assert_eq!(
        times[1] - times[0],
        BOUND + h.quota_cfg.settle_retry_backoff,
        "the hung call is cut at the bound, then the backoff applies"
    );
    eventually("report", || h.sink.reports().len() == 1).await;
    let commits = h.quota.commits();
    assert_eq!(
        commits[1].1.actual_amount, commits[0].1.actual_amount,
        "the same amount and key on the retry"
    );
    assert_eq!(commits[1].1.idempotency_key, commits[0].1.idempotency_key);
}

#[tokio::test(start_paused = true)]
async fn a_hanging_release_times_out_and_is_retried_after_the_backoff() {
    let mut h = priced();
    // Credential failure after admission: the lease goes back (release).
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.quota.hang_settles(1);
    let service = h.service();
    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect_err("credentials unavailable");

    eventually_within(RETRY_WITHIN, "two releases", || {
        h.quota.releases().len() == 2
    })
    .await;
    let times = h.quota.settle_times();
    assert_eq!(
        times[1] - times[0],
        BOUND + h.quota_cfg.settle_retry_backoff
    );
}

#[tokio::test(start_paused = true)]
async fn the_stream_settle_is_bounded_too() {
    let mut h = priced();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin
        .set_stream(StreamScript::Events(vec![ev_completed(0, "p", None)]));
    h.quota.hang_settles(1);
    let service = h.service();
    let stream = bounded(Box::pin(
        service.create_response_stream(&ctx(), body(MODEL)),
    ))
    .await
    .expect("stream");
    drop(futures_util::StreamExt::collect::<Vec<_>>(stream).await);

    eventually_within(RETRY_WITHIN, "two commits", || h.quota.commits().len() == 2).await;
    let times = h.quota.settle_times();
    assert_eq!(
        times[1] - times[0],
        BOUND + h.quota_cfg.settle_retry_backoff
    );
}

// ── settle safety lead before the lease expires ─────────────────────────────

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn settle_retries_stop_a_safety_lead_before_the_lease_expires() {
    // Backlog item: no retry inside the last `SETTLE_SAFETY_LEAD` (2 s) of the
    // lease, where it would only draw a LEASE_NOT_ACTIVE.
    let mut h = priced();
    h.timeouts.provider_sync = Duration::from_secs(10);
    h.quota_cfg.lease_ttl_margin = Duration::from_secs(0);
    h.quota_cfg.settle_retry_backoff = Duration::from_secs(1);
    h.quota.script_commit(
        (0..100).map(|_| Err(crate::domain::test_support::qe_errors::unavailable())),
    );
    let service = h.service();
    let started = Instant::now();
    bounded(Box::pin(service.create_response(&ctx(), body(MODEL))))
        .await
        .expect("response");

    // The lease lasts 10 s: retries run until 8 s, none after.
    tokio::time::sleep(Duration::from_secs(30)).await;
    let last = *h.quota.settle_times().last().expect("settle calls");
    assert!(
        last - started <= Duration::from_secs(8),
        "no settle call inside the safety lead: last at {:?}",
        last - started
    );
    assert!(logs_contain("abandoned"));
}
