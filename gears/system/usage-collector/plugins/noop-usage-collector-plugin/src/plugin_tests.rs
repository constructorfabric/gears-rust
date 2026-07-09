//! Tests for the no-op storage backend's [`UsageCollectorPluginV1`] surface.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use usage_collector_sdk::{
    IdempotencyKey, MetadataKey, ResourceRef, UsageCollectorPluginError, UsageCollectorPluginV1,
    UsageKind, UsageRecord, UsageRecordStatus, UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

use super::NoopBackend;

fn sample_id(suffix: &str) -> UsageTypeGtsId {
    UsageTypeGtsId::new(format!(
        "{prefix}{suffix}",
        prefix = UsageTypeGtsId::USAGE_RECORD_BASE,
    ))
    .expect("usage_record-derived gts_id must parse")
}

fn keyset<const N: usize>(values: [&str; N]) -> BTreeSet<MetadataKey> {
    values
        .into_iter()
        .map(|v| MetadataKey::new(v).expect("valid metadata key fixture"))
        .collect()
}

fn sample_record(uuid: &str, idempotency_key: &str) -> UsageRecord {
    UsageRecord {
        uuid: Uuid::parse_str(uuid).expect("valid record uuid fixture"),
        gts_id: sample_id("test.uc.batch.order.v1"),
        tenant_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222")
            .expect("valid tenant uuid fixture"),
        resource_ref: ResourceRef::new("vm-1", "compute.vm").expect("valid resource ref fixture"),
        subject_ref: None,
        metadata: BTreeMap::new(),
        value: Decimal::from(1),
        idempotency_key: IdempotencyKey::new(idempotency_key)
            .expect("valid idempotency key fixture"),
        corrects_id: None,
        status: UsageRecordStatus::Active,
        created_at: time::OffsetDateTime::from_unix_timestamp(0).expect("epoch fixture"),
    }
}

#[tokio::test]
async fn create_usage_type_echoes_input() {
    let backend = NoopBackend::new();
    let gts_id = sample_id("test.uc.phase01.echo.v1");
    let metadata_fields = keyset(["region", "az"]);
    let input = UsageType {
        gts_id: gts_id.clone(),
        kind: UsageKind::Counter,
        metadata_fields: metadata_fields.clone(),
    };

    let record = backend
        .create_usage_type(input.clone())
        .await
        .expect("noop create_usage_type must echo input");

    assert_eq!(record.gts_id, gts_id);
    assert_eq!(record.kind, input.kind);
    assert_eq!(record.metadata_fields, metadata_fields);
}

#[tokio::test]
async fn get_usage_type_returns_not_found_with_target_id() {
    let backend = NoopBackend::new();
    let gts_id = sample_id("test.uc.phase01.getmiss.v1");

    let err = backend
        .get_usage_type(gts_id.clone())
        .await
        .expect_err("noop backend MUST surface UsageTypeNotFound");

    match err {
        UsageCollectorPluginError::UsageTypeNotFound { gts_id: returned } => {
            assert_eq!(
                returned, gts_id,
                "the not-found variant MUST echo the supplied gts_id",
            );
        }
        other => panic!("expected UsageTypeNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_usage_type_returns_not_found_with_target_id() {
    let backend = NoopBackend::new();
    let gts_id = sample_id("test.uc.phase01.delmiss.v1");

    let err = backend
        .delete_usage_type(gts_id.clone())
        .await
        .expect_err("noop backend MUST surface UsageTypeNotFound");

    match err {
        UsageCollectorPluginError::UsageTypeNotFound { gts_id: returned } => {
            assert_eq!(
                returned, gts_id,
                "the not-found variant MUST echo the supplied gts_id",
            );
        }
        other => panic!("expected UsageTypeNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn create_usage_records_preserves_input_order() {
    let backend = NoopBackend::new();
    let inputs = vec![
        sample_record("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "k-1"),
        sample_record("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "k-2"),
        sample_record("cccccccc-cccc-cccc-cccc-cccccccccccc", "k-3"),
    ];

    let outputs = backend
        .create_usage_records(inputs.clone())
        .await
        .expect("noop create_usage_records must succeed on echo");

    assert_eq!(
        outputs.len(),
        inputs.len(),
        "result vec length MUST match input vec length",
    );
    for (i, (input, output)) in inputs.iter().zip(outputs.into_iter()).enumerate() {
        let echoed = output.unwrap_or_else(|e| {
            panic!("noop must Ok-echo every input record, got error at index {i}: {e:?}")
        });
        assert_eq!(
            &echoed, input,
            "echoed record at index {i} MUST equal the input at the same index",
        );
    }
}

#[tokio::test]
async fn create_usage_records_rejects_empty_batch_as_internal() {
    let backend = NoopBackend::new();

    let err = backend
        .create_usage_records(Vec::new())
        .await
        .expect_err("an empty batch is a host-contract breach, not a success");

    assert!(
        matches!(err, UsageCollectorPluginError::Internal(_)),
        "empty batch MUST surface as Internal (non-retryable host-contract breach), got {err:?}",
    );
}

#[tokio::test]
async fn deactivate_usage_record_returns_not_found_with_target_id() {
    let backend = NoopBackend::new();
    let id = uuid::Uuid::from_u128(0x1234_5678_9ABC_DEF0);

    let err = backend
        .deactivate_usage_record(id)
        .await
        .expect_err("noop backend MUST surface UsageRecordNotFound");

    match err {
        UsageCollectorPluginError::UsageRecordNotFound { id: returned } => {
            assert_eq!(
                returned, id,
                "the not-found variant MUST echo the supplied target id",
            );
        }
        other => panic!("expected UsageRecordNotFound, got {other:?}"),
    }
}
