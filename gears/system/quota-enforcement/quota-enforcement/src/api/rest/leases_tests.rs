#![allow(clippy::expect_used)]
//! The three lease endpoints over the shared service: an acquisition answers
//! 200 with a token or a denial, commit and release take the token from the
//! path and the tenant from the body, and bad amounts or TTLs are actionable
//! 400s rather than 422s.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    EnforcementMode, LeaseState, LeaseToken, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId,
    QuotaSource, QuotaType, SubjectRef,
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

async fn acquire(app: &Router, amount: i64, ttl_secs: Value, key: &str) -> (StatusCode, Value) {
    post(
        app,
        "/leases",
        json!({
            "attribution": attribution(),
            "amount": amount,
            "ttl_secs": ttl_secs,
            "idempotency_key": key
        }),
    )
    .await
}

fn token_of(body: &Value) -> LeaseToken {
    let raw = body["token"].as_str().expect("a token");
    LeaseToken::new(uuid::Uuid::parse_str(raw).expect("uuid"))
}

#[tokio::test]
async fn an_acquisition_answers_two_hundred_with_a_token_and_its_expiry() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = acquire(&app, 30, json!(60), "a1").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], json!("acquired"));
    assert!(body["expires_at"].is_string());
    assert_eq!(
        storage.lease_state(token_of(&body)),
        Some(LeaseState::Active)
    );
    assert_eq!(storage.consumed(quota), 30);
}

#[tokio::test]
async fn a_denied_acquisition_is_two_hundred_with_its_decision() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = acquire(&app, 500, json!(60), "a1").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a denial is a verdict, not a Problem"
    );
    assert_eq!(body["outcome"], json!("denied"));
    assert_eq!(body["decision"]["result"]["outcome"], json!("denied"));
    assert_eq!(storage.consumed(quota), 0);
}

#[tokio::test]
async fn a_bad_amount_or_ttl_is_a_four_hundred_not_a_four_twenty_two() {
    let (app, _storage, _quota) = seeded().await;
    for (amount, ttl) in [
        (0, json!(60)),
        (10, Value::Null),
        (10, json!(0)),
        (10, json!(3_601)),
    ] {
        let (status, _body) = acquire(&app, amount, ttl.clone(), "a1").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "amount {amount}, ttl {ttl}"
        );
    }
}

#[tokio::test]
async fn commit_and_release_settle_the_lease_named_in_the_path() {
    let (app, storage, quota) = seeded().await;
    let (_, first) = acquire(&app, 30, json!(60), "a1").await;
    let (_, second) = acquire(&app, 20, json!(60), "a2").await;
    let (committed, released) = (token_of(&first), token_of(&second));

    let (status, body) = post(
        &app,
        &format!("/leases/{}/commit", committed.as_uuid()),
        json!({ "tenant_id": tenant().as_uuid(), "actual_amount": 12, "idempotency_key": "c1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["outcome"], json!("allowed"));
    assert_eq!(storage.lease_state(committed), Some(LeaseState::Committed));

    let (status, _body) = post(
        &app,
        &format!("/leases/{}/release", released.as_uuid()),
        json!({ "tenant_id": tenant().as_uuid(), "idempotency_key": "r1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(storage.lease_state(released), Some(LeaseState::Released));
    assert_eq!(
        storage.consumed(quota),
        12,
        "only the committed share stays"
    );

    // Settling it again under a new key: nothing is active any more.
    let (status, _body) = post(
        &app,
        &format!("/leases/{}/release", committed.as_uuid()),
        json!({ "tenant_id": tenant().as_uuid(), "idempotency_key": "r2" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "LEASE_NOT_ACTIVE");
}

#[tokio::test]
async fn an_unknown_token_is_a_four_oh_four() {
    let (app, _storage, _quota) = seeded().await;

    let (status, _body) = post(
        &app,
        &format!("/leases/{}/commit", uuid::Uuid::now_v7()),
        json!({ "tenant_id": tenant().as_uuid(), "idempotency_key": "c1" }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_negative_commit_is_a_four_hundred() {
    let (app, _storage, _quota) = seeded().await;
    let (_, body) = acquire(&app, 10, json!(60), "a1").await;

    let (status, _body) = post(
        &app,
        &format!("/leases/{}/commit", token_of(&body).as_uuid()),
        json!({ "tenant_id": tenant().as_uuid(), "actual_amount": -1, "idempotency_key": "c1" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}
