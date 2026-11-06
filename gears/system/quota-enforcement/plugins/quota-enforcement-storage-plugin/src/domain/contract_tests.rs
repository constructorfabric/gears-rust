//! Every contract primitive reaches the store method of the same name: the
//! stores answer each call with an error naming the method that received it,
//! and the contract must return exactly that error.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quota_enforcement_sdk::engine::{EvaluationLimits, TransactionEvaluator};
use quota_enforcement_sdk::{
    ActiveQuotaCounts, ApplicableQuotas, AppliedMutation, AttributionDigest, BatchDebitItem,
    BatchEntry, BatchTimer, BulkCreateEnvelope, BulkCreated, BulkDeactivateEnvelope,
    BulkDeactivated, BulkUpdateEnvelope, BulkUpdated, DeactivateOutcome, EvaluatedBatch,
    EvaluatedDebit, EvaluatedLease, EvaluatedMutation, EvaluationContext, EvaluationFailure,
    ExpiredLease, IdempotencyRecord, IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite,
    LeaseToken, MetricId, NotificationEvent, OperationType, PageRequest, PageResult,
    PartialIdempotencyWrite, PayloadHash, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate,
    PolicyVersion, PolicyVersionMeta, ProjectionBinding, Quota, QuotaDraft,
    QuotaEnforcementStoragePluginV1, QuotaFilter, QuotaId, QuotaPatch, QuotaSnapshot,
    RollbackTarget, StorageError, TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::StoragePlugin;
use crate::domain::ports::{
    Actor, ConsumptionStore, LeaseStore, PolicyStore, QuotaStore, StoreError,
};
use crate::infra::storage::SqlFoundationStore;
use crate::test_support::{draft, global_policy_draft, scope_for, tenant, test_db, user};

/// Answers every store call with an error naming the method.
struct Tagging;

fn tag(method: &str) -> StorageError {
    StorageError::Internal(method.to_owned())
}

fn quota_tag(method: &str) -> StoreError {
    StoreError::InvalidPatch {
        detail: method.to_owned(),
    }
}

/// What a [`quota_tag`] becomes once the plugin lifts it to the contract.
fn lifted(method: &str) -> StorageError {
    StorageError::Internal(format!("invalid patch: {method}"))
}

fn err<T>(result: Result<T, StorageError>) -> StorageError {
    match result {
        Ok(_) => panic!("the tagging store answers every call with an error"),
        Err(error) => error,
    }
}

#[async_trait]
impl QuotaStore for Tagging {
    async fn create_quota(
        &self,
        _actor: &Actor,
        _scope: &AccessScope,
        _draft: QuotaDraft,
        _events: &[NotificationEvent],
    ) -> Result<QuotaId, StoreError> {
        Err(quota_tag("create_quota"))
    }

    async fn update_quota(
        &self,
        _actor: &Actor,
        _scope: &AccessScope,
        _quota_id: QuotaId,
        _patch: QuotaPatch,
        _events: &[NotificationEvent],
    ) -> Result<Quota, StoreError> {
        Err(quota_tag("update_quota"))
    }

    async fn deactivate_quota(
        &self,
        _actor: &Actor,
        _scope: &AccessScope,
        _quota_id: QuotaId,
        _events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StoreError> {
        Err(quota_tag("deactivate_quota"))
    }

    async fn read_quotas(
        &self,
        _scope: &AccessScope,
        _filter: QuotaFilter,
        _page: PageRequest,
    ) -> Result<PageResult<Quota>, StoreError> {
        Err(quota_tag("read_quotas"))
    }

    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StoreError> {
        Err(quota_tag("read_active_projection_bindings"))
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StoreError> {
        Err(quota_tag("read_active_quota_counts"))
    }

    async fn bulk_create_quotas(
        &self,
        _actor: &Actor,
        _envelope: &BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<BulkCreated>, StoreError> {
        Err(quota_tag("bulk_create_quotas"))
    }

    async fn bulk_update_quotas(
        &self,
        _actor: &Actor,
        _envelope: &BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<BulkUpdated>, StoreError> {
        Err(quota_tag("bulk_update_quotas"))
    }

    async fn bulk_deactivate_quotas(
        &self,
        _actor: &Actor,
        _envelope: &BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<BulkDeactivated>, StoreError> {
        Err(quota_tag("bulk_deactivate_quotas"))
    }
}

#[async_trait]
impl ConsumptionStore for Tagging {
    async fn apply_debit_plan(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _mutation: &EvaluatedMutation<'_>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        Err(tag("apply_debit_plan"))
    }

    async fn apply_batch_debit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _batch: &EvaluatedBatch<'_>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        Err(tag("apply_batch_debit"))
    }

    async fn apply_credit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _quota_id: QuotaId,
        _amount: u64,
        _idempotency: &PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        Err(tag("apply_credit"))
    }

    async fn apply_rollback(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _target: &RollbackTarget,
        _idempotency: &IdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        Err(tag("apply_rollback"))
    }

    async fn read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        Err(tag("read_quota_snapshot"))
    }

    async fn bulk_read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _pairs: &[ApplicableQuotas],
        _page: PageRequest,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        Err(tag("bulk_read_quota_snapshot"))
    }

    async fn lookup_idempotency(
        &self,
        _scope_of: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        Err(tag("lookup_idempotency"))
    }

    async fn reclaim_expired_idempotency(
        &self,
        _batch_size: u32,
        _before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        Err(tag("reclaim_expired_idempotency"))
    }

    async fn reclaim_operation_log(
        &self,
        _batch_size: u32,
        _before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        Err(tag("reclaim_operation_log"))
    }
}

#[async_trait]
impl LeaseStore for Tagging {
    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _mutation: &EvaluatedMutation<'_>,
        _ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        Err(tag("acquire_lease"))
    }

    async fn commit_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _token: LeaseToken,
        _actual_amount: Option<u64>,
        _idempotency: &PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        Err(tag("commit_lease"))
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _token: LeaseToken,
        _idempotency: &PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        Err(tag("release_lease"))
    }

    async fn reclaim_expired_leases(
        &self,
        _batch_size: u32,
        _before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        Err(tag("reclaim_expired_leases"))
    }

    async fn count_expired_unreclaimed_leases(
        &self,
        _before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        Err(tag("count_expired_unreclaimed_leases"))
    }
}

