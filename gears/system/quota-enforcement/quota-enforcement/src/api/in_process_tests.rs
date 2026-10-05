#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    EnforcementMode, MetricId, PageRequest, PeriodType, QuotaFilter, QuotaManagerClientV1,
    QuotaPatch, QuotaSource, QuotaSpec, QuotaStatus, QuotaType, SubjectRef,
};
use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, Problem};

use super::InProcessQuotaManager;
use crate::domain::Service;
use crate::domain::admission::Admission;
use crate::domain::bootstrap::Bound;
use crate::domain::catalog::{CatalogBuilder, CatalogConfig, MetricClassifications};
use crate::domain::quotas::QuotaLimits;
use crate::domain::readiness::Readiness;
use crate::test_support::{
    FakeContractRegistry, FakeMetricRegistry, LLM_TENANT_PROJECTION, LLM_USER_PROJECTION,
    METRIC_TOKENS, NoopCoordinator, PermitTenantsPdp, RecordingMetrics, ctx, tenant,
};

fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

fn spec() -> QuotaSpec {
    QuotaSpec {
        tenant_id: tenant(),
        subject: SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: "u1".to_owned(),
        },
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        quota_type: QuotaType::Consumption,
        period: Some(PeriodType::Month),
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(100),
        notification_thresholds: Vec::new(),
        validity_window: None,
        fail_open_hint: false,
        metadata: json!({ "regions": ["eu"], "weight": 5 })
            .as_object()
            .cloned()
            .expect("object"),
        source: QuotaSource::Operator,
    }
}

fn unbound_service() -> Arc<Service> {
    unbound_service_with(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])))
}

fn unbound_service_with(pdp: Arc<dyn authz_resolver_sdk::AuthZResolverApi>) -> Arc<Service> {
    let metrics = Arc::new(RecordingMetrics::default());
    let enforcer = PolicyEnforcer::new(pdp);
    Arc::new(Service::new(
        Admission::new(enforcer, metrics),
        Arc::new(Readiness::new()),
        QuotaLimits {
            metadata_max_bytes: 4096,
            list_max_limit: 500,
            list_max_ids: 100,
            bulk_max_items: 50,
        },
        crate::test_support::policy_limits(),
        crate::domain::service::OperationsRuntime {
            cache_entries: 16,
            cache_ttl: std::time::Duration::from_secs(5),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
            snapshot: crate::domain::operations::SnapshotLimits::default(),
        },
    ))
}

async fn bound_service() -> Arc<Service> {
    bound_service_over(Arc::new(InMemoryStorage::new())).await
}

/// [`bound_service`] over a storage the caller prepared, e.g. bootstrapped
/// with the global policy so operations can evaluate.
async fn bound_service_over(storage: Arc<InMemoryStorage>) -> Arc<Service> {
    bind(unbound_service(), storage).await
}

async fn bind(service: Arc<Service>, storage: Arc<InMemoryStorage>) -> Arc<Service> {
    let registry = Arc::new(FakeContractRegistry::llm_gateway());
    let metrics = RecordingMetrics::default();
    let catalog = CatalogBuilder::new(registry.as_ref(), &metrics)
        .build(&CatalogConfig {
            subject_projections: vec![type_id(LLM_USER_PROJECTION), type_id(LLM_TENANT_PROJECTION)],
            resource_projections: Vec::new(),
        })
        .await
        .expect("catalogue");
    service
        .bind(Bound {
            engines: Arc::new(crate::domain::engines::builtin_registry().expect("engines")),
            artifacts: Arc::new(crate::domain::engines::PolicyArtifactCache::new(
                std::num::NonZeroUsize::new(256).expect("capacity"),
                std::num::NonZeroUsize::new(2).expect("permits"),
            )),
            storage,
            coordinator: Arc::new(NoopCoordinator),
            catalog: Arc::new(catalog),
            registry,
            metric_registry: Arc::new(FakeMetricRegistry::classified()),
            classifications: Arc::new(MetricClassifications::from_pairs([(
                MetricId::parse(METRIC_TOKENS).expect("metric"),
                crate::domain::ports::metric_registry::MetricDescriptor {
                    kind: quota_enforcement_sdk::MetricKind::Counter,
                    mode: crate::domain::ports::metric_registry::MetricMode::QuotaGated,
                },
            )])),
        })
        .expect("bind");
    service
}

fn status(err: CanonicalError) -> u16 {
    Problem::from(err).status.expect("status")
}

