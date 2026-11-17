#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tenant offboarding through the gear (`cpt-cf-graph-storage-fr-tenant-offboarding`):
//! who may start it, what the platform must have said first, the fence it
//! puts on every other operation, and the worker that finishes it. The
//! store's half -- the rows that go, the write that races the fence -- is in
//! the conformance suite.
//!
//! The caller is a service identity whose own tenant is the platform root,
//! as Construct's is. The stub PDP grants `offboard` on the tenants it is
//! given, asked about each as a named tenant, and records what it was asked.

#[allow(dead_code)]
mod conformance;
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::api::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, TenantMode,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use graph_storage::api::rest::routes::register_routes;
use graph_storage::domain::error::DomainError;
use graph_storage::domain::local_client::GraphStorageLocalClient;
use graph_storage::domain::offboarding::{
    TenantDirectory, TenantState, WorkerSettings, WorkerState, run_pending, run_worker,
};
use graph_storage::infra::fake_store::FakeGraphStore;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::OffboardRequest;
use graph_storage_sdk::plugin_api::GraphStoreV1;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use tower::ServiceExt;
use uuid::Uuid;

use support::Harness;

/// Grants a named tenant when it is one of `granted`, and records the
/// resource and action of every such question.
struct Offboarders {
    granted: Vec<Uuid>,
    asked: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AuthZResolverApi for Offboarders {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let Some(target) = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|context| context.root_id)
        else {
            return support::AllowInOwnTenant.evaluate(ctx, request).await;
        };
        assert!(matches!(
            request.context.tenant_context.as_ref().map(|c| &c.mode),
            Some(TenantMode::RootOnly)
        ));
        self.asked.lock().unwrap().push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
        ));
        if !self.granted.contains(&target) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            });
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![target],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// The platform, as far as offboarding asks it: these tenants are deleted,
/// every other one is live. Counts the questions.
struct Platform {
    deleted: Vec<Uuid>,
    unknown: Vec<Uuid>,
    asked: AtomicUsize,
}

#[async_trait]
impl TenantDirectory for Platform {
    async fn state(
        &self,
        _ctx: &SecurityContext,
        tenant: Uuid,
        _within: Duration,
    ) -> Result<TenantState, DomainError> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        Ok(if self.deleted.contains(&tenant) {
            TenantState::Deleted
        } else if self.unknown.contains(&tenant) {
            TenantState::Unknown
        } else {
            TenantState::Live
        })
    }
}

struct Stand {
    store: Arc<FakeGraphStore>,
    pdp: Arc<Offboarders>,
    platform: Arc<Platform>,
    harness: Harness,
    client: Arc<dyn GraphStorageClientV1>,
    service: SecurityContext,
}

fn stand(granted: Vec<Uuid>, deleted: Vec<Uuid>) -> Stand {
    let store = Arc::new(FakeGraphStore::new());
    let pdp = Arc::new(Offboarders {
        granted,
        asked: Mutex::new(Vec::new()),
    });
    let platform = Arc::new(Platform {
        deleted,
        unknown: Vec::new(),
        asked: AtomicUsize::new(0),
    });
    let harness = Harness::offboarding(
        Arc::clone(&store),
        Arc::clone(&pdp) as Arc<dyn AuthZResolverApi>,
        Arc::clone(&platform) as Arc<dyn TenantDirectory>,
    );
    let client: Arc<dyn GraphStorageClientV1> =
        Arc::new(GraphStorageLocalClient::new(Arc::clone(&harness.services)));
    let service = SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(Uuid::now_v7())
        .build()
        .expect("a valid security context");
    Stand {
        store,
        pdp,
        platform,
        harness,
        client,
        service,
    }
}

impl Stand {
    async fn seed(&self, tenant: Uuid) {
        let scoped = self.client.in_tenant(tenant);
        scoped
            .register_types(&self.service, conformance::ontology_batch())
            .await
            .expect("the ontology registers");
        scoped
            .ingest(
                &self.service,
                conformance::batch(
                    vec![conformance::node("a", "a"), conformance::node("b", "b")],
                    vec![conformance::edge("a", "b")],
                ),
            )
            .await
            .expect("the graph lands");
    }
}