#[async_trait]
impl PolicyStore for Tagging {
    async fn create_policy(
        &self,
        _ctx: &SecurityContext,
        _draft: PolicyDraft,
        _events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        Err(tag("create_policy"))
    }

    async fn update_policy(
        &self,
        _ctx: &SecurityContext,
        _policy_id: PolicyId,
        _update: PolicyUpdate,
        _events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        Err(tag("update_policy"))
    }

    async fn rollback_policy(
        &self,
        _ctx: &SecurityContext,
        _policy_id: PolicyId,
        _target_version: u32,
        _comment: Option<String>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        Err(tag("rollback_policy"))
    }

    async fn delete_policy(
        &self,
        _ctx: &SecurityContext,
        _policy_id: PolicyId,
        _comment: Option<String>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        Err(tag("delete_policy"))
    }

    async fn read_policy(
        &self,
        _scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Err(tag("read_policy"))
    }

    async fn read_active_policy_by_id(
        &self,
        _policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Err(tag("read_active_policy_by_id"))
    }

    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        Err(tag("read_active_policies"))
    }

    async fn read_policy_version(
        &self,
        _policy_id: &PolicyId,
        _version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Err(tag("read_policy_version"))
    }

    async fn list_policy_versions(
        &self,
        _policy_id: &PolicyId,
        _page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        Err(tag("list_policy_versions"))
    }
}

/// The plugin over [`Tagging`], seen only through the contract.
async fn contract() -> Arc<dyn QuotaEnforcementStoragePluginV1> {
    let tagging = Arc::new(Tagging);
    Arc::new(StoragePlugin::new(
        Arc::new(SqlFoundationStore::new(test_db().await)),
        Arc::clone(&tagging) as Arc<dyn QuotaStore>,
        Arc::clone(&tagging) as Arc<dyn PolicyStore>,
        Arc::clone(&tagging) as Arc<dyn ConsumptionStore>,
        tagging as Arc<dyn LeaseStore>,
    ))
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_type("service")
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

fn write(operation_type: OperationType) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&[user("u1")]),
            operation_type,
            key: "k1".to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([1; 32]),
    }
}

