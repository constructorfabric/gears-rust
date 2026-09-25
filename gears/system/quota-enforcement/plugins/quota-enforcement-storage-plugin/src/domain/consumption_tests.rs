#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quota_enforcement_sdk::{
    ApplicableQuotas, AppliedMutation, EvaluatedDebit, EvaluatedMutation, IdempotencyRecord,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, NotificationEvent, OperationType,
    PartialIdempotencyWrite, QuotaId, QuotaSnapshot, RollbackTarget, StorageError, TenantId,
    TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{ConsumptionStore, StoragePlugin};
use crate::infra::storage::SqlFoundationStore;
use crate::test_support::{FakePolicyStore, FakeQuotaStore, test_db};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadCall {
    Lookup(IdempotencyScope),
    ReclaimIdempotency {
        batch_size: u32,
        before: OffsetDateTime,
    },
    ReclaimOperationLog {
        batch_size: u32,
        before: OffsetDateTime,
    },
}

#[derive(Default)]
struct RecordingConsumptionStore {
    calls: Mutex<Vec<ReadCall>>,
}

impl RecordingConsumptionStore {
    fn calls(&self) -> Vec<ReadCall> {
        self.calls.lock().expect("recording lock").clone()
    }
}

async fn plugin_over(store: Arc<RecordingConsumptionStore>) -> StoragePlugin {
    StoragePlugin::new(
        Arc::new(SqlFoundationStore::new(test_db().await)),
        Arc::new(FakeQuotaStore::default()),
        Arc::new(FakePolicyStore::default()),
        store,
        Arc::new(crate::test_support::FakeLeaseStore) as Arc<dyn crate::domain::ports::LeaseStore>,
    )
}

fn idempotency_scope() -> IdempotencyScope {
    IdempotencyScope {
        tenant_id: TenantId::new(Uuid::from_u128(1)),
        subject_key: IdempotencySubjectKey::from_bytes([2; 32]),
        operation_type: OperationType::Debit,
        key: "request-1".to_owned(),
    }
}

#[async_trait]
impl ConsumptionStore for RecordingConsumptionStore {
    async fn apply_debit_plan(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _mutation: &EvaluatedMutation<'_>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        Err(StorageError::Internal("not part of this test".into()))
    }

    async fn apply_batch_debit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _batch: &quota_enforcement_sdk::EvaluatedBatch<'_>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        Err(StorageError::Internal("not part of this test".into()))
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
        Err(StorageError::Internal("not part of this test".into()))
    }

    async fn apply_rollback(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _target: &RollbackTarget,
        _idempotency: &IdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        Err(StorageError::Internal("not part of this test".into()))
    }

    async fn read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        Err(StorageError::Internal("not part of this test".into()))
    }

    async fn lookup_idempotency(
        &self,
        scope: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        self.calls
            .lock()
            .expect("recording lock")
            .push(ReadCall::Lookup(scope.clone()));
        Ok(None)
    }

    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.calls
            .lock()
            .expect("recording lock")
            .push(ReadCall::ReclaimIdempotency { batch_size, before });
        Ok(3)
    }

    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.calls
            .lock()
            .expect("recording lock")
            .push(ReadCall::ReclaimOperationLog { batch_size, before });
        Ok(5)
    }
}

#[tokio::test]
async fn consumption_store_returns_the_bound_store() {
    let store = Arc::new(RecordingConsumptionStore::default());
    let plugin = plugin_over(store.clone()).await;

    assert!(std::ptr::eq(plugin.consumption_store(), store.as_ref()));
}

#[tokio::test]
async fn lookup_idempotency_preserves_scope_and_result() {
    let store = Arc::new(RecordingConsumptionStore::default());
    let plugin = plugin_over(store.clone()).await;
    let scope = idempotency_scope();

    assert!(
        plugin
            .lookup_idempotency(&scope)
            .await
            .expect("lookup")
            .is_none()
    );
    assert_eq!(store.calls(), vec![ReadCall::Lookup(scope)]);
}

#[tokio::test]
async fn reclaim_expired_idempotency_preserves_cutoff_and_result() {
    let store = Arc::new(RecordingConsumptionStore::default());
    let plugin = plugin_over(store.clone()).await;
    let before = OffsetDateTime::from_unix_timestamp(1_000).expect("valid timestamp");

    assert_eq!(
        plugin
            .reclaim_expired_idempotency(7, before)
            .await
            .expect("idempotency reclaim"),
        3
    );
    assert_eq!(
        store.calls(),
        vec![ReadCall::ReclaimIdempotency {
            batch_size: 7,
            before,
        }]
    );
}

#[tokio::test]
async fn reclaim_operation_log_preserves_cutoff_and_result() {
    let store = Arc::new(RecordingConsumptionStore::default());
    let plugin = plugin_over(store.clone()).await;
    let before = OffsetDateTime::from_unix_timestamp(1_000).expect("valid timestamp");

    assert_eq!(
        plugin
            .reclaim_operation_log(11, before)
            .await
            .expect("operation-log reclaim"),
        5
    );
    assert_eq!(
        store.calls(),
        vec![ReadCall::ReclaimOperationLog {
            batch_size: 11,
            before,
        }]
    );
}
