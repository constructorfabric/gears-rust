#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use quota_resolver_sdk::models::{
    DenialReason, LeaseOutcome, MetricItem, MetricObject, PlatformAcquireLeaseRequest,
    PlatformCommitLeaseRequest, PlatformDebitRequest, PlatformQuotaStateRequest,
    PlatformReleaseLeaseRequest, PlatformReturnRequest, PrincipalSubject, QuotaDecision,
    QuotaState, QuotaStateMetric, QuotaStateRequest, QuotaSubject, SubjectInput,
};
use quota_resolver_sdk::rest::{
    PlatformQuotaResolverApiRestClient, QUOTA_STATE_RATE_LIMIT_ZONE, QuotaResolverApiRestClient,
    register_platform_quota_resolver_api_rest_routes, register_quota_state_route,
};
use quota_resolver_sdk::{PlatformQuotaResolverApi, QuotaResolverApi};
use serde_json::json;
use toolkit::Page;
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_odata::PageInfo;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

const TENANT_SUBJECT: &str = "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~";
const METRIC: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~";

fn tenant() -> Uuid {
    Uuid::from_u128(42)
}

fn state(tenant: Uuid) -> QuotaState {
    QuotaState {
        quota_id: "q1".to_owned(),
        subject: QuotaSubject {
            subject_type: TENANT_SUBJECT.to_owned(),
            id: tenant.to_string(),
        },
        metric_type: METRIC.to_owned(),
        cap: Some(100),
        consumed: 40,
        remaining: Some(60),
        period: None,
        metadata: None,
    }
}

fn page(items: Vec<QuotaState>) -> Page<QuotaState> {
    Page {
        items,
        page_info: PageInfo {
            next_cursor: Some("next".to_owned()),
            prev_cursor: None,
            limit: 10,
        },
    }
}

fn metric_item(amount: i64) -> MetricItem {
    MetricItem {
        metric: serde_json::from_value::<MetricObject>(
            json!({ "type": METRIC, "metadata": {}, "future": 1 }),
        )
        .unwrap(),
        amount,
    }
}

fn principal() -> SubjectInput {
    SubjectInput::Principal(PrincipalSubject {
        subject_type: "gts.cf.core.am.user.v1~".to_owned(),
        id: Uuid::from_u128(7),
    })
}

struct Platform;

#[async_trait]
impl PlatformQuotaResolverApi for Platform {
    async fn debit(
        &self,
        _ctx: PlatformSecurityContext,
        request: PlatformDebitRequest,
    ) -> Result<QuotaDecision, CanonicalError> {
        let decision = if request.items[0].amount <= 10 {
            QuotaDecision::allowed()
        } else {
            QuotaDecision::denied(DenialReason::QuotaExceeded)
        };
        Ok(decision
            .with_diagnostic("tenant", request.tenant_id.to_string())
            .with_diagnostic("future", request.items[0].metric.extra["future"].clone()))
    }

    async fn acquire_lease(
        &self,
        _ctx: PlatformSecurityContext,
        request: PlatformAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError> {
        Ok(LeaseOutcome::Acquired {
            token: format!("lease-{}", request.ttl_secs),
            expires_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        })
    }

    async fn commit_lease(
        &self,
        _ctx: PlatformSecurityContext,
        _request: PlatformCommitLeaseRequest,
    ) -> Result<(), CanonicalError> {
        Ok(())
    }

    async fn release_lease(
        &self,
        _ctx: PlatformSecurityContext,
        _request: PlatformReleaseLeaseRequest,
    ) -> Result<(), CanonicalError> {
        Ok(())
    }

    async fn return_amount(
        &self,
        _ctx: PlatformSecurityContext,
        _request: PlatformReturnRequest,
    ) -> Result<(), CanonicalError> {
        Err(CanonicalError::service_unavailable()
            .with_detail("backend unavailable")
            .create())
    }

    async fn quota_state(
        &self,
        _ctx: PlatformSecurityContext,
        request: PlatformQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        Ok(page(vec![state(request.tenant_id)]))
    }
}

struct Tenant;

#[async_trait]
impl QuotaResolverApi for Tenant {
    async fn quota_state(
        &self,
        ctx: SecurityContext,
        request: QuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        assert_eq!(request.metrics, [QuotaStateMetric::of(METRIC)]);
        Ok(page(vec![state(ctx.subject_tenant_id())]))
    }
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(7))
        .subject_tenant_id(tenant())
        .bearer_token("token".to_owned())
        .build()
        .unwrap()
}

async fn serve() -> (
    PlatformQuotaResolverApiRestClient,
    QuotaResolverApiRestClient,
    serde_json::Value,
) {
    let platform_ctx = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut()
                .insert(PlatformSecurityContext::outbound_marker());
            req.extensions_mut().insert(caller());
            next.run(req).await
        },
    );
    let openapi = OpenApiRegistryImpl::new();
    let router = register_platform_quota_resolver_api_rest_routes(
        Router::new(),
        &openapi,
        Arc::new(Platform),
    );
    let app = register_quota_state_route(router, &openapi, Arc::new(Tenant)).layer(platform_ctx);
    let spec =
        serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let config = || ClientConfig::new(format!("http://{addr}"));
    (
        PlatformQuotaResolverApiRestClient::new(config()).unwrap(),
        QuotaResolverApiRestClient::new(config()).unwrap(),
        spec,
    )
}