/// The stable reason a precondition failure carries, read from its
/// violation rather than from a rendering.
fn reason_of(error: &CanonicalError) -> String {
    match error {
        CanonicalError::FailedPrecondition { ctx, .. } => ctx
            .violations
            .first()
            .map(|violation| violation.type_.clone())
            .unwrap_or_default(),
        other => panic!("expected a failed precondition, got {other:?}"),
    }
}

fn settings(batch: u64) -> WorkerSettings {
    WorkerSettings::new(
        std::num::NonZeroU64::new(batch).expect("a batch"),
        Duration::from_secs(10),
        Duration::from_secs(30),
    )
    .expect("valid settings")
}

/// One worker pass with a fresh worker's memory.
async fn one_pass(store: &dyn GraphStoreV1, batch: u64) -> usize {
    run_pending(
        store,
        settings(batch),
        &mut WorkerState::default(),
        &CancellationToken::new(),
    )
    .await
}

/// A granted caller offboards a deleted tenant: the record comes back
/// fenced, the PDP was asked for `offboard` on the tenant resource, every
/// other operation for the tenant is refused as fenced from then on, and
/// the worker finishes the deletion.
#[tokio::test]
async fn a_deleted_tenant_is_fenced_and_the_worker_removes_its_graph() {
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], vec![tenant]);
    stand.seed(tenant).await;

    let record = stand
        .client
        .offboard_tenant(
            &stand.service,
            tenant,
            OffboardRequest {
                deletion_generation: graph_storage_sdk::models::DeletionGeneration::new(4),
            },
        )
        .await
        .expect("a granted caller offboards a deleted tenant");
    assert_eq!(record.tenant_id, tenant);
    assert_eq!(
        record
            .deletion_generation
            .map(graph_storage_sdk::models::DeletionGeneration::get),
        Some(4)
    );
    assert!(!record.is_complete(), "the worker has not run yet");
    assert!(
        stand.pdp.asked.lock().unwrap().contains(&(
            graph_storage_sdk::gts::TENANT_RESOURCE.to_owned(),
            "offboard".to_owned()
        )),
        "the PDP is asked for offboard on the tenant: {:?}",
        stand.pdp.asked.lock().unwrap()
    );

    let scoped = stand.client.in_tenant(tenant);
    let read = scoped
        .revision(&stand.service)
        .await
        .expect_err("a read of a fenced tenant is refused");
    assert_eq!(reason_of(&read), "TENANT_FENCED");
    let write = scoped
        .ingest(
            &stand.service,
            conformance::batch(vec![conformance::node("late", "late")], Vec::new()),
        )
        .await
        .expect_err("a write for a fenced tenant is refused");
    assert_eq!(reason_of(&write), "TENANT_FENCED");

    let again = stand
        .client
        .offboard_tenant(&stand.service, tenant, OffboardRequest::default())
        .await
        .expect("a repeated call is safe");
    assert_eq!(again, record, "and answers the same record");

    let finished = one_pass(stand.store.as_ref(), 1).await;
    assert_eq!(finished, 1);
    let done = stand
        .client
        .tenant_offboarding(&stand.service, tenant)
        .await
        .expect("the record reads");
    assert!(done.is_complete(), "{done:?}");
    assert!(
        done.removed.nodes >= 2 && done.removed.edges >= 1,
        "{done:?}"
    );
    assert!(
        stand
            .store
            .pending_offboardings()
            .await
            .expect("pending lists")
            .is_empty()
    );
}

/// The permission alone starts nothing: a tenant the platform has not
/// marked deleted is refused, and nothing is fenced.
#[tokio::test]
async fn a_tenant_the_platform_has_not_deleted_is_refused_and_stays_usable() {
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], Vec::new());
    stand.seed(tenant).await;

    let refused = stand
        .client
        .offboard_tenant(&stand.service, tenant, OffboardRequest::default())
        .await
        .expect_err("not deleted, not offboarded");
    assert_eq!(reason_of(&refused), "TENANT_NOT_DELETED");
    assert_eq!(
        conformance::record_of(stand.store.as_ref(), tenant).await,
        None,
        "nothing was fenced"
    );
    stand
        .client
        .in_tenant(tenant)
        .revision(&stand.service)
        .await
        .expect("the tenant is served as before");
}