#[tokio::test]
async fn before_bootstrap_every_call_is_not_ready() {
    let client = InProcessQuotaManager::new(unbound_service());
    let err = client
        .create_quota(&ctx(), spec())
        .await
        .expect_err("not ready");
    assert_eq!(status(err), 503);
    let err = client
        .read_quotas(&ctx(), QuotaFilter::default(), PageRequest::default())
        .await
        .expect_err("not ready");
    assert_eq!(status(err), 503);
}

#[tokio::test]
async fn the_client_round_trips_through_the_same_domain_path_as_rest() {
    let client = InProcessQuotaManager::new(bound_service().await);
    let id = client.create_quota(&ctx(), spec()).await.expect("create");
    client
        .update_quota(
            &ctx(),
            id,
            QuotaPatch {
                fail_open_hint: Some(true),
                ..QuotaPatch::default()
            },
        )
        .await
        .expect("update returns unit");
    let page = client
        .read_quotas(&ctx(), QuotaFilter::default(), PageRequest::default())
        .await
        .expect("read");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].quota.id, id);
    assert!(page.items[0].quota.fail_open_hint);
    assert_eq!(page.items[0].quota.record_version, 2);
    assert!(page.items[0].currently_within_window);
    let outcome = client
        .deactivate_quota(&ctx(), id)
        .await
        .expect("deactivate");
    assert!(outcome.resolved_leases.is_empty());
    let page = client
        .read_quotas(
            &ctx(),
            QuotaFilter {
                status: Some(QuotaStatus::Deactivated),
                ..QuotaFilter::default()
            },
            PageRequest::default(),
        )
        .await
        .expect("read deactivated");
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn domain_rejections_arrive_as_canonical_errors() {
    let client = InProcessQuotaManager::new(bound_service().await);
    let rate = QuotaSpec {
        quota_type: QuotaType::Rate,
        period: None,
        ..spec()
    };
    let err = client.create_quota(&ctx(), rate).await.expect_err("rate");
    assert_eq!(status(err), 501);
    let err = client
        .update_quota(
            &ctx(),
            quota_enforcement_sdk::QuotaId::generate(),
            QuotaPatch::default(),
        )
        .await
        .expect_err("an empty patch is rejected before the lookup");
    assert_eq!(status(err), 400);
}

#[tokio::test]
async fn a_caller_supplied_constraint_contract_is_rejected_before_the_lookup() {
    let client = InProcessQuotaManager::new(bound_service().await);
    let id = client.create_quota(&ctx(), spec()).await.expect("created");
    let err = client
        .update_quota(
            &ctx(),
            id,
            QuotaPatch {
                metadata: Some(serde_json::Map::new()),
                constraint_contract: Some(quota_enforcement_sdk::ContractRef {
                    type_id: type_id(crate::test_support::LLM_TOKEN_CONSTRAINT),
                    version: 1,
                }),
                ..QuotaPatch::default()
            },
        )
        .await
        .expect_err("the gear alone names the accepted contract");
    assert!(
        format!("{err:?}").contains("CONSTRAINT_CONTRACT_NOT_CALLER_SUPPLIED"),
        "{err:?}"
    );
    assert_eq!(status(err), 400);
}

#[tokio::test]
async fn credit_is_a_manager_operation_through_the_same_domain_path() {
    let client = InProcessQuotaManager::new(unbound_service());
    let err = client
        .credit(
            &ctx(),
            quota_enforcement_sdk::CreditRequest {
                tenant_id: tenant(),
                quota_id: quota_enforcement_sdk::QuotaId::generate(),
                amount: 1,
                idempotency_key: "c1".to_owned(),
            },
        )
        .await
        .expect_err("not ready");
    assert_eq!(status(err), 503);

    let client = InProcessQuotaManager::new(bound_service().await);
    let id = client.create_quota(&ctx(), spec()).await.expect("create");
    let decision = client
        .credit(
            &ctx(),
            quota_enforcement_sdk::CreditRequest {
                tenant_id: tenant(),
                quota_id: id,
                amount: 1,
                idempotency_key: "c1".to_owned(),
            },
        )
        .await
        .expect("credit");
    assert_eq!(
        decision.result,
        quota_enforcement_sdk::DecisionResult::Allowed
    );
}

fn policy_spec() -> quota_enforcement_sdk::PolicySpec {
    quota_enforcement_sdk::PolicySpec {
        scope: quota_enforcement_sdk::PolicyScope::Metric {
            metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        },
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        comment: Some("v1".to_owned()),
    }
}

