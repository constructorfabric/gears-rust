#![allow(clippy::expect_used)]

use crate::domain::operations::ReturnRequest;
use std::sync::Arc;

use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, Actor, CommitLeaseRequest, DebitRequest,
    DecisionResult, EnforcementMode, EvaluationAttribution, MetricId, MetricRef,
    QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType, ReleaseLeaseRequest,
    RollbackableOperation, SnapshotRequest, SnapshotSubject, SubjectClaim, SubjectRef, TenantId,
};
use serde_json::json;
use toolkit_security::{AccessScope, PlatformIdentity};
use uuid::Uuid;

use super::TrustedOperations;
use crate::domain::error::DomainError;
use crate::domain::service::Service;
use crate::test_support::{
    METRIC_TOKENS, PermitTenantsPdp, USER_SUBJECT_TYPE, UncallablePdp, actor, bound_service_over,
    caller, mrw_storage, tenant,
};

fn other_tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x7e57_0000_0000_0000_0000_0000_0000_0002))
}

fn resolver_caller() -> PlatformIdentity {
    PlatformIdentity::Shared {
        name: "billing".to_owned(),
    }
}

fn attribution() -> EvaluationAttribution {
    EvaluationAttribution {
        tenant_id: tenant(),
        subjects: vec![SubjectClaim {
            subject_type: USER_SUBJECT_TYPE.to_owned(),
            id: "u-1".to_owned(),
        }],
        metric: MetricRef {
            metric_type: METRIC_TOKENS.to_owned(),
            id: None,
            metadata: json!({ "region": "eu-west-1" }).as_object().cloned(),
        },
    }
}

fn debit(key: &str) -> DebitRequest {
    DebitRequest {
        attribution: attribution(),
        amount: 10,
        idempotency_key: key.to_owned(),
    }
}

fn acquire(key: &str) -> AcquireLeaseRequest {
    AcquireLeaseRequest {
        attribution: attribution(),
        amount: 5,
        ttl_secs: Some(60),
        idempotency_key: key.to_owned(),
    }
}

fn commit(token: quota_enforcement_sdk::LeaseToken, key: &str) -> CommitLeaseRequest {
    CommitLeaseRequest {
        tenant_id: tenant(),
        token,
        actual_amount: None,
        metric_type: None,
        idempotency_key: key.to_owned(),
    }
}

fn release(token: quota_enforcement_sdk::LeaseToken, key: &str) -> ReleaseLeaseRequest {
    ReleaseLeaseRequest {
        tenant_id: tenant(),
        token,
        idempotency_key: key.to_owned(),
    }
}

fn snapshot() -> SnapshotRequest {
    SnapshotRequest {
        tenant_id: tenant(),
        subjects: vec![SnapshotSubject {
            subject_type: USER_SUBJECT_TYPE.to_owned(),
            id: "u-1".to_owned(),
            metric: METRIC_TOKENS.to_owned(),
            metadata: None,
        }],
        limit: None,
        cursor: None,
    }
}

async fn quota(storage: &InMemoryStorage) -> QuotaId {
    storage
        .create_quota(
            &actor(),
            &AccessScope::allow_all(),
            quota_enforcement_sdk::QuotaDraft {
                tenant_id: tenant(),
                subject: SubjectRef {
                    subject_type: gts::GtsTypeId::try_new(USER_SUBJECT_TYPE).expect("type"),
                    subject_id: "u-1".to_owned(),
                },
                metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
                quota_type: QuotaType::Allocation,
                period: None,
                enforcement_mode: EnforcementMode::Hard,
                cap: Some(100),
                notification_thresholds: Vec::new(),
                validity_window: None,
                fail_open_hint: false,
                metadata: serde_json::Map::new(),
                source: QuotaSource::Operator,
            },
            &[],
        )
        .await
        .expect("quota")
}

fn acquired(outcome: AcquireLeaseOutcome) -> quota_enforcement_sdk::LeaseToken {
    match outcome {
        AcquireLeaseOutcome::Acquired { token, .. } => token,
        AcquireLeaseOutcome::Denied { decision } => panic!("denied: {decision:?}"),
    }
}

async fn trusted() -> (TrustedOperations, Arc<InMemoryStorage>, QuotaId) {
    let storage = mrw_storage().await;
    let id = quota(&storage).await;
    let service: Arc<Service> =
        bound_service_over(Arc::new(UncallablePdp), Arc::clone(&storage)).await;
    (TrustedOperations::new(service), storage, id)
}

#[tokio::test]
async fn every_trusted_operation_runs_without_a_pdp_call_under_the_tenant_only_scope() {
    let (ops, storage, id) = trusted().await;
    let caller = Some(resolver_caller());

    let decision = ops
        .debit(tenant(), caller.clone(), debit("d-1"))
        .await
        .expect("debit");
    assert_eq!(decision.result, DecisionResult::Allowed);
    let token = acquired(
        ops.acquire_lease(tenant(), caller.clone(), acquire("a-1"))
            .await
            .expect("acquire"),
    );
    ops.commit_lease(tenant(), caller.clone(), commit(token, "c-1"))
        .await
        .expect("commit");
    let token = acquired(
        ops.acquire_lease(tenant(), caller.clone(), acquire("a-2"))
            .await
            .expect("acquire"),
    );
    ops.release_lease(tenant(), caller.clone(), release(token, "r-1"))
        .await
        .expect("release");
    let page = ops
        .snapshot(tenant(), caller, snapshot())
        .await
        .expect("snapshot");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].quota_id, id);
    assert_eq!(
        storage.consumed(id),
        15,
        "the debit and the committed lease"
    );
    let scopes = storage.scopes();
    assert!(scopes.len() >= 6, "every operation reached storage");
    assert!(
        scopes
            .iter()
            .all(|scope| *scope == AccessScope::for_tenant(tenant().as_uuid())),
        "{scopes:?}"
    );
}

