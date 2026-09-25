#![allow(clippy::expect_used)]
//! The four consumption endpoints over the shared service: a denial is HTTP
//! 200 with a body, server-derived fields a caller echoes back are ignored, and
//! an unusable amount is an actionable 400 rather than a 422.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    EnforcementMode, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType,
    SubjectRef,
};
use serde_json::{Value, json};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit_security::AccessScope;
use tower::ServiceExt as _;

use super::super::routes::{PATH_PREFIX, register_routes};
use crate::domain::Service;
use crate::test_support::{
    LLM_TOKEN_CONSTRAINT, LLM_USER_PROJECTION, METRIC_TOKENS, PermitTenantsPdp, bound_service_over,
    ctx, tenant,
};

async fn seeded() -> (Router, Arc<InMemoryStorage>, QuotaId) {
    let storage = Arc::new(InMemoryStorage::new());
    let mut bundle = bundle_with_global_policy();
    if let Some(policy) = bundle.global_policy.as_mut() {
        policy.engine_id = "most-restrictive-wins".to_owned();
    }
    storage.bootstrap(&bundle).await.expect("bootstrap");
    let quota = storage
        .create_quota(
            &ctx(),
            &AccessScope::allow_all(),
            QuotaDraft {
                tenant_id: tenant(),
                subject: SubjectRef {
                    projection_type: gts::GtsTypeId::new(LLM_USER_PROJECTION),
                    subject_id: "u-1".to_owned(),
                },
                metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
                quota_type: QuotaType::Allocation,
                period: None,
                enforcement_mode: EnforcementMode::Hard,
                cap: Some(100),
                notification_thresholds: Vec::new(),
                validity_window: None,
                fail_open_hint: false,
                metadata: serde_json::Map::new(),
                source: QuotaSource::Operator,
                constraint_contract: quota_enforcement_sdk::ContractRef {
                    type_id: gts::GtsTypeId::new(LLM_TOKEN_CONSTRAINT),
                    version: 1,
                },
            },
            &[],
        )
        .await
        .expect("quota");
    let service: Arc<Service> = bound_service_over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        Arc::clone(&storage),
    )
    .await;
    let app = register_routes(Router::new(), &OpenApiRegistryImpl::new(), service)
        .layer(Extension(ctx()));
    (app, storage, quota)
}

async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("{PATH_PREFIX}{path}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn attribution() -> Value {
    json!({
        "tenant_id": tenant().as_uuid(),
        "metric": METRIC_TOKENS,
        "subjects": [{ "kind": quota_enforcement_sdk::SCOPE_USER, "id": "u-1" }],
        "metadata": { "region": "eu-west-1" }
    })
}