fn partial() -> PartialIdempotencyWrite {
    PartialIdempotencyWrite {
        tenant_id: tenant(),
        key: "k1".to_owned(),
        payload_hash: PayloadHash::from_bytes([1; 32]),
    }
}

fn applicable() -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects: vec![user("u1")],
        metric: MetricId::parse(crate::test_support::METRIC_TOKENS).expect("metric"),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(50).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

/// Never called: the tagging store answers before any evaluation.
fn evaluator() -> Arc<TransactionEvaluator> {
    Arc::new(|_context: &EvaluationContext<'_>| {
        Err(EvaluationFailure::PreparationRequired {
            policy_id: PolicyId::new("unused"),
            version: 1,
        })
    })
}

#[tokio::test]
async fn quota_primitives_reach_their_store_methods() {
    let plugin = contract().await;
    let (ctx, scope, id) = (ctx(), scope_for(tenant()), QuotaId::generate());
    let deactivate = BulkDeactivateEnvelope {
        tenant_id: tenant(),
        idempotency: write(OperationType::BulkDeactivateQuotas),
        items: Vec::new(),
    };
    let update = BulkUpdateEnvelope {
        tenant_id: tenant(),
        idempotency: write(OperationType::BulkUpdateQuotas),
        items: Vec::new(),
    };
    let create = BulkCreateEnvelope {
        tenant_id: tenant(),
        idempotency: write(OperationType::BulkCreateQuotas),
        items: Vec::new(),
    };

    let answers = [
        (
            err(plugin
                .create_quota(&ctx, &scope, draft(tenant(), "u1", Some(5)), &[])
                .await),
            "create_quota",
        ),
        (
            err(plugin
                .update_quota(&ctx, &scope, id, QuotaPatch::default(), &[])
                .await),
            "update_quota",
        ),
        (
            err(plugin.deactivate_quota(&ctx, &scope, id, &[]).await),
            "deactivate_quota",
        ),
        (
            err(plugin
                .read_quotas(&ctx, &scope, QuotaFilter::default(), PageRequest::default())
                .await),
            "read_quotas",
        ),
        (
            err(plugin.read_active_projection_bindings().await),
            "read_active_projection_bindings",
        ),
        (
            err(plugin.read_active_quota_counts().await),
            "read_active_quota_counts",
        ),
        (
            err(plugin.bulk_create_quotas(&ctx, &create).await),
            "bulk_create_quotas",
        ),
        (
            err(plugin.bulk_update_quotas(&ctx, &update).await),
            "bulk_update_quotas",
        ),
        (
            err(plugin.bulk_deactivate_quotas(&ctx, &deactivate).await),
            "bulk_deactivate_quotas",
        ),
    ];
    for (answer, method) in answers {
        assert_eq!(answer, lifted(method), "{method}");
    }
}

