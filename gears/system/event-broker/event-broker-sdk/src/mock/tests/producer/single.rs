//! Mirrors scenarios/producer/single/. Tests migrated per mock-reference-alignment.

#[cfg(test)]
use super::super::helpers::*;

use super::super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBroker;
use crate::internal_test_helpers::{murmur3_32, partition_for};
use crate::mock::stubs::test_ctx_for_tenant;
use crate::producer::backend::IngestOutcome;
use uuid::Uuid;

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[tokio::test]
async fn s1_01_publish_single_async() {
    // Default publish path is async: durably enqueued → Accepted (202). No
    // offset/partition/sequence returned inline; they are server-stamped.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let outcome = broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Accepted);

    // Side effect: event lands on the tenant-derived partition and is stored.
    let p = partition_for(&c.subject_tenant_id().to_string(), 4);
    assert_eq!(h.stored(TOPIC, p).await.len(), 1);
}

/// Scenario: producer/single/1.02-positive-publish-sync-wait-persisted.md
#[tokio::test]
async fn s1_02_publish_sync_wait_persisted() {
    // Sync-wait publish holds until the backend persists, returning Persisted
    // (201) instead of the default Accepted (202).
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let outcome = broker
        .publish_sync(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Persisted);
    // Persisted means the event is durably in the log before the call returns.
    assert_eq!(h.stored(TOPIC, 0).await.len(), 1);
}

/// Scenario: producer/single/1.03-negative-schema-validation-failure.md
#[tokio::test]
async fn s1_03_schema_validation_failure() {
    // data is validated against the event type's data_schema at ingest; a payload
    // failing validation is rejected (400 / EventDataInvalid), no event admitted.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    h.register_event_type(
        TOPIC,
        EVT,
        serde_json::json!({
            "type": "object",
            "required": ["order_id", "total_cents"],
            "properties": {
                "order_id": { "type": "string" },
                "total_cents": { "type": "integer" }
            }
        }),
        &[],
    )
    .await;
    let c = ctx();
    let mut ev = wire_event(TOPIC, EVT, c.subject_tenant_id());
    // Missing the required `total_cents`.
    ev.data = Some(serde_json::json!({ "order_id": "order-bad" }));
    let err = broker.publish(&c, &ev).await.unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("EventDataInvalid") || msg.contains("validation") || msg.contains("invalid"),
        "bad payload must fail validation: {msg}"
    );
    // No event admitted.
    assert!(h.stored(TOPIC, 0).await.is_empty());
}

/// Scenario: producer/single/1.04-negative-rate-limited.md
#[tokio::test]
async fn s1_04_rate_limited() {
    // When the tenant publish quota is exhausted, the next publish is refused
    // (429 / RateLimited) and no event is admitted.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let handle = broker.handle();
    // Some(0) → zero allowance: the next publish (which charges 1 unit) is refused.
    handle.set_publish_rate_limit(Some(0)).await;
    let c = ctx();
    let err = broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("RateLimited") || msg.contains("rate limit"),
        "exhausted quota must yield RateLimited: {msg}"
    );
    // No event admitted.
    assert!(h.stored(TOPIC, 0).await.is_empty());
}