fn attribution() -> quota_enforcement_sdk::EvaluationAttribution {
    quota_enforcement_sdk::EvaluationAttribution {
        tenant_id: tenant(),
        metric: METRIC_TOKENS.to_owned(),
        subjects: vec![quota_enforcement_sdk::SubjectClaim {
            kind: quota_enforcement_sdk::SCOPE_USER.to_owned(),
            id: "u1".to_owned(),
        }],
        metadata: Some(
            json!({ "region": "eu" })
                .as_object()
                .cloned()
                .expect("object"),
        ),
        resource: None,
    }
}

#[tokio::test]
async fn before_bootstrap_every_operator_and_enforcement_call_is_not_ready() {
    use quota_enforcement_sdk::{QuotaEnforcementClientV1, QuotaOperatorClientV1};
    let service = unbound_service();
    let operator = super::InProcessQuotaOperator::new(Arc::clone(&service));
    let enforcement = super::InProcessQuotaEnforcement::new(service);
    let id = quota_enforcement_sdk::PolicyId::new("p");
    let token = quota_enforcement_sdk::LeaseToken::new(uuid::Uuid::from_u128(1));
    let patch = quota_enforcement_sdk::PolicyPatch {
        if_match_version: 1,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: None,
    };

    let refusals = [
        operator.create_policy(&ctx(), policy_spec()).await.err(),
        operator
            .update_policy(&ctx(), id.clone(), patch)
            .await
            .err(),
        operator
            .rollback_policy(&ctx(), id.clone(), 1, None)
            .await
            .err(),
        operator.delete_policy(&ctx(), id.clone(), None).await.err(),
        operator.read_policy(&ctx(), id.clone(), None).await.err(),
        operator
            .list_policy_versions(&ctx(), id, PageRequest::default())
            .await
            .err(),
        enforcement
            .debit(
                &ctx(),
                quota_enforcement_sdk::DebitRequest {
                    attribution: attribution(),
                    amount: 1,
                    idempotency_key: "d".to_owned(),
                },
            )
            .await
            .err(),
        enforcement
            .rollback(
                &ctx(),
                quota_enforcement_sdk::RollbackRequest {
                    attribution: attribution(),
                    original_operation: quota_enforcement_sdk::RollbackableOperation::Debit,
                    original_idempotency_key: "d".to_owned(),
                    idempotency_key: "r".to_owned(),
                },
            )
            .await
            .err(),
        enforcement
            .evaluate_preview(
                &ctx(),
                quota_enforcement_sdk::PreviewRequest {
                    attribution: attribution(),
                    amount: 1,
                },
            )
            .await
            .err(),
        enforcement
            .acquire_lease(
                &ctx(),
                quota_enforcement_sdk::AcquireLeaseRequest {
                    attribution: attribution(),
                    amount: 1,
                    ttl_secs: Some(60),
                    idempotency_key: "a".to_owned(),
                },
            )
            .await
            .err(),
        enforcement
            .commit_lease(
                &ctx(),
                quota_enforcement_sdk::CommitLeaseRequest {
                    tenant_id: tenant(),
                    token,
                    actual_amount: None,
                    idempotency_key: "c".to_owned(),
                },
            )
            .await
            .err(),
        enforcement
            .release_lease(
                &ctx(),
                quota_enforcement_sdk::ReleaseLeaseRequest {
                    tenant_id: tenant(),
                    token,
                    idempotency_key: "l".to_owned(),
                },
            )
            .await
            .err(),
    ];
    for (index, refusal) in refusals.into_iter().enumerate() {
        let err = refusal.unwrap_or_else(|| panic!("call {index} answered before bootstrap"));
        assert_eq!(status(err), 503, "call {index}");
    }
}

