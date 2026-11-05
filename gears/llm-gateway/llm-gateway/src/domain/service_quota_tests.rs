//! Slice 5a pipeline tests, sync and embeddings: pricing pre-check, quota
//! admission concurrent with credential resolution, lease release, settle at
//! every terminal state, hand-off to the usage sink (DESIGN §3.2 Request
//! Pipeline / Quota Guard, §3.6 Admission: Quota, §4.7 Quota bullet).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingRequest, EmbeddingResponse, LlmGatewayError, ProviderErrorCategory,
    ProviderFailure, ResponseResource, Usage,
};
use model_registry_sdk::ModelInfoV1;
use rust_decimal::Decimal;
use secrecy::SecretString;
use serde_json::json;
use tokio::sync::Notify;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::Service;
use crate::domain::ports::CredentialBroker;
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::test_support::{
    FakePlugin, FakeRules, Harness, SERVICE_SUBJECT, body, ctx, denied, entered, eventually,
    eventually_within, qe_errors, remove_cost, sample_embedding_response, sample_response,
    sample_response_with_text, set_cost,
};

const MODEL: &str = "openai::gpt-4o";
const EMB_MODEL: &str = "openai::text-embedding";

fn dec(mantissa: i64, scale: u32) -> Decimal {
    Decimal::new(mantissa, scale)
}

/// Harness whose model costs 1000 / 2000 micro-credits per 1K tokens. A
/// "hello" request then reserves 8194 units: 2 input tokens plus the model
/// output limit of 4096 tokens.
fn priced() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h
}

const RESERVED: i64 = 8194;

fn embeddings(input: Option<u64>, output: Option<u64>) -> Harness {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, input, output));
    h
}

fn embedding_request(text: &str) -> EmbeddingRequest {
    EmbeddingRequest::new(EMB_MODEL, EmbeddingInput::Single(text.to_owned()))
}

/// A plugin answering with provider usage of `cost` AI credits.
fn plugin_costing(h: &mut Harness, cost: Decimal) {
    h.set_plugin(FakePlugin::with(
        move |c| {
            let mut resource = sample_response(&c.request_id, "gpt-4o");
            resource.usage = Some(Usage::reported(100, 50, cost));
            Ok(resource)
        },
        h.log.clone(),
        false,
    ));
}

fn provider_failure() -> LlmGatewayError {
    LlmGatewayError::provider(
        ProviderErrorCategory::ProviderUnavailable,
        "provider down",
        ProviderFailure::default(),
    )
}

async fn sync_call(
    h: &Harness,
) -> Result<ResponseResource, Box<crate::domain::CreateResponseFailure>> {
    h.service()
        .create_response_traced(&ctx(), body(MODEL))
        .await
}

// ── pricing pre-check ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_model_without_the_required_pricing_is_internal_before_any_lease_or_key() {
    for patch in [
        (|v: &mut serde_json::Value| set_cost(v, Some(1000), None)) as fn(&mut serde_json::Value),
        |v| set_cost(v, None, Some(2000)),
        remove_cost,
    ] {
        let mut h = Harness::new();
        h.set_model(patch);
        let failure = sync_call(&h).await.expect_err("fail-closed");

        assert_eq!(failure.error.code(), "internal");
        assert_eq!(h.broker.calls(), 0, "no credential fetch");
        assert!(h.quota.calls().is_empty(), "no lease");
        assert_eq!(h.plugin.calls(), 0);
        assert!(h.sink.reports().is_empty());
    }
}

#[tokio::test]
async fn the_pricing_check_runs_after_the_capability_checks() {
    // A capability failure wins over a pricing failure.
    let mut h = Harness::new();
    h.set_model(remove_cost);
    h.rules = FakeRules {
        unsupported_params: ["temperature"].into(),
        ..FakeRules::default()
    };
    let mut request = body(MODEL);
    request.temperature = Some(0.5);
    let failure = h
        .service()
        .create_response_traced(&ctx(), request)
        .await
        .expect_err("rejected");
    assert_eq!(failure.error.code(), "unsupported_parameter");
}