// --- ADR-0002 partition golden-vector contract (migrated from partitioning.rs) --
// These pin the partition resolution that scenario single/1.01 relies on
// ("partition is computed at ingest as murmur3_32(...) % N").

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[test]
fn s1_01_golden_single_partition_always_zero() {
    for key in &[
        "foo",
        "bar",
        "tenant-id",
        "00000000-0000-0000-0000-000000000001",
    ] {
        assert_eq!(partition_for(key, 1), 0, "any key mod 1 must be 0");
    }
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[test]
fn s1_01_golden_deterministic_repeated_calls() {
    let key = "repeat-test-subject";
    let first = partition_for(key, 32);
    for _ in 0..100 {
        assert_eq!(partition_for(key, 32), first);
    }
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[test]
fn s1_01_golden_within_bounds() {
    for n in &[1_u32, 2, 8, 16, 64] {
        for key in &[
            "foo",
            "bar",
            "some-key",
            "00000000-0000-0000-0000-000000000001",
        ] {
            let p = partition_for(key, *n);
            assert!(p < *n, "partition {p} must be < {n}");
        }
    }
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[test]
fn s1_01_golden_uuid_partition_16_pinned() {
    let key = "00000000-0000-0000-0000-000000000001";
    let pinned: u32 = (murmur3_32(key.as_bytes(), 0) & 0x7fff_ffff) % 16;
    assert_eq!(
        partition_for(key, 16),
        pinned,
        "partition_for drifted from pinned value - check ADR-0002 golden vectors"
    );
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[tokio::test]
async fn s1_01_tenant_default_routes_by_tenant() {
    // No partition_key set -> routes by tenant, not by subject, per ADR-0002.
    let (broker, h) = broker_with_topic(TOPIC, 8).await;
    let t1 = Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000001").unwrap();
    let c1 = test_ctx_for_tenant(t1);
    let mut ev = wire_event(TOPIC, EVT, c1.subject_tenant_id());
    ev.subject = "subject-that-must-not-drive-default-partition".to_owned();

    let tenant_p = partition_for(&t1.to_string(), 8);
    let subject_p = partition_for(&ev.subject, 8);
    assert_ne!(
        tenant_p, subject_p,
        "fixture must distinguish tenant default from subject fallback"
    );

    broker.publish(&c1, &ev).await.unwrap();

    let stored_at_tenant = h.stored(TOPIC, tenant_p).await;
    let stored_at_subject = h.stored(TOPIC, subject_p).await;
    assert_eq!(stored_at_tenant.len(), 1, "event lands on tenant partition");
    assert!(
        stored_at_subject.is_empty(),
        "event must not land on subject partition"
    );
    assert_eq!(stored_at_tenant[0].event.partition.unwrap(), tenant_p);
    assert_eq!(stored_at_tenant[0].event.partition_key, None);
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[tokio::test]
async fn s1_01_explicit_partition_key_overrides_tenant() {
    let (broker, h) = broker_with_topic(TOPIC2, 4).await;
    let c = ctx();
    let mut ev = wire_event(TOPIC2, EVT, c.subject_tenant_id());
    ev.partition_key = Some("explicit-key".to_owned());

    let explicit_p = partition_for("explicit-key", 4);
    let tenant_p = partition_for(&c.subject_tenant_id().to_string(), 4);
    assert_ne!(
        explicit_p, tenant_p,
        "fixture must distinguish explicit key from tenant default"
    );

    broker.publish(&c, &ev).await.unwrap();

    let stored = h.stored(TOPIC2, explicit_p).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].event.partition.unwrap(), explicit_p);
    assert_eq!(
        stored[0].event.partition_key.as_deref(),
        Some("explicit-key")
    );
    assert!(
        h.stored(TOPIC2, tenant_p).await.is_empty(),
        "explicit partition_key must override tenant default"
    );
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[tokio::test]
async fn s1_01_same_tenant_events_same_partition() {
    let (broker, h) = broker_with_topic(TOPIC3, 8).await;
    let c = ctx();
    let expected_p = partition_for(&c.subject_tenant_id().to_string(), 8);
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC3, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    assert_eq!(h.stored(TOPIC3, expected_p).await.len(), 5);
}

/// Scenario: producer/single/1.01-positive-publish-single-async.md
#[tokio::test]
async fn s1_01_offsets_are_monotonic_per_partition() {
    // Offsets are server-assigned; per partition they are monotonic from 0.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let stored = h.stored(TOPIC, 0).await;
    assert_eq!(stored.len(), 5);
    for (i, se) in stored.iter().enumerate() {
        // Offsets are 1-based (A6/A7): the i-th stored event has offset i+1.
        assert_eq!(se.event.offset.unwrap(), (i as i64) + 1);
    }
}