#[tokio::test]
async fn the_operator_client_drives_a_policy_through_its_versions() {
    use quota_enforcement_sdk::QuotaOperatorClientV1;
    let service = bind(
        unbound_service_with(Arc::new(crate::test_support::PermitUnconstrainedPdp)),
        Arc::new(InMemoryStorage::new()),
    )
    .await;
    let operator = super::InProcessQuotaOperator::new(service);
    let created = operator
        .create_policy(&ctx(), policy_spec())
        .await
        .expect("create");
    let id = created.policy_id.clone();
    let updated = operator
        .update_policy(
            &ctx(),
            id.clone(),
            quota_enforcement_sdk::PolicyPatch {
                if_match_version: created.version,
                engine_id: None,
                engine_config: None,
                timeout_ms: Some(50),
                comment: Some("v2".to_owned()),
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.version, 2);
    assert_eq!(
        operator
            .read_policy(&ctx(), id.clone(), Some(1))
            .await
            .expect("read v1")
            .version,
        1
    );
    let history = operator
        .list_policy_versions(
            &ctx(),
            id.clone(),
            PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .expect("history");
    assert_eq!(history.items.len(), 2);
    let rolled_back = operator
        .rollback_policy(&ctx(), id.clone(), 1, Some("back".to_owned()))
        .await
        .expect("rollback");
    assert_eq!(
        rolled_back.timeout_ms, None,
        "v1's configuration is active again"
    );
    operator
        .delete_policy(&ctx(), id.clone(), None)
        .await
        .expect("delete");
    let err = operator
        .read_policy(&ctx(), id, None)
        .await
        .expect_err("a deleted policy has no active version");
    assert!(status(err) >= 400);
}

#[tokio::test]
async fn the_enforcement_client_debits_previews_rolls_back_and_settles_leases() {
    use quota_enforcement_sdk::{AcquireLeaseOutcome, DecisionResult, QuotaEnforcementClientV1};
    let storage = Arc::new(InMemoryStorage::new());
    {
        use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
        let mut bundle = quota_enforcement_sdk::testing::bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            policy.engine_id = "most-restrictive-wins".to_owned();
        }
        storage.bootstrap(&bundle).await.expect("bootstrap");
    }
    let service = bound_service_over(Arc::clone(&storage)).await;
    let quota = InProcessQuotaManager::new(Arc::clone(&service))
        .create_quota(&ctx(), spec())
        .await
        .expect("quota");
    let client = super::InProcessQuotaEnforcement::new(service);

    let preview = client
        .evaluate_preview(
            &ctx(),
            quota_enforcement_sdk::PreviewRequest {
                attribution: attribution(),
                amount: 10,
            },
        )
        .await
        .expect("preview");
    assert!(preview.preview);
    assert_eq!(storage.consumed(quota), 0, "a preview moves nothing");

    let debit = client
        .debit(
            &ctx(),
            quota_enforcement_sdk::DebitRequest {
                attribution: attribution(),
                amount: 10,
                idempotency_key: "d1".to_owned(),
            },
        )
        .await
        .expect("debit");
    assert_eq!(debit.result, DecisionResult::Allowed);
    assert_eq!(storage.consumed(quota), 10);

    client
        .rollback(
            &ctx(),
            quota_enforcement_sdk::RollbackRequest {
                attribution: attribution(),
                original_operation: quota_enforcement_sdk::RollbackableOperation::Debit,
                original_idempotency_key: "d1".to_owned(),
                idempotency_key: "rb1".to_owned(),
            },
        )
        .await
        .expect("rollback");
    assert_eq!(
        storage.consumed(quota),
        0,
        "the rollback reversed the debit"
    );

    let acquire = |key: &str| quota_enforcement_sdk::AcquireLeaseRequest {
        attribution: attribution(),
        amount: 30,
        ttl_secs: Some(60),
        idempotency_key: key.to_owned(),
    };
    let AcquireLeaseOutcome::Acquired {
        token: committed, ..
    } = client
        .acquire_lease(&ctx(), acquire("a1"))
        .await
        .expect("acquire")
    else {
        panic!("the first lease fits");
    };
    let AcquireLeaseOutcome::Acquired {
        token: released, ..
    } = client
        .acquire_lease(&ctx(), acquire("a2"))
        .await
        .expect("acquire")
    else {
        panic!("the second lease fits");
    };
    client
        .commit_lease(
            &ctx(),
            quota_enforcement_sdk::CommitLeaseRequest {
                tenant_id: tenant(),
                token: committed,
                actual_amount: Some(12),
                idempotency_key: "c1".to_owned(),
            },
        )
        .await
        .expect("commit");
    client
        .release_lease(
            &ctx(),
            quota_enforcement_sdk::ReleaseLeaseRequest {
                tenant_id: tenant(),
                token: released,
                idempotency_key: "r1".to_owned(),
            },
        )
        .await
        .expect("release");
    assert_eq!(
        storage.consumed(quota),
        12,
        "only the committed share stays"
    );
}
