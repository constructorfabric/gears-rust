#![allow(clippy::expect_used)]
//! The batch debit endpoint over the shared service: a verdict answers 200
//! whether the batch was allowed or denied, a bad item amount is a 400
//! naming the item, and `independent` mode answers 501.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    BatchDebitRequest, BatchItemRequest, BatchMode, BatchResult, EnforcementMode,
    EvaluationAttribution, QuotaDraft, QuotaEnforcementClientV1, QuotaEnforcementStoragePluginV1,
    QuotaId, QuotaSource, QuotaType, SubjectClaim, SubjectRef,
};
use serde_json::{Value, json};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit_canonical_errors::Problem;
use toolkit_security::AccessScope;
use tower::ServiceExt as _;

use super::super::routes::{PATH_PREFIX, register_routes};
use crate::domain::Service;
use crate::test_support::{
    LLM_TOKEN_CONSTRAINT, LLM_USER_PROJECTION, METRIC_TOKENS, PermitTenantsPdp, bound_service_over,
    ctx, tenant,
};

async fn seeded() -> (Router, Arc<InMemoryStorage>, QuotaId) {
    let (service, storage, quota) = seeded_service().await;
    let app = register_routes(Router::new(), &OpenApiRegistryImpl::new(), service)
        .layer(Extension(ctx()));
    (app, storage, quota)
}

async fn seeded_service() -> (Arc<Service>, Arc<InMemoryStorage>, QuotaId) {
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
    (service, storage, quota)
}

async fn batch_debit(app: &Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("{PATH_PREFIX}/operations/batch-debit"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn item(amount: i64, key: &str) -> Value {
    json!({
        "attribution": {
            "tenant_id": tenant().as_uuid(),
            "metric": METRIC_TOKENS,
            "subjects": [{ "kind": quota_enforcement_sdk::SCOPE_USER, "id": "u-1" }],
            "metadata": { "region": "eu-west-1" }
        },
        "amount": amount,
        "idempotency_key": key
    })
}

fn body(mode: &str, items: &[Value]) -> Value {
    json!({ "mode": mode, "items": items, "idempotency_key": "b1" })
}

#[tokio::test]
async fn an_allowed_batch_answers_two_hundred_with_every_item() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = batch_debit(&app, body("atomic", &[item(30, "i1"), item(40, "i2")])).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], json!("allowed"));
    assert_eq!(body["items"][0]["idempotency_key"], json!("i1"));
    assert_eq!(
        body["items"][1]["decision"]["result"]["outcome"],
        json!("allowed")
    );
    assert_eq!(storage.consumed(quota), 70);
}

#[tokio::test]
async fn a_denied_batch_is_two_hundred_with_every_item_s_decision() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = batch_debit(&app, body("atomic", &[item(60, "i1"), item(60, "i2")])).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a denial is a verdict, not a Problem"
    );
    assert_eq!(body["result"], json!("denied"));
    assert_eq!(
        body["items"][0]["decision"]["result"]["outcome"],
        json!("allowed")
    );
    assert_eq!(
        body["items"][1]["decision"]["result"]["outcome"],
        json!("denied")
    );
    assert_eq!(storage.consumed(quota), 0);
}

#[tokio::test]
async fn a_bad_item_amount_is_a_four_hundred_naming_the_item() {
    let (app, _storage, _quota) = seeded().await;

    let (status, body) = batch_debit(&app, body("atomic", &[item(5, "i1"), item(-1, "i2")])).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("items[1].amount"),
        "the problem names the item: {body}"
    );
}

#[tokio::test]
async fn independent_mode_answers_five_oh_one() {
    let (app, storage, quota) = seeded().await;

    let (status, body) = batch_debit(&app, body("independent", &[item(5, "i1")])).await;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(storage.consumed(quota), 0);
}

#[tokio::test]
async fn the_in_process_client_answers_through_the_same_domain_path() {
    let (service, storage, quota) = seeded_service().await;
    let client = crate::api::in_process::InProcessQuotaEnforcement::new(service);
    let item = |amount: i64, key: &str| BatchItemRequest {
        attribution: EvaluationAttribution {
            tenant_id: tenant(),
            metric: METRIC_TOKENS.to_owned(),
            subjects: vec![SubjectClaim {
                kind: quota_enforcement_sdk::SCOPE_USER.to_owned(),
                id: "u-1".to_owned(),
            }],
            metadata: json!({ "region": "eu-west-1" }).as_object().cloned(),
            resource: None,
        },
        amount,
        idempotency_key: key.to_owned(),
    };

    let decision = client
        .batch_debit(
            &ctx(),
            BatchDebitRequest {
                mode: BatchMode::Atomic,
                items: vec![item(30, "i1"), item(40, "i2")],
                idempotency_key: "b1".to_owned(),
            },
        )
        .await
        .expect("batch");
    let error = client
        .batch_debit(
            &ctx(),
            BatchDebitRequest {
                mode: BatchMode::Independent,
                items: vec![item(5, "i1")],
                idempotency_key: "b2".to_owned(),
            },
        )
        .await
        .expect_err("independent");

    assert_eq!(decision.result, BatchResult::Allowed);
    assert_eq!(storage.consumed(quota), 70);
    assert_eq!(Problem::from(error).status, Some(501));
}

#[tokio::test]
async fn decision_shaped_fields_in_the_batch_or_its_items_are_ignored() {
    let (app, storage, quota) = seeded().await;
    let mut forged = item(60, "i2");
    forged["decision"] = json!({ "result": { "outcome": "allowed" } });
    forged["debit_plan"] = json!([{ "quota_id": quota.as_uuid(), "amount": 0 }]);
    let mut request = body("atomic", &[item(60, "i1"), forged]);
    request["result"] = json!("allowed");

    let (status, body) = batch_debit(&app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["result"],
        json!("denied"),
        "the forged verdict is ignored"
    );
    assert_eq!(storage.consumed(quota), 0);
}
