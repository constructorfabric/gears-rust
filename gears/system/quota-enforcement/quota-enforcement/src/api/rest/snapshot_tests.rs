#![allow(clippy::expect_used)]
//! The snapshot endpoint over the shared service: one page of per-Quota
//! state with no policy attribution, the same shape for every caller, an
//! empty page for a target matching nothing, and a `Problem` for every
//! failure.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    EnforcementMode, QuotaDraft, QuotaEnforcementClientV1, QuotaEnforcementStoragePluginV1,
    QuotaSource, QuotaType, SCOPE_TENANT, SCOPE_USER, SnapshotRequest, SnapshotSubject, SubjectRef,
};
use serde_json::{Value, json};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit_canonical_errors::Problem;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt as _;
use uuid::Uuid;

use super::super::routes::{PATH_PREFIX, register_routes};
use crate::domain::Service;
use crate::test_support::{
    LLM_TENANT_PROJECTION, LLM_TOKEN_CONSTRAINT, LLM_USER_PROJECTION, METRIC_REQUESTS,
    METRIC_TOKENS, PermitTenantsPdp, bound_service_over, ctx, tenant,
};

fn draft(subject: SubjectRef) -> QuotaDraft {
    QuotaDraft {
        tenant_id: tenant(),
        subject,
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
    }
}

async fn seeded_service() -> (Arc<Service>, Arc<InMemoryStorage>) {
    let storage = Arc::new(InMemoryStorage::new());
    let mut bundle = bundle_with_global_policy();
    if let Some(policy) = bundle.global_policy.as_mut() {
        policy.engine_id = "most-restrictive-wins".to_owned();
    }
    storage.bootstrap(&bundle).await.expect("bootstrap");
    for subject in [
        SubjectRef {
            projection_type: gts::GtsTypeId::new(LLM_USER_PROJECTION),
            subject_id: "u-1".to_owned(),
        },
        SubjectRef {
            projection_type: gts::GtsTypeId::new(LLM_TENANT_PROJECTION),
            subject_id: tenant().to_string(),
        },
    ] {
        storage
            .create_quota(&ctx(), &AccessScope::allow_all(), draft(subject), &[])
            .await
            .expect("quota");
    }
    let service = bound_service_over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        Arc::clone(&storage),
    )
    .await;
    (service, storage)
}

fn app(service: Arc<Service>, caller: SecurityContext) -> Router {
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), service).layer(Extension(caller))
}

async fn snapshot(app: &Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("{PATH_PREFIX}/snapshot"))
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

fn body(subjects: &[(&str, &str)]) -> Value {
    json!({
        "tenant_id": tenant().as_uuid(),
        "subjects": subjects
            .iter()
            .map(|(kind, id)| json!({ "kind": kind, "id": id, "metric": METRIC_TOKENS }))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn a_page_carries_per_quota_state_and_no_policy_attribution() {
    let (service, _storage) = seeded_service().await;
    let app = app(service, ctx());

    let (status, page) = snapshot(&app, body(&[(SCOPE_USER, "u-1")])).await;

    assert_eq!(status, StatusCode::OK, "{page}");
    let items = page["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "the user's and the tenant's Quota");
    let expected: BTreeSet<&str> = [
        "quota_id",
        "subject",
        "metric",
        "quota_type",
        "enforcement_mode",
        "cap",
        "consumed",
        "remaining",
        "period",
        "metadata",
        "validity_window",
        "currently_within_window",
    ]
    .into_iter()
    .collect();
    for item in items {
        let keys: BTreeSet<&str> = item
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, expected, "exactly the PRD 5.10 fields: {item}");
    }
    let page_keys: BTreeSet<&str> = page
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        page_keys,
        ["items", "next_cursor"].into_iter().collect(),
        "no aggregate figure beside the list"
    );
}

#[tokio::test]
async fn an_operator_and_a_backend_get_the_same_state_shape() {
    let (service, _storage) = seeded_service().await;
    let backend = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xbacc))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("backend context");

    let (_, as_operator) = snapshot(
        &app(Arc::clone(&service), ctx()),
        body(&[(SCOPE_USER, "u-1")]),
    )
    .await;
    let (_, as_backend) = snapshot(&app(service, backend), body(&[(SCOPE_USER, "u-1")])).await;

    assert_eq!(as_operator, as_backend);
}

#[tokio::test]
async fn a_target_matching_nothing_is_two_hundred_with_an_empty_page() {
    let (service, _storage) = seeded_service().await;
    // Nothing is provisioned on this metric, at either tier.
    let (status, page) = snapshot(
        &app(service, ctx()),
        json!({
            "tenant_id": tenant().as_uuid(),
            "subjects": [{ "kind": SCOPE_USER, "id": "u-1", "metric": METRIC_REQUESTS }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"], json!([]));
    assert_eq!(page["next_cursor"], Value::Null);
}

#[tokio::test]
async fn a_bad_target_is_a_four_hundred_naming_the_subject() {
    let (service, _storage) = seeded_service().await;
    let app = app(service, ctx());

    let (status, problem) = snapshot(
        &app,
        body(&[
            (SCOPE_USER, "u-1"),
            (SCOPE_TENANT, &Uuid::from_u128(0xbad).to_string()),
        ]),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let rendered = problem.to_string();
    assert!(rendered.contains("subjects[1].id"), "{rendered}");
    assert!(rendered.contains("SNAPSHOT_TENANT_MISMATCH"), "{rendered}");
    assert!(problem.get("items").is_none(), "a Problem, never a page");
}

#[tokio::test]
async fn the_in_process_client_answers_through_the_same_domain_path() {
    let (service, _storage) = seeded_service().await;
    let client = crate::api::in_process::InProcessQuotaEnforcement::new(service);

    let page = client
        .snapshot(
            &ctx(),
            SnapshotRequest {
                tenant_id: tenant(),
                subjects: vec![SnapshotSubject {
                    kind: SCOPE_TENANT.to_owned(),
                    id: tenant().to_string(),
                    metric: METRIC_TOKENS.to_owned(),
                }],
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect("snapshot");
    let error = client
        .snapshot(
            &ctx(),
            SnapshotRequest {
                tenant_id: tenant(),
                subjects: Vec::new(),
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect_err("no subjects");

    assert_eq!(page.items.len(), 1, "the tenant's own Quota");
    assert_eq!(Problem::from(error).status, Some(400));
}
