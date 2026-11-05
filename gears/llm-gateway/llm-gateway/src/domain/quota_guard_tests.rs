//! Quota Guard tests (DESIGN §3.2 Quota Guard, §3.6 Admission: Quota, §4.7
//! Quota testing bullet): reason-token mapping, pricing pre-check, quote,
//! `acquire_lease` shape, settle (commit/release, overrun, identity,
//! idempotency, retry until lease expiry), and the hand-off to the Usage
//! Reporter port.

use std::time::Duration;

use llm_gateway_sdk::{LlmGatewayError, Usage};
use quota_enforcement_sdk::{SCOPE_USER, SubjectClaim, TenantId};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use super::{
    Operation, QuotaGuard, Quote, RequestMeta, RequestQuota, SettleErrorClass,
    classify_settle_error, map_acquire_error, reason_token,
};
use crate::domain::pricing::Pricing;
use crate::domain::test_support::{
    FakeQuota, Harness, OPENAI_TYPE, QuotaCall, SERVICE_SUBJECT, acquired, body, ctx, denied,
    eventually, eventually_within, qe_errors, remove_cost, sample_model_info_patched, set_cost,
};

const CALLER_TENANT: u128 = 2;
const CALLER_USER: u128 = 1;

fn dec(mantissa: i64, scale: u32) -> Decimal {
    Decimal::new(mantissa, scale)
}

fn meta() -> RequestMeta {
    RequestMeta {
        response_id: "resp_unit".to_owned(),
        model: "openai::gpt-4o".to_owned(),
        provider_type: OPENAI_TYPE.to_owned(),
    }
}

fn quote(reserved_units: i64) -> Quote {
    Quote {
        pricing: Pricing {
            input_per_1k_micro: 1000,
            output_per_1k_micro: 2000,
        },
        input_tokens: 10,
        reserved_units,
    }
}

fn code(err: &LlmGatewayError) -> &'static str {
    err.code()
}

/// Admit a request reserving `reserved` units under the harness fakes.
async fn admit(h: &Harness, reserved: i64) -> RequestQuota {
    h.quota_guard()
        .admit(&ctx(), meta(), Operation::Response, &quote(reserved))
        .await
        .expect("admitted")
}

/// Usage costing `credits` AI credits, complete and not estimated.
fn usage(credits: Decimal) -> Usage {
    Usage::reported(100, 50, credits)
}

// ── reason tokens ───────────────────────────────────────────────────────────