#[tokio::test]
async fn zero_pricing_acquires_no_lease_and_does_no_settle_but_reports_the_usage() {
    let mut h = Harness::new(); // zero rates by default
    plugin_costing(&mut h, dec(3, 4));
    sync_call(&h).await.expect("admitted without a lease");

    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.quota.calls().is_empty(), "no acquire, commit or release");
    assert_eq!(
        h.sink.reports()[0].quantity_micro_credits,
        300,
        "the plugin's quantity is recorded"
    );
    assert_eq!(h.plugin.calls(), 1);
}

#[tokio::test]
async fn mixed_zero_and_nonzero_rates_reserve_normally() {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(0), Some(2000)));
    sync_call(&h).await.expect("admitted");
    let acquires = h.quota.acquires();
    assert_eq!(acquires.len(), 1);
    assert_eq!(
        acquires[0].1.amount, 8192,
        "4096 output tokens x 2000 / 1000"
    );
}

// ── admission ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_lease_amount_is_the_upper_bound_estimate_in_micro_credits() {
    let h = priced();
    sync_call(&h).await.expect("admitted");

    let acquires = h.quota.acquires();
    assert_eq!(acquires.len(), 1);
    let (subject, request) = &acquires[0];
    assert_eq!(request.amount, RESERVED);
    assert_eq!(
        *subject,
        Uuid::from_u128(1),
        "acquired under the caller context"
    );
    assert_eq!(
        request.ttl_secs,
        Some((h.timeouts.provider_sync + h.quota_cfg.lease_ttl_margin).as_secs())
    );
    let response_id = h.plugin.contexts()[0].request_id.clone();
    assert!(request.idempotency_key.contains(&response_id));
}

#[tokio::test]
async fn the_request_output_limit_shrinks_the_reservation() {
    let h = priced();
    let mut request = body(MODEL);
    request.max_output_tokens = Some(100);
    h.service()
        .create_response(&ctx(), request)
        .await
        .expect("admitted");
    assert_eq!(h.quota.acquires()[0].1.amount, 202);
}

#[tokio::test]
async fn instructions_and_tools_enlarge_the_input_estimate() {
    let h = priced();
    let mut request = body(MODEL);
    request.max_output_tokens = Some(0);
    request.instructions = Some("x".repeat(4000));
    h.service()
        .create_response(&ctx(), request)
        .await
        .expect("admitted");
    // 4000 instruction bytes + 5 bytes of "hello" = 4005 bytes = 1002 tokens
    // (rounded up once); (1002 input tokens x 1000) / 1000, no output allowance.
    assert_eq!(h.quota.acquires()[0].1.amount, 1002);
}

#[tokio::test]
async fn no_applicable_quota_admits_without_a_lease_and_without_settle() {
    let mut h = priced();
    plugin_costing(&mut h, dec(3, 4));
    h.quota.script_acquire([Ok(denied("NO_APPLICABLE_QUOTA"))]);
    sync_call(&h).await.expect("admitted");

    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.quota.settle_calls(), 0);
    assert_eq!(h.plugin.calls(), 1);
}

#[tokio::test]
async fn a_denied_lease_is_quota_exceeded_without_a_provider_call() {
    let h = priced();
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let failure = sync_call(&h).await.expect_err("denied");

    assert_eq!(failure.error.code(), "quota_exceeded");
    assert!(failure.response_id.starts_with("resp_"));
    assert_eq!(h.plugin.calls(), 0);
    assert!(h.sink.reports().is_empty(), "nothing was dispatched");
    assert_eq!(h.quota.settle_calls(), 0, "no lease, nothing to settle");
}

