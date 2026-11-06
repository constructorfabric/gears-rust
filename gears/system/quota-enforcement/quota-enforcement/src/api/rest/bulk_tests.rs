#![allow(clippy::expect_used)]
//! The bulk Quota endpoints over the shared service: a committed envelope
//! answers 200 with one summary per item, a failing item is a `Problem`
//! naming `items[index]`, and the in-process client takes the same path.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use quota_enforcement_sdk::{
    BulkDeactivateItem, BulkDeactivateQuotasRequest, QuotaManagerClientV1, QuotaStatus,
};
use serde_json::{Value, json};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use tower::ServiceExt as _;

use super::super::routes::{PATH_PREFIX, register_routes};
use crate::domain::Service;
use crate::test_support::{
    LLM_USER_PROJECTION, METRIC_TOKENS, PermitTenantsPdp, bound_service, ctx, tenant,
};

async fn service() -> Arc<Service> {
    bound_service(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await
}

fn app(service: Arc<Service>) -> Router {
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), service).layer(Extension(ctx()))
}

async fn post(app: &Router, path: &str, body: &Value) -> (StatusCode, Value) {
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn quota(subject: &str) -> Value {
    json!({
        "tenant_id": tenant().as_uuid(),
        "subject": { "projection_type": LLM_USER_PROJECTION, "subject_id": subject },
        "metric": METRIC_TOKENS,
        "quota_type": "gts.cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1",
        "period": "gts.cf.core.qe.period_type.v1~cf.core.qe.month.v1",
        "enforcement_mode": "gts.cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1",
        "cap": 100,
        "notification_thresholds": [50],
        "metadata": { "regions": ["eu"], "weight": 5 },
        "source": "gts.cf.core.qe.source_type.v1~cf.core.qe.operator.v1"
    })
}

fn create_body(key: &str, quotas: Vec<Value>) -> Value {
    json!({
        "tenant_id": tenant().as_uuid(),
        "idempotency_key": key,
        "items": quotas
            .into_iter()
            .enumerate()
            .map(|(index, quota)| json!({ "idempotency_key": format!("seat-{index}"), "quota": quota }))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn a_bulk_create_answers_one_summary_per_item_and_replays_it() {
    let app = app(service().await);
    let body = create_body("pack", vec![quota("u1"), quota("u2")]);
    let (status, created) = post(&app, "/quotas/bulk-create", &body).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(created["items"][1]["index"], json!(1));
    assert_eq!(created["items"][1]["idempotency_key"], json!("seat-1"));

    let (status, replayed) = post(&app, "/quotas/bulk-create", &body).await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, created, "the stored outcome, verbatim");
}

#[tokio::test]
async fn a_failing_item_is_a_problem_naming_it() {
    let app = app(service().await);
    let mut bad = quota("u2");
    bad["cap"] = json!(-1);
    let (status, problem) = post(
        &app,
        "/quotas/bulk-create",
        &create_body("pack", vec![quota("u1"), bad]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(
        problem.to_string().contains("items[1].cap"),
        "the problem names the item: {problem}"
    );
}

#[tokio::test]
async fn an_envelope_over_the_configured_limit_is_bulk_too_large() {
    let app = app(service().await);
    let quotas = (0..51).map(|index| quota(&format!("u{index}"))).collect();
    let (status, problem) = post(&app, "/quotas/bulk-create", &create_body("pack", quotas)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(problem.to_string().contains("BULK_TOO_LARGE"), "{problem}");
}

#[tokio::test]
async fn bulk_update_and_deactivate_answer_their_summaries() {
    let app = app(service().await);
    let (_, created) = post(
        &app,
        "/quotas/bulk-create",
        &create_body("pack", vec![quota("u1"), quota("u2")]),
    )
    .await;
    let ids: Vec<Value> = created["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["quota_id"].clone())
        .collect();

    let (status, updated) = post(
        &app,
        "/quotas/bulk-update",
        &json!({
            "tenant_id": tenant().as_uuid(),
            "idempotency_key": "raise",
            "items": ids.iter().map(|id| json!({ "quota_id": id, "patch": { "cap": 200 } })).collect::<Vec<_>>(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["items"][0]["record_version"], json!(2));

    let (status, deactivated) = post(
        &app,
        "/quotas/bulk-deactivate",
        &json!({
            "tenant_id": tenant().as_uuid(),
            "idempotency_key": "off",
            "items": ids.iter().map(|id| json!({ "quota_id": id })).collect::<Vec<_>>(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deactivated}");
    assert_eq!(deactivated["items"][1]["quota_id"], ids[1]);
    assert_eq!(deactivated["items"][1]["resolved_leases"], json!([]));
}

#[tokio::test]
async fn the_in_process_client_takes_the_same_path() {
    let service = service().await;
    let quotas = service.quotas().expect("bound");
    let id = quotas
        .create(&ctx(), {
            let body: super::super::dto::CreateQuotaDto =
                serde_json::from_value(quota("u1")).expect("dto");
            crate::domain::quotas::CreateQuotaRequest::try_from(body).expect("request")
        })
        .await
        .expect("create")
        .quota
        .id;
    let client = crate::api::in_process::InProcessQuotaManager::new(Arc::clone(&service));
    let outcome = client
        .bulk_deactivate_quotas(
            &ctx(),
            BulkDeactivateQuotasRequest {
                tenant_id: tenant(),
                idempotency_key: "off".to_owned(),
                items: vec![BulkDeactivateItem {
                    idempotency_key: Some("only".to_owned()),
                    quota_id: id,
                }],
            },
        )
        .await
        .expect("bulk deactivate");
    assert_eq!(outcome.items[0].idempotency_key.as_deref(), Some("only"));
    let view = service
        .quotas()
        .expect("bound")
        .get(&ctx(), id)
        .await
        .expect("read");
    assert_eq!(view.quota.status, QuotaStatus::Deactivated);
}

#[tokio::test]
async fn an_oversized_envelope_is_bulk_too_large_even_when_an_item_is_malformed() {
    let app = app(service().await);
    let mut quotas: Vec<Value> = (0..501).map(|index| quota(&format!("u{index}"))).collect();
    quotas[0]["subject"]["projection_type"] = json!("not a type id");
    let (status, problem) = post(&app, "/quotas/bulk-create", &create_body("pack", quotas)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(problem.to_string().contains("BULK_TOO_LARGE"), "{problem}");
}