#[test]
fn reason_tokens_are_read_where_each_category_carries_them() {
    let cases = [
        (
            qe_errors::precondition("LEASE_NOT_ACTIVE"),
            Some("LEASE_NOT_ACTIVE"),
        ),
        (
            qe_errors::invalid_argument("TTL_OUT_OF_BOUNDS"),
            Some("TTL_OUT_OF_BOUNDS"),
        ),
        (
            qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
            Some("LEASE_CONTENTION_TIMEOUT"),
        ),
        (
            qe_errors::exhausted("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
            Some("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
        ),
        (qe_errors::permission_denied(), Some("AUTHZ")),
        (qe_errors::unavailable(), None),
        (qe_errors::deadline_exceeded(), None),
        (qe_errors::not_found(), None),
        (qe_errors::internal(), None),
    ];
    for (err, expected) in cases {
        assert_eq!(reason_token(&err).as_deref(), expected, "{err:?}");
    }
}

// ── acquire error mapping ───────────────────────────────────────────────────

#[test]
fn unavailable_timeout_inflight_limit_and_contention_are_quota_check_unavailable() {
    for err in [
        qe_errors::unavailable(),
        qe_errors::deadline_exceeded(),
        qe_errors::exhausted("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
        qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
    ] {
        assert_eq!(
            code(&map_acquire_error(&err)),
            "quota_check_unavailable",
            "{err:?}"
        );
    }
}

#[test]
fn a_pdp_refusal_is_permission_denied() {
    assert_eq!(
        code(&map_acquire_error(&qe_errors::permission_denied())),
        "permission_denied"
    );
}

#[test]
fn contract_faults_are_internal() {
    for err in [
        qe_errors::invalid_argument("TTL_OUT_OF_BOUNDS"),
        qe_errors::precondition("METRIC_NOT_QUOTA_GATED"),
        qe_errors::precondition("METRIC_NOT_REGISTERED"),
        qe_errors::precondition("CONSTRAINT_CONTRACT_MISMATCH"),
        qe_errors::precondition("PROJECTION_NOT_RESOLVABLE"),
        qe_errors::aborted("IDEMPOTENCY_PAYLOAD_MISMATCH"),
    ] {
        assert_eq!(code(&map_acquire_error(&err)), "internal", "{err:?}");
    }
}

#[test]
fn unlisted_errors_are_internal_and_never_quota_exceeded() {
    // `ENGINE_COST_EXCEEDED` shares the ResourceExhausted category with the
    // in-flight lease limit: the mapping is by reason token, not category.
    for err in [
        qe_errors::exhausted("ENGINE_COST_EXCEEDED"),
        qe_errors::exhausted("SOMETHING_NEW"),
        qe_errors::aborted("SOMETHING_NEW"),
        qe_errors::precondition("SOMETHING_NEW"),
        qe_errors::not_found(),
        qe_errors::internal(),
    ] {
        let mapped = map_acquire_error(&err);
        assert_eq!(code(&mapped), "internal", "{err:?}");
        assert_ne!(code(&mapped), "quota_exceeded");
    }
}

// ── settle error classes ────────────────────────────────────────────────────

#[test]
fn unavailable_timeout_and_contention_are_transient_settle_errors() {
    for err in [
        qe_errors::unavailable(),
        qe_errors::deadline_exceeded(),
        qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
    ] {
        assert_eq!(
            classify_settle_error(&err),
            SettleErrorClass::Transient,
            "{err:?}"
        );
    }
}

#[test]
fn every_other_settle_error_is_permanent() {
    for err in [
        qe_errors::precondition("LEASE_NOT_ACTIVE"),
        qe_errors::precondition("OVER_COMMIT_NOT_AUTHORIZED"),
        qe_errors::not_found(),
        qe_errors::aborted("IDEMPOTENCY_PAYLOAD_MISMATCH"),
        qe_errors::permission_denied(),
        qe_errors::invalid_argument("INVALID_AMOUNT"),
        qe_errors::internal(),
    ] {
        assert_eq!(
            classify_settle_error(&err),
            SettleErrorClass::Permanent,
            "{err:?}"
        );
    }
}

// ── pricing pre-check ───────────────────────────────────────────────────────

fn check(
    patch: impl FnOnce(&mut serde_json::Value),
    op: Operation,
) -> Result<Pricing, LlmGatewayError> {
    let rules =
        crate::domain::provider_rules::ProviderRulesRegistry::permissive().rules_for(OPENAI_TYPE);
    let model = sample_model_info_patched(OPENAI_TYPE, patch);
    QuotaGuard::check_pricing(rules.as_ref(), &model, op)
}

#[test]
fn responses_require_both_rates_and_return_them() {
    for op in [Operation::Response, Operation::Stream] {
        let pricing = check(|v| set_cost(v, Some(150), Some(600)), op).expect("priced");
        assert_eq!(
            pricing,
            Pricing {
                input_per_1k_micro: 150,
                output_per_1k_micro: 600
            }
        );
    }
}

#[test]
fn a_response_model_missing_either_rate_or_all_pricing_is_internal() {
    for op in [Operation::Response, Operation::Stream] {
        let missing_output =
            check(|v| set_cost(v, Some(150), None), op).expect_err("no output rate");
        let missing_input = check(|v| set_cost(v, None, Some(600)), op).expect_err("no input rate");
        let missing_cost = check(remove_cost, op).expect_err("no cost");
        for err in [missing_output, missing_input, missing_cost] {
            assert_eq!(code(&err), "internal", "{op:?}");
        }
    }
}

#[test]
fn an_unparseable_rate_is_internal() {
    let err = check(
        |v| {
            v["provider_settings"]["cost"] =
                json!({ "input_per_1k_micro": "x", "output_per_1k_micro": 1 });
        },
        Operation::Response,
    )
    .expect_err("unparseable");
    assert_eq!(code(&err), "internal");
}

#[test]
fn embeddings_require_only_the_input_rate_and_ignore_the_output_rate() {
    let pricing = check(|v| set_cost(v, Some(150), None), Operation::Embedding).expect("priced");
    assert_eq!(
        pricing,
        Pricing {
            input_per_1k_micro: 150,
            output_per_1k_micro: 0
        }
    );
    let ignoring =
        check(|v| set_cost(v, Some(150), Some(600)), Operation::Embedding).expect("priced");
    assert_eq!(
        ignoring.output_per_1k_micro, 0,
        "the output rate is not part of embeddings"
    );
}

#[test]
fn an_embedding_model_missing_the_input_rate_is_internal() {
    let err = check(|v| set_cost(v, None, Some(600)), Operation::Embedding).expect_err("no input");
    assert_eq!(code(&err), "internal");
    let err = check(remove_cost, Operation::Embedding).expect_err("no cost");
    assert_eq!(code(&err), "internal");
}

#[test]
fn zero_embedding_input_rate_is_explicit_zero_even_with_a_nonzero_output_rate() {
    let pricing = check(|v| set_cost(v, Some(0), Some(600)), Operation::Embedding).expect("priced");
    assert!(pricing.is_zero());
}

#[test]
fn mixed_zero_and_nonzero_response_rates_are_not_zero_pricing() {
    let pricing = check(|v| set_cost(v, Some(0), Some(600)), Operation::Response).expect("priced");
    assert!(!pricing.is_zero());
}

#[test]
#[tracing_test::traced_test]
fn the_pricing_failure_is_logged_with_the_model_and_provider_type() {
    let _ =
        check(|v| set_cost(v, Some(150), None), Operation::Response).expect_err("no output rate");
    assert!(logs_contain(OPENAI_TYPE), "provider type is logged");
    assert!(logs_contain("pricing"), "the cause is named");
}

// ── quote ───────────────────────────────────────────────────────────────────

fn priced_guard(input: u64, output: u64) -> (QuotaGuard, Pricing) {
    (
        Harness::new().quota_guard(),
        Pricing {
            input_per_1k_micro: input,
            output_per_1k_micro: output,
        },
    )
}

fn model(patch: impl FnOnce(&mut serde_json::Value)) -> model_registry_sdk::ModelInfoV1 {
    sample_model_info_patched(OPENAI_TYPE, patch)
}

#[test]
fn response_quote_is_input_plus_output_allowance_in_micro_credits() {
    // "hello" = 5 bytes = 2 tokens; the model output limit is 4096 tokens:
    // (2 x 1000 + 4096 x 2000) / 1000 = 8194 price units = 8194 micro-credits.
    let (guard, pricing) = priced_guard(1000, 2000);
    let quote = guard.response_quote(pricing, &model(|_| {}), &body("m"));
    assert_eq!(quote.input_tokens, 2);
    assert_eq!(quote.reserved_units, 8194);
    assert_eq!(quote.pricing, pricing);
}

#[test]
fn response_quote_uses_the_request_output_limit_when_set() {
    let (guard, pricing) = priced_guard(1000, 2000);
    let mut request = body("m");
    request.max_output_tokens = Some(100);
    let quote = guard.response_quote(pricing, &model(|_| {}), &request);
    assert_eq!(quote.reserved_units, 202, "(2 x 1000 + 100 x 2000) / 1000");
}

#[test]
fn response_quote_falls_back_to_the_configured_output_allowance() {
    let mut h = Harness::new();
    h.quota_cfg.fallback_max_output_tokens = 500;
    let guard = h.quota_guard();
    let pricing = Pricing {
        input_per_1k_micro: 1000,
        output_per_1k_micro: 2000,
    };
    let no_limit = model(|v| v["context_window"]["max_output_tokens"] = serde_json::Value::Null);
    let quote = guard.response_quote(pricing, &no_limit, &body("m"));
    assert_eq!(quote.reserved_units, 1002, "(2 x 1000 + 500 x 2000) / 1000");
}

#[test]
fn response_quote_rounds_up_once_to_the_metric_unit() {
    // (2 x 1 + 100 x 1) / 1000 = 0.102 price units -> 1 micro-credit.
    let (guard, pricing) = priced_guard(1, 1);
    let mut request = body("m");
    request.max_output_tokens = Some(100);
    assert_eq!(
        guard
            .response_quote(pricing, &model(|_| {}), &request)
            .reserved_units,
        1
    );
}

#[test]
fn response_quote_follows_the_configured_units_per_credit() {
    let mut h = Harness::new();
    h.quota_cfg.units_per_credit = 1000;
    let guard = h.quota_guard();
    let pricing = Pricing {
        input_per_1k_micro: 1000,
        output_per_1k_micro: 2000,
    };
    let mut request = body("m");
    request.max_output_tokens = Some(100);
    // 202 price units = 0.000202 credit = 0.202 units at 1000/credit -> 1.
    assert_eq!(
        guard
            .response_quote(pricing, &model(|_| {}), &request)
            .reserved_units,
        1
    );
}

#[test]
fn embedding_quote_is_the_input_estimate_only() {
    let (guard, _) = priced_guard(0, 0);
    let pricing = Pricing {
        input_per_1k_micro: 4000,
        output_per_1k_micro: 0,
    };
    let request = llm_gateway_sdk::EmbeddingRequest::new(
        "m",
        llm_gateway_sdk::EmbeddingInput::Single("a".repeat(400)),
    );
    let quote = guard.embedding_quote(pricing, &request);
    assert_eq!(quote.input_tokens, 100);
    assert_eq!(quote.reserved_units, 400, "100 tokens x 4000 / 1000");
}

#[test]
fn zero_pricing_quotes_zero_units() {
    let (guard, pricing) = priced_guard(0, 0);
    assert_eq!(
        guard
            .response_quote(pricing, &model(|_| {}), &body("m"))
            .reserved_units,
        0
    );
}

// ── admission: acquire_lease shape ──────────────────────────────────────────

#[tokio::test]
async fn admit_acquires_a_lease_under_the_caller_context_with_attribution() {
    let h = Harness::new();
    let quota = admit(&h, 500).await;
    assert!(quota.has_lease());

    let acquires = h.quota.acquires();
    assert_eq!(acquires.len(), 1);
    let (subject, request) = &acquires[0];
    assert_eq!(
        *subject,
        Uuid::from_u128(CALLER_USER),
        "caller context, not the service identity"
    );
    assert_eq!(
        request.attribution.tenant_id,
        TenantId::from(Uuid::from_u128(CALLER_TENANT))
    );
    assert_eq!(request.attribution.metric, h.quota_cfg.metric);
    assert_eq!(
        request.attribution.subjects,
        vec![SubjectClaim {
            kind: SCOPE_USER.to_owned(),
            id: Uuid::from_u128(CALLER_USER).to_string(),
        }]
    );
    let metadata = request.attribution.metadata.clone().expect("metadata");
    assert_eq!(
        serde_json::Value::Object(metadata),
        json!({ "model": "openai::gpt-4o", "provider_type": OPENAI_TYPE })
    );
    assert_eq!(request.amount, 500);
    assert!(
        request.idempotency_key.contains("resp_unit"),
        "{}",
        request.idempotency_key
    );
}

#[tokio::test]
async fn lease_ttl_is_the_request_duration_plus_the_margin() {
    let h = Harness::new();
    let guard = h.quota_guard();
    for (op, base) in [
        (Operation::Response, h.timeouts.provider_sync),
        (Operation::Embedding, h.timeouts.provider_sync),
        (Operation::Stream, h.timeouts.stream_max_duration),
    ] {
        guard
            .admit(&ctx(), meta(), op, &quote(10))
            .await
            .expect("admitted");
        let ttl = h.quota.acquires().last().expect("acquire").1.ttl_secs;
        assert_eq!(
            ttl,
            Some((base + h.quota_cfg.lease_ttl_margin).as_secs()),
            "{op:?}"
        );
    }
}

#[tokio::test]
async fn a_zero_amount_is_admitted_without_a_lease_and_without_calling_quota_enforcement() {
    let h = Harness::new();
    let quota = admit(&h, 0).await;
    assert!(!quota.has_lease());
    assert!(h.quota.calls().is_empty());
}

#[tokio::test]
async fn no_applicable_quota_admits_without_a_lease() {
    let h = Harness::new();
    h.quota.script_acquire([Ok(denied("NO_APPLICABLE_QUOTA"))]);
    let quota = admit(&h, 500).await;
    assert!(!quota.has_lease());
}

#[tokio::test]
#[tracing_test::traced_test]
async fn any_other_denial_is_quota_exceeded_and_the_violated_ids_are_logged_not_returned() {
    let h = Harness::new();
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let err = h
        .quota_guard()
        .admit(&ctx(), meta(), Operation::Response, &quote(500))
        .await
        .err()
        .expect("denied");
    assert_eq!(code(&err), "quota_exceeded");
    assert!(
        logs_contain(&Uuid::from_u128(0xBAD).to_string()),
        "violated quota id is logged"
    );
    assert!(logs_contain("resp_unit"), "with the response id");
    assert!(
        !format!("{err:?}").contains(&Uuid::from_u128(0xBAD).to_string()),
        "not returned"
    );
}

#[tokio::test]
async fn acquire_errors_map_by_reason_token() {
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
        (qe_errors::exhausted("ENGINE_COST_EXCEEDED"), "internal"),
    ];
    for (err, expected) in cases {
        let h = Harness::new();
        h.quota.script_acquire([Err(err.clone())]);
        let got = h
            .quota_guard()
            .admit(&ctx(), meta(), Operation::Response, &quote(500))
            .await
            .err()
            .expect("fails");
        assert_eq!(code(&got), expected, "{err:?}");
    }
}

// ── settle: commit and release ──────────────────────────────────────────────

#[tokio::test]
async fn settle_commits_the_actual_quantity_in_micro_credits() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4))); // 0.0005 credit

    eventually("commit", || h.quota.settle_calls() == 1).await;
    let (_, commit) = &h.quota.commits()[0];
    assert_eq!(
        commit.actual_amount,
        Some(500),
        "explicit actual, never the full reservation"
    );
    assert_eq!(commit.token, FakeQuota::token(0));
    assert_eq!(
        commit.tenant_id,
        TenantId::from(Uuid::from_u128(CALLER_TENANT)),
        "tenant passed explicitly"
    );
    assert!(h.quota.releases().is_empty());
}