#[tokio::test]
async fn acquire_errors_map_to_their_codes_without_a_provider_call() {
    let cases = [
        (qe_errors::unavailable(), "quota_check_unavailable"),
        (qe_errors::deadline_exceeded(), "quota_check_unavailable"),
        (
            qe_errors::exhausted("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
            "quota_check_unavailable",
        ),
        (
            qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
            "quota_check_unavailable",
        ),
        (qe_errors::permission_denied(), "permission_denied"),
        (qe_errors::invalid_argument("TTL_OUT_OF_BOUNDS"), "internal"),
        (
            qe_errors::precondition("METRIC_NOT_QUOTA_GATED"),
            "internal",
        ),
        (qe_errors::precondition("METRIC_NOT_REGISTERED"), "internal"),
        (
            qe_errors::aborted("IDEMPOTENCY_PAYLOAD_MISMATCH"),
            "internal",
        ),
        (
            qe_errors::precondition("CONSTRAINT_CONTRACT_MISMATCH"),
            "internal",
        ),
        (qe_errors::exhausted("ENGINE_COST_EXCEEDED"), "internal"),
        (qe_errors::internal(), "internal"),
    ];
    for (err, expected) in cases {
        let h = priced();
        h.quota.script_acquire([Err(err.clone())]);
        let failure = sync_call(&h).await.expect_err("admission fails");
        assert_eq!(failure.error.code(), expected, "{err:?}");
        assert_ne!(failure.error.code(), "quota_exceeded");
        assert_eq!(h.plugin.calls(), 0, "{err:?}");
        assert!(h.plugin.contexts().is_empty());
    }
}

#[tokio::test]
async fn pep_denial_still_comes_before_any_quota_call() {
    let mut h = priced();
    h.pdp = crate::domain::test_support::FakePdp::new(
        crate::domain::test_support::PdpMode::Deny,
        h.log.clone(),
    );
    let failure = sync_call(&h).await.expect_err("denied");
    assert_eq!(failure.error.code(), "permission_denied");
    assert!(h.quota.calls().is_empty());
}

// ── admission concurrent with credential resolution ─────────────────────────

/// Broker parked until released, to prove what runs while it waits.
struct GatedBroker {
    entered: Notify,
    release: Notify,
    calls: AtomicUsize,
}

impl GatedBroker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl CredentialBroker for GatedBroker {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _model: &ModelInfoV1,
    ) -> Result<Option<SecretString>, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(None)
    }
}

fn service_with_broker(h: &Harness, broker: Arc<dyn CredentialBroker>) -> Arc<Service> {
    Arc::new(Service::new(
        PolicyEnforcer::new(h.pdp.clone()),
        h.models.clone(),
        h.plugins.clone(),
        broker,
        h.tool_schemas.clone(),
        Arc::new(ProviderRulesRegistry::permissive()),
        h.quota_guard(),
        &h.limits,
        &h.stream,
        &h.timeouts,
    ))
}

#[tokio::test]
async fn credentials_are_resolved_while_admission_is_still_pending() {
    let h = priced();
    h.quota.park_acquire();
    let service = h.service();
    let call = tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await });

    entered(&h.quota.acquire_entered).await;
    eventually("credentials resolved while acquire_lease is parked", || {
        h.broker.calls() == 1
    })
    .await;
    assert_eq!(h.plugin.calls(), 0, "dispatch waits for both");

    h.quota.acquire_release.notify_one();
    call.await.expect("joined").expect("succeeds");
    assert_eq!(h.plugin.calls(), 1);
}

#[tokio::test]
async fn admission_runs_while_credentials_are_still_pending() {
    let h = priced();
    let broker = GatedBroker::new();
    let service = service_with_broker(&h, broker.clone());
    let call = tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await });

    entered(&broker.entered).await;
    eventually("acquire_lease while credentials are parked", || {
        h.quota.acquires().len() == 1
    })
    .await;
    assert_eq!(h.plugin.calls(), 0);

    broker.release.notify_one();
    call.await.expect("joined").expect("succeeds");
    assert_eq!(h.plugin.calls(), 1);
}

#[tokio::test]
async fn a_failed_admission_drops_the_fetched_key_and_never_dispatches() {
    let mut h = priced();
    h.broker = crate::domain::test_support::FakeBroker::with(
        || Ok(Some(SecretString::from("sk-fetched"))),
        h.log.clone(),
    );
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let failure = sync_call(&h).await.expect_err("denied");

    assert_eq!(failure.error.code(), "quota_exceeded");
    assert_eq!(h.broker.calls(), 1, "fetched concurrently");
    assert!(
        h.plugin.contexts().is_empty(),
        "the key never reaches the plugin"
    );
}