/// A caller the policy does not grant is answered as if the tenant had no
/// offboarding at all -- before the platform is asked, and without a fence.
#[tokio::test]
async fn a_caller_without_the_grant_is_not_found_and_fences_nothing() {
    let tenant = Uuid::now_v7();
    let stand = stand(Vec::new(), vec![tenant]);

    let refused = stand
        .client
        .offboard_tenant(&stand.service, tenant, OffboardRequest::default())
        .await
        .expect_err("refused");
    assert_eq!(refused.status_code(), 404, "{refused:?}");
    let status = stand
        .client
        .tenant_offboarding(&stand.service, tenant)
        .await
        .expect_err("refused");
    assert_eq!(status.status_code(), 404, "{status:?}");
    assert_eq!(
        conformance::record_of(stand.store.as_ref(), tenant).await,
        None
    );
    assert_eq!(
        stand.platform.asked.load(Ordering::Relaxed),
        0,
        "the platform is not asked for a caller the policy refused"
    );
}

/// Without a tenant resolver the gear cannot confirm a deletion, so it does
/// not start one: a dependency outage, not a grant.
#[tokio::test]
async fn without_a_tenant_resolver_no_tenant_is_offboarded() {
    let tenant = Uuid::now_v7();
    let pdp: Arc<dyn AuthZResolverApi> = Arc::new(Offboarders {
        granted: vec![tenant],
        asked: Mutex::new(Vec::new()),
    });
    // The directory the gear wires, over a hub with no resolver in it.
    let store = Arc::new(FakeGraphStore::new());
    let harness = Harness::offboarding(
        Arc::clone(&store),
        pdp,
        Arc::new(graph_storage::infra::tenants::HubTenantDirectory::new(
            Arc::new(toolkit::client_hub::ClientHub::new()),
        )),
    );
    let client = GraphStorageLocalClient::new(Arc::clone(&harness.services));
    let service = SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(Uuid::now_v7())
        .build()
        .expect("a valid security context");
    let refused = client
        .offboard_tenant(&service, tenant, OffboardRequest::default())
        .await
        .expect_err("no resolver, no offboarding");
    assert_eq!(refused.status_code(), 503, "{refused:?}");
    assert_eq!(conformance::record_of(store.as_ref(), tenant).await, None);
}

/// The status of a tenant never offboarded is not found, as a refusal is.
#[tokio::test]
async fn a_tenant_never_offboarded_has_no_record() {
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], vec![tenant]);
    let missing = stand
        .client
        .tenant_offboarding(&stand.service, tenant)
        .await
        .expect_err("no record");
    assert_eq!(missing.status_code(), 404, "{missing:?}");
}

/// The policy decides before the fence: a caller without a grant asking
/// about a fenced tenant hears not found, never that it is being offboarded.
#[tokio::test]
async fn a_caller_without_the_grant_does_not_learn_a_tenant_is_fenced() {
    let tenant = Uuid::now_v7();
    let stand = stand(Vec::new(), vec![tenant]);
    conformance::fence(stand.store.as_ref(), tenant, None)
        .await
        .expect("the fence");
    let refused = stand
        .client
        .in_tenant(tenant)
        .revision(&stand.service)
        .await
        .expect_err("refused");
    assert_eq!(refused.status_code(), 404, "{refused:?}");
}