#[tokio::test]
async fn the_commit_quantity_rounds_up() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(15, 7))); // 1.5 micro-credits
    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(2));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn an_actual_above_the_reservation_commits_the_reservation_and_logs_the_overrun() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(2, 3))); // 2000 micro-credits

    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(1000));
    eventually("report", || !h.sink.reports().is_empty()).await;
    assert_eq!(
        h.sink.reports()[0].quantity_micro_credits,
        2000,
        "the full actual is reported"
    );
    assert!(logs_contain("overrun"));
    assert!(logs_contain("resp_unit"));
    assert!(logs_contain("1000"), "reserved amount");
    assert!(logs_contain("2000"), "actual amount");
}

#[tokio::test]
async fn an_actual_equal_to_the_reservation_commits_it_without_an_overrun() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(1, 3)));
    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(1000));
}

#[tokio::test]
async fn a_zero_actual_releases_the_lease_instead_of_committing() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(Decimal::ZERO));

    eventually("release", || h.quota.settle_calls() == 1).await;
    assert!(h.quota.commits().is_empty());
    let (subject, release) = &h.quota.releases()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(release.token, FakeQuota::token(0));
    assert_eq!(
        release.tenant_id,
        TenantId::from(Uuid::from_u128(CALLER_TENANT))
    );
}

