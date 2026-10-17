//! REST round trip of the three QE contracts: the generated server routes on an
//! ephemeral axum server, behind the production bearer-auth middleware, driven
//! by the generated clients.

#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use quota_enforcement_sdk::rest::{
    register_quota_enforcement_api_rest_routes, register_quota_manager_api_rest_routes,
    register_quota_operator_api_rest_routes,
};
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, BatchDebitRequest, BatchDecision,
    BulkCreateQuotasRequest, BulkCreated, BulkDeactivateQuotasRequest, BulkDeactivated,
    BulkUpdateQuotasRequest, BulkUpdated, CommitLeaseRequest, CreditRequest, DeactivateOutcome,
    DebitRequest, Decision, DecisionPreview, DeletePolicyRequest, ListPolicyVersionsRequest,
    PageRequest, PageResult, PolicyId, PolicySpec, PolicyVersion, PolicyVersionMeta,
    PreviewRequest, QuotaEnforcementApi, QuotaEnforcementApiRestClient, QuotaFilter, QuotaId,
    QuotaManagerApi, QuotaManagerApiRestClient, QuotaOperatorApi, QuotaOperatorApiRestClient,
    QuotaSnapshot, QuotaSpec, QuotaView, ReadPolicyRequest, ReadQuotasRequest, ReleaseLeaseRequest,
    RollbackPolicyRequest, RollbackRequest, SnapshotRequest, UpdatePolicyRequest,
    UpdateQuotaRequest,
};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_gts::gts_id;
use toolkit_http_middleware::security_context_middleware;
use toolkit_security::{AuthNError, BearerAuthenticator, SecurityContext};
use uuid::Uuid;

const GOOD_TOKEN: &str = "good-token";
const SUBJECT: Uuid = Uuid::from_u128(0x5eed);
const TENANT: Uuid = Uuid::from_u128(0x7e17);

#[resource_error(gts_id!("cf.core.qe.quota.v1~"))]
struct QuotaResource;

/// The production middleware's token check: one token is valid and maps to a
/// known caller, every other token is rejected.
struct StubAuthenticator;

impl BearerAuthenticator for StubAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        if token == GOOD_TOKEN {
            Ok(SecurityContext::builder()
                .subject_id(SUBJECT)
                .subject_tenant_id(TENANT)
                .build()
                .expect("caller context"))
        } else {
            Err(AuthNError::InvalidToken)
        }
    }
}

fn unused() -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail("not used by this test")
        .create()
}

/// Records every call and the caller the server rebuilt for it.
#[derive(Default)]
struct Seen {
    calls: AtomicUsize,
    caller: Mutex<Option<(Uuid, Uuid)>>,
}

impl Seen {
    fn record(&self, ctx: &SecurityContext) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.caller.lock().unwrap() = Some((ctx.subject_id(), ctx.subject_tenant_id()));
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn caller(&self) -> Option<(Uuid, Uuid)> {
        *self.caller.lock().unwrap()
    }
}

struct FakeManager(Arc<Seen>);

#[async_trait]
impl QuotaManagerApi for FakeManager {
    async fn create_quota(
        &self,
        ctx: &SecurityContext,
        _spec: QuotaSpec,
    ) -> Result<QuotaId, CanonicalError> {
        self.0.record(ctx);
        Ok(QuotaId::new(Uuid::from_u128(7)))
    }

    async fn update_quota(
        &self,
        _ctx: &SecurityContext,
        _request: UpdateQuotaRequest,
    ) -> Result<(), CanonicalError> {
        Err(unused())
    }

    async fn deactivate_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
    ) -> Result<DeactivateOutcome, CanonicalError> {
        self.0.record(ctx);
        Err(
            QuotaResource::not_found(format!("UNKNOWN_QUOTA: quota {quota_id} not found"))
                .with_resource(quota_id.to_string())
                .create(),
        )
    }

    async fn read_quotas(
        &self,
        ctx: &SecurityContext,
        request: ReadQuotasRequest,
    ) -> Result<PageResult<QuotaView>, CanonicalError> {
        self.0.record(ctx);
        Ok(PageResult {
            items: Vec::new(),
            next_cursor: Some(format!("after-{}", request.page.limit)),
        })
    }

    async fn credit(
        &self,
        _ctx: &SecurityContext,
        _request: CreditRequest,
    ) -> Result<Decision, CanonicalError> {
        Err(unused())
    }

    async fn bulk_create_quotas(
        &self,
        _ctx: &SecurityContext,
        _request: BulkCreateQuotasRequest,
    ) -> Result<BulkCreated, CanonicalError> {
        Err(unused())
    }

    async fn bulk_update_quotas(
        &self,
        _ctx: &SecurityContext,
        _request: BulkUpdateQuotasRequest,
    ) -> Result<BulkUpdated, CanonicalError> {
        Err(unused())
    }

    async fn bulk_deactivate_quotas(
        &self,
        _ctx: &SecurityContext,
        _request: BulkDeactivateQuotasRequest,
    ) -> Result<BulkDeactivated, CanonicalError> {
        Err(unused())
    }
}