#[tokio::test]
async fn a_credential_failure_after_the_lease_was_acquired_releases_the_lease() {
    let mut h = priced();
    h.broker = crate::domain::test_support::FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    let failure = sync_call(&h).await.expect_err("credentials fail");

    assert_eq!(failure.error.code(), "credentials_unavailable");
    assert_eq!(h.plugin.calls(), 0);
    eventually("release", || h.quota.releases().len() == 1).await;
    let (subject, release) = &h.quota.releases()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(
        release.token,
        crate::domain::test_support::FakeQuota::token(0)
    );
    assert!(h.quota.commits().is_empty());
    assert!(
        h.sink.reports().is_empty(),
        "nothing was dispatched: no usage record"
    );
}

#[tokio::test]
async fn a_credential_failure_without_a_lease_calls_no_settle() {
    let mut h = Harness::new(); // zero pricing: no lease
    h.broker = crate::domain::test_support::FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    sync_call(&h).await.expect_err("credentials fail");
    tokio::task::yield_now().await;
    assert!(h.quota.calls().is_empty());
}

#[tokio::test]
async fn when_both_fail_the_admission_error_is_reported_and_nothing_is_held() {
    let mut h = priced();
    h.broker = crate::domain::test_support::FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let failure = sync_call(&h).await.expect_err("both fail");
    assert_eq!(failure.error.code(), "quota_exceeded");
    assert_eq!(h.quota.settle_calls(), 0, "no lease was acquired");
}

// ── settle: sync terminal states ────────────────────────────────────────────

#[tokio::test]
async fn a_completed_call_commits_the_plugin_cost_in_micro_credits() {
    let mut h = priced();
    plugin_costing(&mut h, dec(4, 4)); // 0.0004 credit
    sync_call(&h).await.expect("ok");

    eventually("commit", || h.quota.commits().len() == 1).await;
    let (subject, commit) = &h.quota.commits()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(commit.actual_amount, Some(400));
    assert_eq!(
        commit.token,
        crate::domain::test_support::FakeQuota::token(0)
    );
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 400);
    assert!(report.usage.complete);
    assert!(!report.usage.estimated);
    assert_eq!(report.response_id, h.plugin.contexts()[0].request_id);
    assert!(h.quota.releases().is_empty());
}

#[tokio::test]
async fn a_zero_cost_call_releases_the_lease() {
    let mut h = priced();
    plugin_costing(&mut h, Decimal::ZERO);
    sync_call(&h).await.expect("ok");

    eventually("release", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty());
    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 0);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_cost_above_the_reservation_commits_the_reservation_and_reports_the_full_actual() {
    let mut h = priced();
    plugin_costing(&mut h, dec(1, 2)); // 10_000 micro-credits > 8194
    sync_call(&h).await.expect("ok");

    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(RESERVED));
    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 10_000);
    assert!(logs_contain("overrun"));
    assert!(logs_contain(&h.plugin.contexts()[0].request_id));
}

#[tokio::test]
async fn a_provider_error_after_dispatch_releases_the_lease_and_reports_zero() {
    let mut h = priced();
    h.set_plugin(FakePlugin::with(
        |_| Err(provider_failure()),
        h.log.clone(),
        false,
    ));
    let failure = sync_call(&h).await.expect_err("provider failure");
    assert_eq!(failure.error.code(), "provider_unavailable");

    eventually("release", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty());
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert_eq!(report.quantity_micro_credits, 0);
    assert!(report.usage.complete, "a provider error response is final");
    assert!(!report.usage.estimated);
}