#[tokio::test]
async fn an_actual_that_rounds_to_zero_units_is_not_a_release() {
    // Any positive cost rounds up to at least one unit.
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(1, 20)));
    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(1));
}

#[tokio::test]
async fn settle_runs_under_the_service_identity_not_the_caller() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));
    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].0, Uuid::from_u128(SERVICE_SUBJECT));
    assert!(h.identity.calls() >= 1);
}

#[tokio::test]
async fn settle_has_its_own_idempotency_key_derived_from_the_response_id() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));
    eventually("commit", || h.quota.settle_calls() == 1).await;

    let acquire_key = h.quota.acquires()[0].1.idempotency_key.clone();
    let settle_key = h.quota.commits()[0].1.idempotency_key.clone();
    assert_ne!(acquire_key, settle_key);
    assert!(settle_key.contains("resp_unit"), "{settle_key}");
}

// ── settle: usage hand-off ──────────────────────────────────────────────────

#[tokio::test]
async fn usage_is_handed_to_the_sink_after_the_settle_call() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually("report", || h.sink.reports().len() == 1).await;
    let calls: Vec<&str> = h
        .log
        .entries()
        .into_iter()
        .filter(|e| matches!(*e, "quota_commit" | "quota_release" | "usage_sink"))
        .collect();
    assert_eq!(
        calls,
        ["quota_commit", "usage_sink"],
        "the record never precedes the settle"
    );

    let report = &h.sink.reports()[0];
    assert_eq!(report.response_id, "resp_unit");
    assert_eq!(report.tenant_id, Uuid::from_u128(CALLER_TENANT));
    assert_eq!(report.user_id, Uuid::from_u128(CALLER_USER));
    assert_eq!(report.model, "openai::gpt-4o");
    assert_eq!(report.provider_type, OPENAI_TYPE);
    assert_eq!(report.quantity_micro_credits, 500);
    assert!(report.usage.complete);
    assert!(!report.usage.estimated);
}