struct FakeEnforcement(Arc<Seen>);

#[async_trait]
impl QuotaEnforcementApi for FakeEnforcement {
    async fn debit(
        &self,
        ctx: &SecurityContext,
        request: DebitRequest,
    ) -> Result<Decision, CanonicalError> {
        self.0.record(ctx);
        let mut decision = Decision::allowed_with_plan(std::collections::BTreeMap::new());
        decision
            .diagnostics
            .insert("key".to_owned(), serde_json::json!(request.idempotency_key));
        Ok(decision)
    }

    async fn rollback(
        &self,
        _ctx: &SecurityContext,
        _request: RollbackRequest,
    ) -> Result<Decision, CanonicalError> {
        Err(unused())
    }

    async fn evaluate_preview(
        &self,
        _ctx: &SecurityContext,
        _request: PreviewRequest,
    ) -> Result<DecisionPreview, CanonicalError> {
        Err(unused())
    }

    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, CanonicalError> {
        Err(unused())
    }

    async fn commit_lease(
        &self,
        _ctx: &SecurityContext,
        _request: CommitLeaseRequest,
    ) -> Result<Decision, CanonicalError> {
        Err(unused())
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _request: ReleaseLeaseRequest,
    ) -> Result<Decision, CanonicalError> {
        Err(unused())
    }

    async fn batch_debit(
        &self,
        _ctx: &SecurityContext,
        _request: BatchDebitRequest,
    ) -> Result<BatchDecision, CanonicalError> {
        Err(unused())
    }

    async fn snapshot(
        &self,
        _ctx: &SecurityContext,
        _request: SnapshotRequest,
    ) -> Result<PageResult<QuotaSnapshot>, CanonicalError> {
        Err(unused())
    }
}

struct FakeOperator(Arc<Seen>);

#[async_trait]
impl QuotaOperatorApi for FakeOperator {
    async fn create_policy(
        &self,
        _ctx: &SecurityContext,
        _spec: PolicySpec,
    ) -> Result<PolicyVersion, CanonicalError> {
        Err(unused())
    }