#[tokio::test]
async fn an_output_validation_failure_still_settles_the_plugin_cost() {
    let mut h = priced();
    h.set_model(|v| {
        set_cost(v, Some(1000), Some(2000));
        v["capabilities"]["response_schema"] = json!(true);
    });
    h.set_plugin(FakePlugin::with(
        |c| {
            let mut resource = sample_response_with_text(&c.request_id, "gpt-4o", "not json");
            resource.usage = Some(Usage::reported(100, 50, dec(4, 4)));
            Ok(resource)
        },
        h.log.clone(),
        false,
    ));
    let mut request = body(MODEL);
    request.text = serde_json::from_value(json!({ "format": {
        "type": "json_schema", "name": "o", "strict": true, "schema": { "type": "object" }
    }}))
    .expect("text format");
    let failure = h
        .service()
        .create_response_traced(&ctx(), request)
        .await
        .expect_err("validation fails");
    assert_eq!(failure.error.code(), "output_validation_error");

    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(
        h.quota.commits()[0].1.actual_amount,
        Some(400),
        "the tokens were spent"
    );
    eventually("report", || h.sink.reports().len() == 1).await;
}

#[tokio::test]
async fn a_response_without_usage_settles_the_core_estimate() {
    // The plugin returned no usage: input-only estimate, flagged.
    let h = priced(); // echo plugin: `usage = None`
    sync_call(&h).await.expect("ok");

    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(
        h.quota.commits()[0].1.actual_amount,
        Some(2),
        "2 input tokens x 1000 / 1000"
    );
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
}

#[tokio::test]
async fn a_consumer_disconnect_during_dispatch_settles_the_input_only_estimate() {
    let mut h = priced();
    h.set_plugin(FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    ));
    let service = h.service();
    let call = tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await });
    entered(&h.plugin.entered).await;
    call.abort();
    drop(call.await);

    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
    assert!(h.quota.releases().is_empty());
}

#[tokio::test]
async fn a_consumer_disconnect_while_admission_is_pending_releases_the_lease_once_acquired() {
    // The request is cut before dispatch: nothing was sent to the provider.
    let h = priced();
    h.quota.park_acquire();
    let service = h.service();
    let call = tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await });
    entered(&h.quota.acquire_entered).await;
    call.abort();
    drop(call.await);
    assert_eq!(h.plugin.calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_response_is_never_delayed_by_settle() {
    let mut h = priced();
    plugin_costing(&mut h, dec(4, 4));
    // Every settle attempt fails transiently; the response still returns.
    h.quota
        .script_commit((0..100).map(|_| Err(qe_errors::unavailable())));
    sync_call(&h)
        .await
        .expect("returned without waiting for the settle");
    assert!(
        h.sink.reports().is_empty(),
        "the usage waits for the settle"
    );

    eventually_within(Duration::from_secs(60), "retried settle", || {
        h.quota.commits().len() >= 3
    })
    .await;
    assert!(
        h.sink.reports().is_empty(),
        "still no usage while the settle is failing"
    );
}

#[tokio::test]
async fn the_settle_runs_under_the_service_identity_with_explicit_tenant() {
    let mut h = priced();
    plugin_costing(&mut h, dec(4, 4));
    sync_call(&h).await.expect("ok");
    eventually("commit", || h.quota.commits().len() == 1).await;
    let (subject, commit) = &h.quota.commits()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(
        commit.tenant_id,
        quota_enforcement_sdk::TenantId::from(Uuid::from_u128(2))
    );
    assert_ne!(
        commit.idempotency_key,
        h.quota.acquires()[0].1.idempotency_key
    );
}

// ── embeddings ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn embeddings_require_only_the_input_rate() {
    let h = embeddings(Some(4000), None);
    h.service()
        .create_embedding(&ctx(), embedding_request(&"a".repeat(400)))
        .await
        .expect("an embedding model needs no output rate");
    let acquires = h.quota.acquires();
    assert_eq!(acquires.len(), 1);
    assert_eq!(acquires[0].1.amount, 400, "100 input tokens x 4000 / 1000");
}

#[tokio::test]
async fn an_embedding_model_without_an_input_rate_is_internal_before_any_lease_or_key() {
    for (input, output) in [(None, Some(100)), (None, None)] {
        let h = embeddings(input, output);
        let failure = h
            .service()
            .create_embedding_traced(&ctx(), embedding_request("hi"))
            .await
            .expect_err("fail-closed");
        assert_eq!(failure.error.code(), "internal");
        assert!(failure.request_id.starts_with("emb_"));
        assert_eq!(h.broker.calls(), 0);
        assert!(h.quota.calls().is_empty());
        assert_eq!(h.plugin.embedding_calls(), 0);
    }
}