/// A tenant the platform does not know is its own refusal, not "wait for the
/// platform to delete it", and nothing is fenced.
#[tokio::test]
async fn a_tenant_the_platform_does_not_know_is_refused_as_unknown() {
    let tenant = Uuid::now_v7();
    let mut stand = stand(vec![tenant], Vec::new());
    let platform = Arc::new(Platform {
        deleted: Vec::new(),
        unknown: vec![tenant],
        asked: AtomicUsize::new(0),
    });
    stand.harness = Harness::offboarding(
        Arc::clone(&stand.store),
        Arc::clone(&stand.pdp) as Arc<dyn AuthZResolverApi>,
        platform,
    );
    let client = GraphStorageLocalClient::new(Arc::clone(&stand.harness.services));
    let refused = client
        .offboard_tenant(&stand.service, tenant, OffboardRequest::default())
        .await
        .expect_err("an unknown tenant is not offboarded");
    assert_eq!(reason_of(&refused), "TENANT_UNKNOWN");
    assert_eq!(
        conformance::record_of(stand.store.as_ref(), tenant).await,
        None
    );
}

/// A tenant already fenced answers its record without asking the platform
/// again, which may since have removed the tenant for good.
#[tokio::test]
async fn a_fenced_tenant_answers_its_record_without_asking_the_platform() {
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], Vec::new());
    let fenced = conformance::fence(stand.store.as_ref(), tenant, Some(2))
        .await
        .expect("the fence");
    let answered = stand
        .client
        .offboard_tenant(&stand.service, tenant, OffboardRequest::default())
        .await
        .expect("the record, though the platform calls the tenant live");
    assert_eq!(answered, fenced);
    assert_eq!(stand.platform.asked.load(Ordering::Relaxed), 0);
}

/// A deletion generation that is not positive cannot be built, and the REST
/// surface refuses one as a bad request before anything is fenced: the
/// first value stands for good, so a bad one would too.
#[tokio::test]
async fn a_deletion_generation_must_be_positive() {
    assert!(graph_storage_sdk::models::DeletionGeneration::new(0).is_none());
    assert!(graph_storage_sdk::models::DeletionGeneration::new(-4).is_none());
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], vec![tenant]);
    let router = register_routes(
        Router::new(),
        &NoopRegistry,
        Arc::clone(&stand.harness.services),
    )
    .layer(axum::Extension(stand.service.clone()));
    for generation in [0, -4] {
        let (status, body) = send(
            &router,
            Request::builder()
                .method("POST")
                .uri(format!("/graph-storage/v1/tenants/{tenant}/offboarding"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"deletion_generation": generation}).to_string(),
                ))
                .expect("a valid request"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(
        conformance::record_of(stand.store.as_ref(), tenant).await,
        None
    );
}

/// A client scoped to one tenant refuses to offboard another: an
/// irreversible deletion does not pick between two tenants.
#[tokio::test]
async fn a_scoped_client_refuses_another_tenant() {
    let (scoped, other) = (Uuid::now_v7(), Uuid::now_v7());
    let stand = stand(vec![scoped, other], vec![scoped, other]);
    let refused = stand
        .client
        .in_tenant(scoped)
        .offboard_tenant(&stand.service, other, OffboardRequest::default())
        .await
        .expect_err("refused");
    assert_eq!(refused.status_code(), 400, "{refused:?}");
    assert_eq!(
        conformance::record_of(stand.store.as_ref(), other).await,
        None
    );
    stand
        .client
        .in_tenant(scoped)
        .offboard_tenant(&stand.service, scoped, OffboardRequest::default())
        .await
        .expect("its own tenant is offboarded");
}

/// A store that does not declare offboarding fences nothing: it could not
/// keep the fence or delete the rows.
#[tokio::test]
async fn a_store_without_offboarding_fences_nothing() {
    let tenant = Uuid::now_v7();
    let store = Arc::new(FakeGraphStore::without_offboarding());
    let harness = Harness::offboarding(
        Arc::clone(&store),
        Arc::new(Offboarders {
            granted: vec![tenant],
            asked: Mutex::new(Vec::new()),
        }),
        Arc::new(Platform {
            deleted: vec![tenant],
            unknown: Vec::new(),
            asked: AtomicUsize::new(0),
        }),
    );
    let client = GraphStorageLocalClient::new(Arc::clone(&harness.services));
    let service = SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(Uuid::now_v7())
        .build()
        .expect("a valid security context");
    let refused = client
        .offboard_tenant(&service, tenant, OffboardRequest::default())
        .await
        .expect_err("refused");
    assert_eq!(refused.status_code(), 501, "{refused:?}");
    assert_eq!(conformance::record_of(store.as_ref(), tenant).await, None);
}