#[tokio::test]
async fn the_sink_report_carries_the_estimate_flags_of_the_usage() {
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(Usage::estimated(100, 10, dec(3, 4)));
    eventually("report", || h.sink.reports().len() == 1).await;
    let report = &h.sink.reports()[0];
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
    assert_eq!(report.quantity_micro_credits, 300);
}

#[tokio::test]
async fn a_request_without_a_lease_goes_straight_to_the_sink_with_no_settle() {
    let h = Harness::new();
    let mut quota = admit(&h, 0).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually("report", || h.sink.reports().len() == 1).await;
    assert!(h.quota.calls().is_empty(), "no acquire, commit or release");
    assert_eq!(
        h.sink.reports()[0].quantity_micro_credits,
        500,
        "the plugin's quantity is still recorded"
    );
    assert_eq!(h.identity.calls(), 0, "no service identity needed");
}

#[tokio::test]
async fn no_applicable_quota_requests_report_without_settle() {
    let h = Harness::new();
    h.quota.script_acquire([Ok(denied("NO_APPLICABLE_QUOTA"))]);
    let mut quota = admit(&h, 500).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));
    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.quota.settle_calls(), 0);
}

// ── settle: retry, classification, identity failures (paused clock) ─────────

#[tokio::test(start_paused = true)]
async fn transient_settle_errors_are_retried_with_the_same_key_and_the_backoff() {
    let h = Harness::new();
    h.quota.script_commit([
        Err(qe_errors::unavailable()),
        Err(qe_errors::aborted("LEASE_CONTENTION_TIMEOUT")),
        Err(qe_errors::deadline_exceeded()),
    ]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    let commits = h.quota.commits();
    assert_eq!(commits.len(), 4, "three transient failures, then success");
    let first = &commits[0].1.idempotency_key;
    assert!(
        commits.iter().all(|(_, c)| &c.idempotency_key == first),
        "one key across retries"
    );
    let times = h.quota.settle_times();
    for pair in times.windows(2) {
        assert!(
            pair[1] - pair[0] >= h.quota_cfg.settle_retry_backoff,
            "backoff between attempts"
        );
    }
    assert_eq!(
        h.sink.reports().len(),
        1,
        "reported once, after the settle succeeded"
    );
}

#[tokio::test(start_paused = true)]
async fn release_is_retried_like_commit() {
    let h = Harness::new();
    h.quota.script_release([Err(qe_errors::unavailable())]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(Decimal::ZERO));
    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    assert_eq!(h.quota.releases().len(), 2);
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn permanent_settle_errors_are_logged_and_abandoned_without_retry() {
    for token in ["LEASE_NOT_ACTIVE", "OVER_COMMIT_NOT_AUTHORIZED"] {
        let h = Harness::new();
        h.quota.script_commit([Err(qe_errors::precondition(token))]);
        let mut quota = admit(&h, 1000).await;
        quota.mark_dispatched();
        quota.settle(usage(dec(5, 4)));

        eventually_within(Duration::from_secs(60), "report", || {
            !h.sink.reports().is_empty()
        })
        .await;
        assert_eq!(h.quota.commits().len(), 1, "{token}: no retry");
        assert!(logs_contain(token), "{token}: the reason is logged");
        assert!(logs_contain("resp_unit"));
        assert_eq!(
            h.sink.reports().len(),
            1,
            "usage still handed over after abandoning"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn not_found_is_abandoned_without_retry() {
    let h = Harness::new();
    h.quota.script_commit([Err(qe_errors::not_found())]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));
    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    assert_eq!(h.quota.commits().len(), 1);
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn retries_stop_at_lease_expiry_and_the_failure_is_logged() {
    let mut h = Harness::new();
    h.quota_cfg.settle_retry_backoff = Duration::from_secs(30);
    h.quota
        .script_commit((0..1000).map(|_| Err(qe_errors::unavailable())));
    let mut quota = admit(&h, 1000).await;
    let admitted_at = tokio::time::Instant::now();
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(10_000), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    let ttl = h.timeouts.provider_sync + h.quota_cfg.lease_ttl_margin;
    let times = h.quota.settle_times();
    assert!(
        times.len() >= 15,
        "keeps retrying while the lease is alive: {}",
        times.len()
    );
    assert!(
        *times.last().expect("attempts") <= admitted_at + ttl,
        "no attempt after the lease expired"
    );
    assert!(h.quota.commits().len() < 1000, "stopped, not exhausted");
    assert!(logs_contain("expire"), "the lease left to expire is logged");
    assert_eq!(
        h.sink.reports().len(),
        1,
        "the usage is handed over after abandoning"
    );
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_credentials_exchange_failure_retries_settle_with_backoff_and_is_logged() {
    let mut h = Harness::new();
    h.identity = crate::domain::test_support::FakeIdentity::failing_first(2);
    let mut quota = admit(&h, 1000).await;
    let started = tokio::time::Instant::now();
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    assert_eq!(
        h.identity.calls(),
        3,
        "two failures, then the exchange works"
    );
    assert_eq!(h.quota.commits().len(), 1);
    assert!(started.elapsed() >= 2 * h.quota_cfg.settle_retry_backoff);
    assert!(logs_contain("credentials exchange failed"));
}

#[tokio::test(start_paused = true)]
async fn an_unauthenticated_settle_re_exchanges_the_identity_and_retries_at_once() {
    let h = Harness::new();
    h.quota.script_commit([Err(qe_errors::unauthenticated())]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    assert_eq!(
        h.identity.invalidations(),
        1,
        "the refused context was dropped"
    );
    let commits = h.quota.commits();
    assert_eq!(commits.len(), 2, "one refusal, then the retry succeeds");
    assert_eq!(
        commits[0].1.idempotency_key, commits[1].1.idempotency_key,
        "same settle key"
    );
    let times = h.quota.settle_times();
    assert!(
        times[1] - times[0] < h.quota_cfg.settle_retry_backoff,
        "re-exchanged and retried without the backoff"
    );
    assert_eq!(h.sink.reports().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn repeated_unauthenticated_refusals_retry_at_once_only_the_first_time() {
    let h = Harness::new();
    h.quota
        .script_commit((0..4).map(|_| Err(qe_errors::unauthenticated())));
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    let times = h.quota.settle_times();
    assert_eq!(times.len(), 5, "four refusals, then success");
    let backoff = h.quota_cfg.settle_retry_backoff;
    assert!(
        times[1] - times[0] < backoff,
        "the first refusal retries at once"
    );
    for pair in times[1..].windows(2) {
        assert!(
            pair[1] - pair[0] >= backoff,
            "later refusals back off instead of forcing another exchange"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_unauthenticated_settle_invalidates_the_generation_it_used() {
    let h = Harness::new();
    h.quota.script_commit([
        Err(qe_errors::unauthenticated()),
        Err(qe_errors::unauthenticated()),
    ]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(60), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    // The fake hands out a new generation after every invalidation.
    assert_eq!(h.identity.invalidated_generations(), [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn the_settle_backoff_is_jittered_within_twenty_percent() {
    let h = Harness::new();
    let mut quota = h
        .quota_guard()
        .with_backoff_jitter(0.2)
        .admit(&ctx(), meta(), Operation::Response, &quote(1000))
        .await
        .expect("admitted");
    h.quota
        .script_commit((0..30).map(|_| Err(qe_errors::unavailable())));
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));

    eventually_within(Duration::from_secs(600), "report", || {
        !h.sink.reports().is_empty()
    })
    .await;
    let times = h.quota.settle_times();
    let base = h.quota_cfg.settle_retry_backoff;
    let gaps: Vec<Duration> = times.windows(2).map(|p| p[1] - p[0]).collect();
    assert_eq!(gaps.len(), 30);
    assert!(
        gaps.iter()
            .all(|g| *g >= base.mul_f64(0.8) && *g <= base.mul_f64(1.2)),
        "{gaps:?}"
    );
    assert!(
        gaps.iter().any(|g| *g != gaps[0]),
        "the delays are not all identical: {gaps:?}"
    );
}

#[test]
fn an_unauthenticated_settle_error_is_its_own_class() {
    assert_eq!(
        classify_settle_error(&qe_errors::unauthenticated()),
        SettleErrorClass::Unauthenticated
    );
}

// ── cut requests: dropping without settle ───────────────────────────────────

#[tokio::test]
async fn dropping_before_dispatch_releases_the_lease_and_reports_no_usage() {
    let h = Harness::new();
    let quota = admit(&h, 1000).await;
    drop(quota);

    eventually("release", || h.quota.releases().len() == 1).await;
    assert!(h.quota.commits().is_empty());
    assert!(h.sink.reports().is_empty(), "nothing was dispatched");
}

#[tokio::test]
async fn explicit_release_releases_the_lease_and_reports_no_usage() {
    let h = Harness::new();
    let quota = admit(&h, 1000).await;
    quota.release();

    eventually("release", || h.quota.releases().len() == 1).await;
    let (subject, release) = &h.quota.releases()[0];
    assert_eq!(*subject, Uuid::from_u128(SERVICE_SUBJECT));
    assert_eq!(
        release.tenant_id,
        TenantId::from(Uuid::from_u128(CALLER_TENANT))
    );
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn release_without_a_lease_calls_nothing() {
    let h = Harness::new();
    let quota = admit(&h, 0).await;
    quota.release();
    tokio::task::yield_now().await;
    assert!(h.quota.calls().is_empty());
    assert!(h.sink.reports().is_empty());
}

#[tokio::test]
async fn dropping_after_dispatch_settles_with_the_input_only_estimate() {
    // 10 input tokens at 1000 per 1K = 10 price units = 10 micro-credits.
    let h = Harness::new();
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    drop(quota);

    eventually("report", || h.sink.reports().len() == 1).await;
    assert_eq!(h.quota.commits().len(), 1);
    assert_eq!(h.quota.commits()[0].1.actual_amount, Some(10));
    let report = &h.sink.reports()[0];
    assert!(!report.usage.complete);
    assert!(report.usage.estimated);
    assert_eq!(report.quantity_micro_credits, 10);
}

#[tokio::test]
async fn the_estimator_of_a_request_carries_its_input_tokens_and_pricing() {
    let h = Harness::new();
    let quota = admit(&h, 1000).await;
    let estimator = quota.estimator();
    assert_eq!(estimator.input_tokens, 10);
    assert_eq!(estimator.pricing, quote(1000).pricing);
}

#[tokio::test]
async fn acquired_outcomes_from_the_script_are_used_as_is() {
    // Sanity of the fake: a scripted lease token reaches the settle call.
    let h = Harness::new();
    let token = quota_enforcement_sdk::LeaseToken::from(Uuid::from_u128(0xFEED));
    h.quota.script_acquire([Ok(acquired(token, 660))]);
    let mut quota = admit(&h, 1000).await;
    quota.mark_dispatched();
    quota.settle(usage(dec(5, 4)));
    eventually("commit", || h.quota.settle_calls() == 1).await;
    assert_eq!(h.quota.commits()[0].1.token, token);
    assert!(matches!(h.quota.calls()[0], QuotaCall::Acquire { .. }));
}