fn marker() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

#[tokio::test]
async fn a_debit_crosses_the_platform_plane_with_its_metric_object_intact() {
    let (platform, _, _) = serve().await;
    let request = |amount| PlatformDebitRequest {
        tenant_id: tenant(),
        subjects: vec![principal()],
        items: vec![metric_item(amount)],
        idempotency_key: "k".to_owned(),
    };

    let allowed = platform.debit(marker(), request(5)).await.unwrap();
    assert!(allowed.allowed);
    assert_eq!(allowed.diagnostics["tenant"], json!(tenant().to_string()));
    assert_eq!(allowed.diagnostics["future"], json!(1));

    let denied = platform.debit(marker(), request(50)).await.unwrap();
    assert!(!denied.allowed, "a denial is a decision, not an error");
    assert_eq!(denied.reason, Some(DenialReason::QuotaExceeded));
}

#[tokio::test]
async fn a_lease_is_acquired_committed_and_released_over_rest() {
    let (platform, _, _) = serve().await;

    let outcome = platform
        .acquire_lease(
            marker(),
            PlatformAcquireLeaseRequest {
                tenant_id: tenant(),
                subjects: vec![principal()],
                items: vec![metric_item(5)],
                ttl_secs: 30,
                idempotency_key: "k".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, LeaseOutcome::Acquired { ref token, .. } if token == "lease-30"));

    platform
        .commit_lease(
            marker(),
            PlatformCommitLeaseRequest {
                tenant_id: tenant(),
                token: "lease-30".to_owned(),
                amounts: None,
                idempotency_key: "c".to_owned(),
            },
        )
        .await
        .unwrap();
    platform
        .release_lease(
            marker(),
            PlatformReleaseLeaseRequest {
                tenant_id: tenant(),
                token: "lease-30".to_owned(),
                idempotency_key: "r".to_owned(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failing_write_keeps_its_canonical_category_over_rest() {
    let (platform, _, _) = serve().await;

    let unavailable = platform
        .return_amount(
            marker(),
            serde_json::from_value::<PlatformReturnRequest>(json!({
                "tenant_id": tenant(),
                "charge": { "operation": "debit", "idempotency_key": "k" },
                "amounts": [{ "metric_type": METRIC, "amount": 1 }],
                "idempotency_key": "r",
            }))
            .unwrap(),
        )
        .await
        .expect_err("a failing backend is an error");
    assert!(
        matches!(unavailable, CanonicalError::ServiceUnavailable { .. }),
        "got: {unavailable:?}"
    );
}

#[tokio::test]
async fn quota_state_pages_cross_both_planes() {
    let (platform, tenant_plane, _) = serve().await;

    let page = platform
        .quota_state(
            marker(),
            PlatformQuotaStateRequest {
                tenant_id: tenant(),
                subject: principal(),
                metrics: vec![QuotaStateMetric::of(METRIC)],
                limit: Some(10),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![state(tenant())]);
    assert_eq!(page.page_info.next_cursor.as_deref(), Some("next"));

    let page = tenant_plane
        .quota_state(
            caller(),
            QuotaStateRequest {
                metrics: vec![QuotaStateMetric::of(METRIC)],
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![state(tenant())]);
}

#[tokio::test]
async fn only_the_tenant_plane_quota_state_is_exposed() {
    let (_, _, spec) = serve().await;

    let mut exposed = Vec::new();
    let mut all = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            all.push(format!("{method} {path}"));
            if operation["x-toolkit-visibility"] == json!("exposed") {
                exposed.push(format!("{method} {path}"));
            }
        }
    }
    all.sort();
    assert_eq!(
        all,
        [
            "post /quota-resolver/v1/platform/acquire-lease",
            "post /quota-resolver/v1/platform/commit-lease",
            "post /quota-resolver/v1/platform/debit",
            "post /quota-resolver/v1/platform/quota-state",
            "post /quota-resolver/v1/platform/release-lease",
            "post /quota-resolver/v1/platform/return-amount",
            "post /quota-resolver/v1/quota-state",
        ]
    );
    assert_eq!(exposed, ["post /quota-resolver/v1/quota-state"]);
    assert_eq!(
        spec["paths"]["/quota-resolver/v1/quota-state"]["post"]["x-throttling-rate-limit-zone"],
        json!(QUOTA_STATE_RATE_LIMIT_ZONE)
    );

    for (path, item) in spec["paths"].as_object().unwrap() {
        if path.starts_with("/quota-resolver/v1/platform/") {
            assert_eq!(
                item["post"]["security"],
                json!([{ "internalToken": [] }]),
                "{path} requires the internal token"
            );
        }
    }
}
