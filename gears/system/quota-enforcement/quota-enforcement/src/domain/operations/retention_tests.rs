use std::sync::Arc;

use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, OperationType, PayloadHash,
    QuotaEnforcementStoragePluginV1, StorageError, TenantId,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{RetentionSweeper, RetentionTiming, SweepReport};
use crate::domain::ports::metrics::RetentionTable;
use crate::test_support::RecordingMetrics;

fn timing(batch: u32) -> RetentionTiming {
    RetentionTiming {
        interval: std::time::Duration::from_secs(300),
        batch_size: std::num::NonZeroU32::new(batch).expect("nonzero"),
        operation_log_retention: time::Duration::days(30),
    }
}

fn far_future() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(32_503_680_000).expect("year 3000")
}

fn write(key: &str) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: TenantId::new(Uuid::from_u128(1)),
            subject_key: IdempotencySubjectKey::from_bytes([1; 32]),
            operation_type: OperationType::Credit,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([2; 32]),
    }
}

/// Storage holding `count` reclaimable records, written through the credit
/// primitive so they are real records rather than injected rows.
async fn storage_with(count: usize) -> Arc<InMemoryStorage> {
    let storage = Arc::new(InMemoryStorage::new());
    storage
        .bootstrap(&quota_enforcement_sdk::testing::bundle_with_global_policy())
        .await
        .expect("bootstrap");
    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(Uuid::from_u128(1))
        .build()
        .expect("context");
    let scope = toolkit_security::AccessScope::allow_all();
    let quota = storage
        .create_quota(
            &ctx,
            &scope,
            quota_enforcement_sdk::testing::quota_draft(
                quota_enforcement_sdk::testing::test_subject("u1"),
                Some(1_000),
            ),
            &[],
        )
        .await
        .expect("quota");
    for index in 0..count {
        storage
            .apply_credit(
                &ctx,
                &scope,
                quota,
                1,
                &quota_enforcement_sdk::PartialIdempotencyWrite {
                    tenant_id: TenantId::new(Uuid::from_u128(1)),
                    key: format!("c{index}"),
                    payload_hash: PayloadHash::from_bytes([u8::try_from(index).unwrap_or(0); 32]),
                },
                &[],
            )
            .await
            .expect("credit");
    }
    let _ = write("unused");
    storage
}

fn sweeper(
    storage: Arc<InMemoryStorage>,
    metrics: Arc<RecordingMetrics>,
    batch: u32,
) -> RetentionSweeper {
    RetentionSweeper::new(storage, metrics, timing(batch))
}

#[tokio::test]
async fn a_sweep_drains_a_table_until_a_batch_comes_back_short() {
    let storage = storage_with(5).await;
    let metrics = Arc::new(RecordingMetrics::default());
    let sweeper = sweeper(Arc::clone(&storage), Arc::clone(&metrics), 2);

    let report = sweeper
        .sweep_once(&CancellationToken::new(), far_future())
        .await;

    assert_eq!(
        report.idempotency, 5,
        "batches of two drained until one came back short"
    );
    let reclaimed: Vec<u64> = metrics
        .reclaimed()
        .into_iter()
        .filter(|(table, _)| *table == RetentionTable::Idempotency)
        .map(|(_, rows)| rows)
        .collect();
    assert_eq!(reclaimed, vec![2, 2, 1]);
}

#[tokio::test]
async fn a_failing_table_is_recorded_and_left_for_the_next_tick() {
    let storage = storage_with(2).await;
    storage.fail_with(StorageError::Unavailable("backend down".to_owned()));
    let metrics = Arc::new(RecordingMetrics::default());
    let sweeper = sweeper(Arc::clone(&storage), Arc::clone(&metrics), 2);

    let report = sweeper
        .sweep_once(&CancellationToken::new(), far_future())
        .await;

    assert_eq!(report, SweepReport::default(), "nothing was deleted");
    assert_eq!(
        metrics.retention_failures(),
        vec![RetentionTable::Idempotency, RetentionTable::OperationLog],
        "each table reports its own failure"
    );

    // The next tick retries and succeeds.
    storage.clear_failure();
    let retried = sweeper
        .sweep_once(&CancellationToken::new(), far_future())
        .await;
    assert_eq!(retried.idempotency, 2);
}

#[tokio::test]
async fn an_already_cancelled_sweep_deletes_nothing() {
    let storage = storage_with(4).await;
    let metrics = Arc::new(RecordingMetrics::default());
    let sweeper = sweeper(Arc::clone(&storage), Arc::clone(&metrics), 2);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = sweeper.sweep_once(&cancel, far_future()).await;

    assert_eq!(report, SweepReport::default());
    assert_eq!(metrics.reclaimed(), Vec::new());
}

#[tokio::test]
async fn unexpired_records_survive_a_sweep() {
    let storage = storage_with(3).await;
    let metrics = Arc::new(RecordingMetrics::default());
    let sweeper = sweeper(Arc::clone(&storage), Arc::clone(&metrics), 10);

    // `now` is well before any record's deadline.
    let report = sweeper
        .sweep_once(
            &CancellationToken::new(),
            OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
        )
        .await;

    assert_eq!(report.idempotency, 0, "nothing had expired yet");
}