#[tokio::test]
async fn embeddings_missing_cost_entirely_is_internal() {
    let mut h = Harness::new();
    h.enable_embeddings_with(remove_cost);
    let err = h
        .service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect_err("fail-closed");
    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn zero_input_rate_embeddings_acquire_no_lease() {
    let h = embeddings(Some(0), Some(500));
    h.service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect("admitted");
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.quota.calls().is_empty());
}

#[tokio::test]
async fn embedding_lease_ttl_is_the_sync_duration_plus_the_margin() {
    let h = embeddings(Some(1000), None);
    h.service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect("admitted");
    assert_eq!(
        h.quota.acquires()[0].1.ttl_secs,
        Some((h.timeouts.provider_sync + h.quota_cfg.lease_ttl_margin).as_secs())
    );
}

#[tokio::test]
async fn embedding_admission_failures_do_not_reach_the_provider() {
    for (err, expected) in [
        (qe_errors::unavailable(), "quota_check_unavailable"),
        (qe_errors::permission_denied(), "permission_denied"),
        (qe_errors::exhausted("ENGINE_COST_EXCEEDED"), "internal"),
    ] {
        let h = embeddings(Some(1000), None);
        h.quota.script_acquire([Err(err)]);
        let failure = h
            .service()
            .create_embedding_traced(&ctx(), embedding_request("hi"))
            .await
            .expect_err("admission fails");
        assert_eq!(failure.error.code(), expected);
        assert_eq!(h.plugin.embedding_calls(), 0);
    }
    let h = embeddings(Some(1000), None);
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let err = h
        .service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect_err("denied");
    assert_eq!(err.code(), "quota_exceeded");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn embedding_credential_failure_releases_the_lease() {
    let mut h = embeddings(Some(1000), None);
    h.broker = crate::domain::test_support::FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    h.service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect_err("credentials fail");
    eventually("release", || h.quota.releases().len() == 1).await;
    assert_eq!(h.plugin.embedding_calls(), 0);
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn an_embedding_settles_the_plugin_cost_and_reports_once() {
    let h = embeddings(Some(4000), None);
    h.plugin.set_embedding(|c| {
        let mut response: EmbeddingResponse =
            sample_embedding_response(&c.request_id, "text-embedding");
        response.usage = Usage::reported(100, 0, dec(2, 4));
        Ok(response)
    });
    let response = h
        .service()
        .create_embedding(&ctx(), embedding_request(&"a".repeat(400)))
        .await
        .expect("ok");

    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(200));
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert_eq!(report.response_id, response.id);
    assert!(report.response_id.starts_with("emb_"));
    assert_eq!(report.quantity_micro_credits, 200);
}

#[tokio::test]
async fn an_embedding_provider_error_releases_the_lease() {
    let h = embeddings(Some(1000), None);
    h.plugin.set_embedding(|_| Err(provider_failure()));
    h.service()
        .create_embedding(&ctx(), embedding_request("hi"))
        .await
        .expect_err("provider failure");
    eventually("release", || h.quota.releases().len() == 1).await;
    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.sink.reports()[0].quantity_micro_credits, 0);
}

#[tokio::test]
async fn a_cancelled_embedding_settles_the_input_estimate() {
    let mut h = embeddings(Some(4000), None);
    h.set_plugin(FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    ));
    let service = h.service();
    let call = tokio::spawn(async move {
        service
            .create_embedding(&ctx(), embedding_request(&"a".repeat(400)))
            .await
    });
    entered(&h.plugin.entered).await;
    call.abort();
    drop(call.await);
    eventually("commit", || h.quota.commits().len() == 1).await;
    assert_eq!(
        h.quota.commits()[0].1.actual_amount,
        Some(400),
        "100 input tokens x 4000 / 1000"
    );
    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.sink.reports()[0].usage.estimated);
}