#[tokio::test]
async fn a_debit_answers_two_hundred_with_its_decision() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = post(
        &app,
        "/operations/debit",
        json!({ "attribution": attribution(), "amount": 10, "idempotency_key": "k1" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["outcome"], json!("allowed"));
    assert_eq!(body["debit_plan"][0]["amount"], json!(10));
    assert_eq!(storage.consumed(quota), 10);
}

#[tokio::test]
async fn a_denial_is_also_two_hundred_with_a_decision_body() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = post(
        &app,
        "/operations/debit",
        json!({ "attribution": attribution(), "amount": 500, "idempotency_key": "k1" }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a denial is a successful call, not a Problem"
    );
    assert_eq!(body["result"]["outcome"], json!("denied"));
    assert!(body["result"]["reason"].is_string());
    assert_eq!(storage.consumed(quota), 0);
}

#[tokio::test]
async fn decision_shaped_fields_a_caller_echoed_back_are_ignored() {
    let (app, _storage, _quota) = seeded().await;
    let fresh = json!({
        "attribution": attribution(),
        "amount": 10,
        "idempotency_key": "k1",
        "result": { "outcome": "allowed" },
        "debit_plan": [{ "quota_id": uuid::Uuid::nil(), "amount": 9999 }],
        "diagnostics": { "engine": "noise" }
    });

    let (status, echoed) = post(&app, "/operations/debit", fresh.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // The replay of the same key must be byte-identical to the fresh answer,
    // and neither may reflect anything the caller sent.
    let (replay_status, replayed) = post(&app, "/operations/debit", fresh).await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(replayed, echoed);
    assert_eq!(echoed["debit_plan"][0]["amount"], json!(10));
    assert_eq!(echoed["diagnostics"].get("engine"), None);
}

#[tokio::test]
async fn a_zero_amount_is_an_actionable_four_hundred() {
    let (app, _storage, quota) = seeded().await;

    for (path, body) in [
        (
            "/operations/debit",
            json!({ "attribution": attribution(), "amount": 0, "idempotency_key": "k1" }),
        ),
        (
            "/operations/evaluate",
            json!({ "attribution": attribution(), "amount": 0 }),
        ),
        (
            "/operations/credit",
            json!({
                "tenant_id": tenant().as_uuid(),
                "quota_id": quota.as_uuid(),
                "amount": 0,
                "idempotency_key": "c1"
            }),
        ),
    ] {
        let (status, _body) = post(&app, path, body).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{path} must answer 400, not a deserialization 422"
        );
    }
}

#[tokio::test]
async fn a_replay_with_a_different_payload_is_a_conflict() {
    let (app, _storage, _quota) = seeded().await;
    post(
        &app,
        "/operations/debit",
        json!({ "attribution": attribution(), "amount": 10, "idempotency_key": "k1" }),
    )
    .await;

    let (status, _body) = post(
        &app,
        "/operations/debit",
        json!({ "attribution": attribution(), "amount": 11, "idempotency_key": "k1" }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_rollback_of_an_unknown_operation_is_four_oh_four() {
    let (app, _storage, _quota) = seeded().await;

    let (status, body) = post(
        &app,
        "/operations/rollback",
        json!({
            "attribution": attribution(),
            "original_idempotency_key": "never-happened",
            "idempotency_key": "r1"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.to_string().contains("operation"),
        "the problem names what was not found: {body}"
    );
}

#[tokio::test]
async fn a_preview_carries_no_key_and_says_it_is_one() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = post(
        &app,
        "/operations/evaluate",
        json!({ "attribution": attribution(), "amount": 10 }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["preview"], json!(true));
    assert_eq!(body["result"]["outcome"], json!("allowed"));
    assert_eq!(storage.consumed(quota), 0, "a preview mutates nothing");
}

#[tokio::test]
async fn a_credit_and_a_rollback_round_trip_over_http() {
    let (app, storage, quota) = seeded().await;
    post(
        &app,
        "/operations/debit",
        json!({ "attribution": attribution(), "amount": 30, "idempotency_key": "k1" }),
    )
    .await;

    let (credit_status, _) = post(
        &app,
        "/operations/credit",
        json!({
            "tenant_id": tenant().as_uuid(),
            "quota_id": quota.as_uuid(),
            "amount": 10,
            "idempotency_key": "c1"
        }),
    )
    .await;
    assert_eq!(credit_status, StatusCode::OK);
    assert_eq!(storage.consumed(quota), 20);

    let (rollback_status, _) = post(
        &app,
        "/operations/rollback",
        json!({
            "attribution": attribution(),
            "original_idempotency_key": "k1",
            "idempotency_key": "r1"
        }),
    )
    .await;
    assert_eq!(rollback_status, StatusCode::OK);
    assert_eq!(storage.consumed(quota), 0, "the reversal floors at zero");
}

#[tokio::test]
async fn the_four_operations_are_declared_in_the_openapi_document() {
    let openapi = OpenApiRegistryImpl::new();
    let service = bound_service_over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        Arc::new(InMemoryStorage::new()),
    )
    .await;
    let _router = register_routes(Router::new(), &openapi, service);

    let document = serde_json::to_value(
        openapi
            .build_openapi(&toolkit::api::openapi_registry::OpenApiInfo::default())
            .expect("document"),
    )
    .expect("json");
    let text = document.to_string();
    for id in [
        "quota_enforcement.debit",
        "quota_enforcement.credit",
        "quota_enforcement.rollback",
        "quota_enforcement.evaluate_preview",
    ] {
        assert!(
            text.contains(id),
            "{id} is missing from the OpenAPI document"
        );
    }
}