#[tokio::test]
async fn consumption_and_lease_primitives_reach_their_store_methods() {
    let plugin = contract().await;
    let (ctx, scope) = (ctx(), scope_for(tenant()));
    let applicable = applicable();
    let null = serde_json::Value::Null;
    let debit = write(OperationType::Debit);
    let mutation = EvaluatedMutation {
        applicable: &applicable,
        amount: 1,
        request: &null,
        resource: &null,
        user_projection: None,
        limits: limits(),
        idempotency: &debit,
        authorized: AttributionDigest::from_bytes([7; 32]),
        evaluate: evaluator(),
    };
    let item = BatchDebitItem {
        applicable: applicable.clone(),
        amount: 1,
        request: null.clone(),
        resource: null.clone(),
        authorized: AttributionDigest::from_bytes([7; 32]),
        item_scope: None,
    };
    let entries = [BatchEntry {
        item: &item,
        scope: &scope,
        user_projection: None,
    }];
    let envelope = write(OperationType::BatchDebit);
    let batch = EvaluatedBatch {
        envelope: &envelope,
        items: &entries,
        limits: limits(),
        evaluate: evaluator(),
        timer: Arc::new(BatchTimer::new(Duration::from_secs(1))),
    };
    let rollback = RollbackTarget {
        original: debit.scope.clone(),
        authorized: AttributionDigest::from_bytes([7; 32]),
    };
    let token = LeaseToken::new(Uuid::from_u128(9));
    let now = OffsetDateTime::now_utc();

    let answers = [
        (
            err(plugin.apply_debit_plan(&ctx, &scope, &mutation, &[]).await),
            "apply_debit_plan",
        ),
        (
            err(plugin.apply_batch_debit(&ctx, &scope, &batch, &[]).await),
            "apply_batch_debit",
        ),
        (
            err(plugin
                .apply_credit(&ctx, &scope, QuotaId::generate(), 1, &partial(), &[])
                .await),
            "apply_credit",
        ),
        (
            err(plugin
                .apply_rollback(
                    &ctx,
                    &scope,
                    &rollback,
                    &write(OperationType::Rollback),
                    &[],
                )
                .await),
            "apply_rollback",
        ),
        (
            err(plugin.read_quota_snapshot(&ctx, &scope, &applicable).await),
            "read_quota_snapshot",
        ),
        (
            err(plugin
                .bulk_read_quota_snapshot(
                    &ctx,
                    &scope,
                    std::slice::from_ref(&applicable),
                    PageRequest::default(),
                )
                .await),
            "bulk_read_quota_snapshot",
        ),
        (
            err(plugin.lookup_idempotency(&debit.scope).await),
            "lookup_idempotency",
        ),
        (
            err(plugin.reclaim_expired_idempotency(10, now).await),
            "reclaim_expired_idempotency",
        ),
        (
            err(plugin.reclaim_operation_log(10, now).await),
            "reclaim_operation_log",
        ),
        (
            err(plugin
                .acquire_lease(&ctx, &scope, &mutation, Duration::from_secs(60))
                .await),
            "acquire_lease",
        ),
        (
            err(plugin
                .commit_lease(&ctx, &scope, token, Some(1), &partial(), &[])
                .await),
            "commit_lease",
        ),
        (
            err(plugin
                .release_lease(&ctx, &scope, token, &partial(), &[])
                .await),
            "release_lease",
        ),
        (
            err(plugin.reclaim_expired_leases(10, now).await),
            "reclaim_expired_leases",
        ),
        (
            err(plugin.count_expired_unreclaimed_leases(now).await),
            "count_expired_unreclaimed_leases",
        ),
    ];
    for (answer, method) in answers {
        assert_eq!(answer, tag(method), "{method}");
    }
}

#[tokio::test]
async fn policy_primitives_reach_their_store_methods() {
    let plugin = contract().await;
    let ctx = ctx();
    let id = PolicyId::new("global");
    let update: PolicyUpdate = serde_json::from_value(serde_json::json!({
        "if_match_version": 1,
        "created_by": "operator"
    }))
    .expect("update");

    let answers = [
        (
            err(plugin.create_policy(&ctx, global_policy_draft(), &[]).await),
            "create_policy",
        ),
        (
            err(plugin.update_policy(&ctx, id.clone(), update, &[]).await),
            "update_policy",
        ),
        (
            err(plugin.rollback_policy(&ctx, id.clone(), 1, None, &[]).await),
            "rollback_policy",
        ),
        (
            err(plugin.delete_policy(&ctx, id.clone(), None, &[]).await),
            "delete_policy",
        ),
        (
            err(plugin.read_policy(&PolicyScope::Global).await),
            "read_policy",
        ),
        (
            err(plugin.read_active_policy_by_id(&id).await),
            "read_active_policy_by_id",
        ),
        (
            err(plugin.read_active_policies().await),
            "read_active_policies",
        ),
        (
            err(plugin.read_policy_version(&id, 1).await),
            "read_policy_version",
        ),
        (
            err(plugin
                .list_policy_versions(&id, PageRequest::default())
                .await),
            "list_policy_versions",
        ),
    ];
    for (answer, method) in answers {
        assert_eq!(answer, tag(method), "{method}");
    }
}