#[tokio::test]
async fn a_return_runs_without_a_pdp_call_names_only_the_charge_and_records_the_caller() {
    let (ops, storage, id) = trusted().await;
    ops.debit(tenant(), None, debit("d-1"))
        .await
        .expect("debit");

    ops.return_amount(
        tenant(),
        Some(resolver_caller()),
        ReturnRequest {
            tenant_id: other_tenant(),
            operation: RollbackableOperation::Debit,
            charge_key: "d-1".to_owned(),
            metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
            amount: 4,
            idempotency_key: "ret-1".to_owned(),
        },
    )
    .await
    .expect("return");

    assert_eq!(storage.consumed(id), 6);
    assert_eq!(
        storage.scopes().last(),
        Some(&AccessScope::for_tenant(tenant().as_uuid())),
        "the tenant the call names replaces the request's"
    );
    assert_eq!(
        storage.operation_log_actors().last(),
        Some(&Actor::Platform {
            identity: Some(resolver_caller())
        })
    );
}

#[tokio::test]
async fn the_operation_log_records_the_platform_identity_or_no_module() {
    let (ops, storage, _) = trusted().await;
    ops.debit(tenant(), Some(resolver_caller()), debit("d-1"))
        .await
        .expect("debit");
    ops.debit(tenant(), None, debit("d-2"))
        .await
        .expect("debit");

    assert_eq!(
        storage.operation_log_actors()[1..],
        [
            Actor::Platform {
                identity: Some(resolver_caller())
            },
            Actor::Platform { identity: None },
        ]
    );
    assert!(
        storage
            .events()
            .iter()
            .all(|event| matches!(event.actor, Actor::Platform { .. })),
        "the events name the same actor"
    );
}

#[tokio::test]
async fn a_trusted_call_reaches_only_the_tenant_it_names() {
    let (ops, storage, id) = trusted().await;
    let token = acquired(
        ops.acquire_lease(tenant(), None, acquire("a-1"))
            .await
            .expect("acquire"),
    );

    let err = ops
        .commit_lease(other_tenant(), None, commit(token, "c-1"))
        .await
        .expect_err("another tenant's token");
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
    let err = ops
        .release_lease(other_tenant(), None, release(token, "r-1"))
        .await
        .expect_err("another tenant's token");
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");

    let decision = ops
        .debit(other_tenant(), None, debit("d-1"))
        .await
        .expect("debit");
    assert!(
        matches!(&decision.result, DecisionResult::Denied { reason, .. }
            if reason == quota_enforcement_sdk::NO_APPLICABLE_QUOTA),
        "{decision:?}"
    );
    let page = ops
        .snapshot(other_tenant(), None, snapshot())
        .await
        .expect("snapshot");
    assert!(page.items.is_empty());
    assert_eq!(storage.consumed(id), 5, "only the held amount moved");
    assert_eq!(
        storage.scopes().last(),
        Some(&AccessScope::for_tenant(other_tenant().as_uuid()))
    );
}

#[tokio::test]
async fn a_malformed_target_is_rejected_before_storage() {
    let (ops, storage, _) = trusted().await;
    let nil = TenantId::new(Uuid::nil());

    let err = ops
        .debit(nil, None, debit("d-1"))
        .await
        .expect_err("nil tenant");
    assert!(
        matches!(err, DomainError::InvalidArgument { .. }),
        "{err:?}"
    );
    let err = ops
        .commit_lease(
            nil,
            None,
            commit(
                quota_enforcement_sdk::LeaseToken::new(Uuid::now_v7()),
                "c-1",
            ),
        )
        .await
        .expect_err("nil tenant");
    assert!(
        matches!(err, DomainError::InvalidArgument { .. }),
        "{err:?}"
    );
    let err = ops
        .snapshot(nil, None, snapshot())
        .await
        .expect_err("nil tenant");
    assert!(
        matches!(err, DomainError::InvalidSnapshot { .. }),
        "{err:?}"
    );

    assert!(storage.scopes().is_empty(), "no storage call");
    assert_eq!(storage.operation_log_len(), 1, "only the seeded Quota");
}

#[tokio::test]
async fn the_api_admits_the_same_operations_through_the_pdp_and_records_the_caller() {
    let storage = mrw_storage().await;
    quota(&storage).await;
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let service = bound_service_over(pdp.clone(), Arc::clone(&storage)).await;
    let ops = service.operations(caller()).expect("bound");

    ops.debit(debit("d-1")).await.expect("debit");
    let token = acquired(ops.acquire_lease(acquire("a-1")).await.expect("acquire"));
    ops.commit_lease(commit(token, "c-1"))
        .await
        .expect("commit");
    let token = acquired(ops.acquire_lease(acquire("a-2")).await.expect("acquire"));
    ops.release_lease(release(token, "r-1"))
        .await
        .expect("release");
    ops.snapshot(snapshot()).await.expect("snapshot");
    quota_enforcement_sdk::QuotaEnforcementApi::debit(
        &crate::api::in_process::InProcessQuotaEnforcement::new(Arc::clone(&service)),
        caller(),
        debit("d-2"),
    )
    .await
    .expect("in-process debit");

    assert_eq!(
        pdp.calls(),
        7,
        "one PDP call per operation, on either transport"
    );
    assert!(
        storage.operation_log_actors().len() > 1
            && storage
                .operation_log_actors()
                .iter()
                .all(|logged| *logged == actor()),
        "{:?}",
        storage.operation_log_actors()
    );
}