/// A tenant whose deletion fails is noted on its record and skipped, the
/// next one still finishes, and the failed one waits out its backoff
/// rather than being retried on the very next pass.
#[tokio::test]
async fn a_failing_deletion_is_noted_and_backs_off_while_the_next_one_finishes() {
    let (stuck, fine) = (Uuid::now_v7(), Uuid::now_v7());
    let store = FakeGraphStore::new();
    for tenant in [stuck, fine] {
        conformance::fence(&store, tenant, None)
            .await
            .expect("the fence");
    }
    store.fail_offboarding_of(stuck);
    let cancel = CancellationToken::new();
    let mut state = WorkerState::default();

    assert_eq!(
        run_pending(&store, settings(1_000), &mut state, &cancel).await,
        1
    );
    let noted = conformance::record_of(&store, stuck)
        .await
        .expect("the record");
    assert_eq!(noted.failures, 1);
    assert_eq!(
        noted
            .last_failure
            .as_ref()
            .map(|failure| failure.class.as_str()),
        Some("unavailable")
    );
    assert!(!noted.is_complete());
    assert!(
        conformance::record_of(&store, fine)
            .await
            .expect("the record")
            .is_complete()
    );

    assert_eq!(
        run_pending(&store, settings(1_000), &mut state, &cancel).await,
        0
    );
    assert_eq!(
        store.failed_offboarding_attempts(),
        1,
        "the failed tenant waits out its backoff instead of failing again"
    );
}

/// The worker stops when it is told to: a pass with a cancelled token does
/// nothing, and a running worker returns once cancelled.
#[tokio::test]
async fn the_worker_stops_when_cancelled() {
    let tenant = Uuid::now_v7();
    let store = Arc::new(FakeGraphStore::new());
    conformance::fence(store.as_ref(), tenant, None)
        .await
        .expect("the fence");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        run_pending(
            store.as_ref(),
            settings(1_000),
            &mut WorkerState::default(),
            &cancelled
        )
        .await,
        0
    );
    assert_eq!(
        conformance::pending_tenants(store.as_ref()).await,
        vec![tenant]
    );

    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_worker(
        Arc::clone(&store) as Arc<dyn GraphStoreV1>,
        settings(1_000),
        cancel.clone(),
    ));
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the worker returns once cancelled")
        .expect("the worker does not panic");
}

// --- REST --------------------------------------------------------------------

struct NoopRegistry;