    async fn update_policy(
        &self,
        _ctx: &SecurityContext,
        _request: UpdatePolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError> {
        Err(unused())
    }

    async fn rollback_policy(
        &self,
        _ctx: &SecurityContext,
        _request: RollbackPolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError> {
        Err(unused())
    }

    async fn delete_policy(
        &self,
        _ctx: &SecurityContext,
        _request: DeletePolicyRequest,
    ) -> Result<(), CanonicalError> {
        Err(unused())
    }

    async fn read_policy(
        &self,
        _ctx: &SecurityContext,
        _request: ReadPolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError> {
        Err(unused())
    }

    async fn list_policy_versions(
        &self,
        ctx: &SecurityContext,
        request: ListPolicyVersionsRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, CanonicalError> {
        self.0.record(ctx);
        Ok(PageResult {
            items: Vec::new(),
            next_cursor: Some(request.id.to_string()),
        })
    }
}

struct Clients {
    manager: QuotaManagerApiRestClient,
    enforcement: QuotaEnforcementApiRestClient,
    operator: QuotaOperatorApiRestClient,
}

/// All three contracts' routes in one router and one `OpenAPI` registry, behind
/// the production `security_context_middleware`.
async fn serve(seen: &Arc<Seen>) -> Clients {
    let openapi = OpenApiRegistryImpl::new();
    let router = register_quota_manager_api_rest_routes(
        Router::new(),
        &openapi,
        Arc::new(FakeManager(Arc::clone(seen))) as Arc<dyn QuotaManagerApi>,
    );
    let router = register_quota_enforcement_api_rest_routes(
        router,
        &openapi,
        Arc::new(FakeEnforcement(Arc::clone(seen))) as Arc<dyn QuotaEnforcementApi>,
    );
    let router = register_quota_operator_api_rest_routes(
        router,
        &openapi,
        Arc::new(FakeOperator(Arc::clone(seen))) as Arc<dyn QuotaOperatorApi>,
    );
    let auth = axum::middleware::from_fn_with_state(
        Arc::new(StubAuthenticator),
        security_context_middleware::<StubAuthenticator>,
    );
    let app: Router = router.route_layer(auth);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = || ClientConfig::new(format!("http://{addr}"));
    Clients {
        manager: QuotaManagerApiRestClient::new(config()).unwrap(),
        enforcement: QuotaEnforcementApiRestClient::new(config()).unwrap(),
        operator: QuotaOperatorApiRestClient::new(config()).unwrap(),
    }
}

fn caller(token: Option<&str>) -> SecurityContext {
    let builder = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2));
    match token {
        Some(token) => builder.bearer_token(token.to_owned()),
        None => builder,
    }
    .build()
    .expect("outbound context")
}

fn spec() -> QuotaSpec {
    serde_json::from_value(serde_json::json!({
        "tenant_id": TENANT,
        "subject": {
            "projection_type": gts_id!("cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~"),
            "subject_id": "u1"
        },
        "metric": gts_id!("cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1"),
        "quota_type": gts_id!("cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1"),
        "period": gts_id!("cf.core.qe.period_type.v1~cf.core.qe.month.v1"),
        "enforcement_mode": gts_id!("cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1"),
        "cap": 100,
        "notification_thresholds": [],
        "metadata": {},
        "source": gts_id!("cf.core.qe.source_type.v1~cf.core.qe.operator.v1")
    }))
    .expect("quota spec")
}

#[tokio::test]
async fn each_contract_round_trips_as_the_caller_the_token_names() {
    let seen = Arc::new(Seen::default());
    let clients = serve(&seen).await;
    let ctx = caller(Some(GOOD_TOKEN));

    let id = clients
        .manager
        .create_quota(&ctx, spec())
        .await
        .expect("create");
    assert_eq!(id, QuotaId::new(Uuid::from_u128(7)));
    assert_eq!(
        seen.caller(),
        Some((SUBJECT, TENANT)),
        "the server acts as the caller its token names, not as the client's local context"
    );

    let page = clients
        .manager
        .read_quotas(
            &ctx,
            ReadQuotasRequest {
                filter: QuotaFilter::default(),
                page: PageRequest {
                    limit: 3,
                    cursor: None,
                },
            },
        )
        .await
        .expect("read");
    assert_eq!(page.next_cursor.as_deref(), Some("after-3"));

    let decision = clients
        .enforcement
        .debit(
            &ctx,
            serde_json::from_value(serde_json::json!({
                "attribution": {
                    "tenant_id": TENANT,
                    "metric": gts_id!("cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1"),
                    "subjects": [],
                    "metadata": {}
                },
                "amount": 1,
                "idempotency_key": "d1"
            }))
            .expect("debit request"),
        )
        .await
        .expect("debit");
    assert_eq!(
        decision.diagnostics.get("key"),
        Some(&serde_json::json!("d1"))
    );

    let history = clients
        .operator
        .list_policy_versions(
            &ctx,
            ListPolicyVersionsRequest {
                id: PolicyId::new("p1"),
                page: PageRequest {
                    limit: 1,
                    cursor: None,
                },
            },
        )
        .await
        .expect("history");
    assert_eq!(history.next_cursor.as_deref(), Some("p1"));
    assert_eq!(seen.calls(), 4);
}

#[tokio::test]
async fn a_canonical_error_keeps_its_category_and_token_over_rest() {
    let seen = Arc::new(Seen::default());
    let clients = serve(&seen).await;

    let err = clients
        .manager
        .deactivate_quota(&caller(Some(GOOD_TOKEN)), QuotaId::new(Uuid::from_u128(9)))
        .await
        .expect_err("an unknown quota");
    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");
    assert!(err.detail().starts_with("UNKNOWN_QUOTA: "), "{err:?}");
}

#[tokio::test]
async fn a_missing_or_invalid_token_is_refused_before_the_service_runs() {
    let seen = Arc::new(Seen::default());
    let clients = serve(&seen).await;

    for ctx in [caller(None), caller(Some("forged"))] {
        let err = clients
            .manager
            .create_quota(&ctx, spec())
            .await
            .expect_err("no valid credential");
        assert!(
            matches!(err, CanonicalError::Unauthenticated { .. }),
            "{err:?}"
        );
    }
    assert_eq!(seen.calls(), 0, "the service never ran");
}