impl toolkit::api::OpenApiRegistry for NoopRegistry {
    fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("the body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// The REST surface: `POST` answers 202 with the record, `{}` is a complete
/// body, `GET` reads it back, and the state turns `completed` once the
/// worker ran.
#[tokio::test]
async fn the_rest_routes_start_and_report_an_offboarding() {
    let tenant = Uuid::now_v7();
    let stand = stand(vec![tenant], vec![tenant]);
    stand.seed(tenant).await;
    let router = register_routes(
        Router::new(),
        &NoopRegistry,
        Arc::clone(&stand.harness.services),
    )
    .layer(axum::Extension(stand.service.clone()));
    let path = format!("/graph-storage/v1/tenants/{tenant}/offboarding");

    let (status, body) = send(
        &router,
        Request::builder()
            .method("POST")
            .uri(&path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("a valid request"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["tenant_id"], tenant.to_string());
    assert_eq!(body["state"], "in_progress");
    assert!(body["deletion_generation"].is_null(), "{body}");

    one_pass(stand.store.as_ref(), 1_000).await;
    let (status, body) = send(
        &router,
        Request::builder()
            .uri(&path)
            .body(Body::empty())
            .expect("a valid request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "completed");
    assert!(body["completed_at"].is_string(), "{body}");
    assert!(body["removed"]["nodes"].as_u64().unwrap() >= 2, "{body}");

    let other = Uuid::now_v7();
    let (status, _) = send(
        &router,
        Request::builder()
            .uri(format!("/graph-storage/v1/tenants/{other}/offboarding"))
            .body(Body::empty())
            .expect("a valid request"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- against the platform's own tenant tree ----------------------------------

/// The platform's static tenant resolver and its tenant-hierarchy policy,
/// with the gear's real resolver adapter: a tenant the platform marked
/// deleted is still one the policy resolves for a named-tenant grant, and
/// the adapter reads its status from the resolver it finds in the hub.
mod platform {
    use super::*;
    use authz_resolver_sdk::models::EvaluationRequest;
    use graph_storage::infra::tenants::HubTenantDirectory;
    use tenant_resolver_sdk::{
        GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
        GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverClient,
        TenantResolverError, TenantResolverPluginClient,
    };

    struct Tree(static_tr_plugin::domain::Service);

    #[async_trait]
    impl TenantResolverClient for Tree {
        async fn get_tenant(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.0.get_tenant(ctx, id).await
        }

        async fn get_root_tenant(
            &self,
            ctx: &SecurityContext,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.0.get_root_tenant(ctx).await
        }

        async fn get_tenants(
            &self,
            ctx: &SecurityContext,
            ids: &[TenantId],
            options: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            self.0.get_tenants(ctx, ids, options).await
        }

        async fn get_ancestors(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
            options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            self.0.get_ancestors(ctx, id, options).await
        }

        async fn get_descendants(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
            options: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            self.0.get_descendants(ctx, id, options).await
        }

        async fn is_ancestor(
            &self,
            ctx: &SecurityContext,
            ancestor_id: TenantId,
            descendant_id: TenantId,
            options: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            self.0
                .is_ancestor(ctx, ancestor_id, descendant_id, options)
                .await
        }
    }

    struct TrPdp(tr_authz_plugin::domain::Service);

    #[async_trait]
    impl AuthZResolverApi for TrPdp {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            Ok(self.0.evaluate(&request).await)
        }
    }

    #[tokio::test]
    async fn a_tenant_the_platform_marked_deleted_is_offboarded_and_a_live_one_is_not() {
        let (root, gone, live) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let config: static_tr_plugin::config::StaticTrPluginConfig =
            serde_json::from_value(serde_json::json!({
                "tenants": [
                    { "id": root, "name": "root" },
                    { "id": gone, "name": "gone", "parent_id": root, "status": "deleted" },
                    { "id": live, "name": "live", "parent_id": root },
                ]
            }))
            .expect("the tenant tree configures");
        let resolver = || {
            Arc::new(Tree(
                static_tr_plugin::domain::Service::from_config(&config).expect("a valid tree"),
            ))
        };
        let hub = Arc::new(toolkit::client_hub::ClientHub::new());
        hub.register::<dyn TenantResolverClient>(resolver());
        let directory = Arc::new(HubTenantDirectory::new(hub));
        let pdp = TrPdp(tr_authz_plugin::domain::Service::new(resolver()));
        let store = Arc::new(FakeGraphStore::new());
        let harness = Harness::offboarding(
            Arc::clone(&store),
            Arc::new(pdp),
            Arc::clone(&directory) as Arc<dyn TenantDirectory>,
        );
        let client = GraphStorageLocalClient::new(Arc::clone(&harness.services));
        let service = SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(root)
            .build()
            .expect("a valid security context");

        let record = client
            .offboard_tenant(&service, gone, OffboardRequest::default())
            .await
            .expect("the grant resolves for a deleted tenant and the platform confirms it");
        assert_eq!(record.tenant_id, gone);

        let refused = client
            .offboard_tenant(&service, live, OffboardRequest::default())
            .await
            .expect_err("a live tenant is not offboarded");
        assert_eq!(reason_of(&refused), "TENANT_NOT_DELETED");
        assert_eq!(conformance::record_of(store.as_ref(), live).await, None);

        // A tenant the platform does not know: the hierarchy policy refuses
        // it before the resolver is asked, so it is not found, and the
        // adapter, asked directly, says unknown rather than live.
        let unknown = Uuid::now_v7();
        let refused = client
            .offboard_tenant(&service, unknown, OffboardRequest::default())
            .await
            .expect_err("an unknown tenant is not offboarded");
        assert_eq!(refused.status_code(), 404, "{refused:?}");
        assert_eq!(
            directory
                .state(&service, unknown, Duration::from_secs(5))
                .await
                .expect("the resolver answers"),
            TenantState::Unknown
        );
        assert_eq!(conformance::record_of(store.as_ref(), unknown).await, None);
    }

    /// A resolver that answers every question the same way, or never.
    enum Scripted {
        Hang,
        Fail(fn() -> TenantResolverError),
    }

    impl Scripted {
        async fn answer<T>(&self) -> Result<T, TenantResolverError> {
            match self {
                Self::Hang => std::future::pending().await,
                Self::Fail(error) => Err(error()),
            }
        }
    }

    #[async_trait]
    impl TenantResolverClient for Scripted {
        async fn get_tenant(
            &self,
            _: &SecurityContext,
            _: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.answer().await
        }

        async fn get_root_tenant(
            &self,
            _: &SecurityContext,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.answer().await
        }

        async fn get_tenants(
            &self,
            _: &SecurityContext,
            _: &[TenantId],
            _: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            self.answer().await
        }

        async fn get_ancestors(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            self.answer().await
        }

        async fn get_descendants(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            self.answer().await
        }

        async fn is_ancestor(
            &self,
            _: &SecurityContext,
            _: TenantId,
            _: TenantId,
            _: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            self.answer().await
        }
    }

    fn directory_over(resolver: Scripted) -> HubTenantDirectory {
        let hub = Arc::new(toolkit::client_hub::ClientHub::new());
        hub.register::<dyn TenantResolverClient>(Arc::new(resolver));
        HubTenantDirectory::new(hub)
    }

    fn anyone() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(Uuid::now_v7())
            .build()
            .expect("a valid security context")
    }

    /// A resolver that does not answer is a deadline within the time the
    /// request has left, not a request held open with it.
    #[tokio::test(start_paused = true)]
    async fn a_resolver_that_does_not_answer_is_a_deadline() {
        let answer = directory_over(Scripted::Hang)
            .state(&anyone(), Uuid::now_v7(), Duration::from_secs(2))
            .await;
        assert!(matches!(answer, Err(DomainError::Deadline)), "{answer:?}");
    }

    /// Each resolver failure keeps its meaning: a refusal is not an outage,
    /// an outage is not a refusal, and a fault is neither.
    #[tokio::test]
    async fn each_resolver_failure_maps_to_its_own_kind() {
        type Failure = fn() -> TenantResolverError;
        let cases: [(Failure, &str); 4] = [
            (|| TenantResolverError::Unauthorized, "AccessDenied"),
            (|| TenantResolverError::NoPluginAvailable, "Unavailable"),
            (
                || TenantResolverError::ServiceUnavailable("down".to_owned()),
                "Unavailable",
            ),
            (
                || TenantResolverError::Internal("a bug".to_owned()),
                "Internal",
            ),
        ];
        for (error, expected) in cases {
            let answer = directory_over(Scripted::Fail(error))
                .state(&anyone(), Uuid::now_v7(), Duration::from_secs(5))
                .await
                .expect_err("a failure");
            assert!(
                format!("{answer:?}").starts_with(expected),
                "{expected}: {answer:?}"
            );
        }
    }
}

/// The worker the gear runs: started over a fenced tenant, its first pass
/// finishes the deletion, and it stops once cancelled in its pause.
#[tokio::test(start_paused = true)]
async fn the_worker_finishes_a_deletion_and_stops_in_its_pause() {
    let tenant = Uuid::now_v7();
    let store = Arc::new(FakeGraphStore::new());
    conformance::fence(store.as_ref(), tenant, None)
        .await
        .expect("the fence");
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_worker(
        Arc::clone(&store) as Arc<dyn GraphStoreV1>,
        settings(1_000),
        cancel.clone(),
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        conformance::record_of(store.as_ref(), tenant)
            .await
            .expect("the record")
            .is_complete(),
        "the first pass finished the deletion"
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the worker returns once cancelled")
        .expect("the worker does not panic");
}
