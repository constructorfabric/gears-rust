//! Mirrors scenarios/producer/batch/. Tests migrated per mock-reference-alignment.

#[cfg(test)]
use super::super::helpers::*;

use super::super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::{EventBroker, IngestOutcome};

/// Scenario: producer/batch/1.01-positive-publish-batch.md
#[tokio::test]
async fn s1_01_publish_batch() {
    // A homogeneous batch (same topic, same resolved partition) is accepted
    // all-or-nothing; both events durably enqueued.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let evs: Vec<_> = (0..2)
        .map(|_| wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .collect();
    let outcomes = broker.publish_batch(&c, &evs).await.unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| *o == IngestOutcome::Accepted));
    assert_eq!(h.stored(TOPIC, 0).await.len(), 2);
}

/// Scenario: producer/batch/1.02-negative-mixed-partition-batch.md
#[tokio::test]
async fn s1_02_mixed_partition_batch() {
    // A batch is atomic per topic. The scenario rejects a batch that mixes
    // topics; the mock additionally enforces a single resolved partition per
    // batch. We assert BOTH: (a) mixed-topic batch is rejected, and (b)
    // mixed-partition batch is rejected. Neither admits any event.
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    h.register_topic(TOPIC2, 2).await;
    let c = ctx();

    // (a) Mixed topics → rejected.
    let err = broker
        .publish_batch(
            &c,
            &[
                wire_event(TOPIC, EVT, c.subject_tenant_id()),
                wire_event(TOPIC2, EVT, c.subject_tenant_id()),
            ],
        )
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("topic") || msg.contains("mixed"),
        "mixed-topic batch must be rejected: {msg}"
    );
    assert!(h.stored(TOPIC, 0).await.is_empty());
    assert!(h.stored(TOPIC, 1).await.is_empty());

    // (b) Mixed partitions within one topic → rejected (per-batch single partition).
    use crate::mock::partitioning::partition_for;
    let p0 = partition_for(&c.subject_tenant_id().to_string(), 2);
    let other = if p0 == 0 { "other-one" } else { "other-zero" };
    if partition_for(other, 2) != p0 {
        let mut ev2 = wire_event(TOPIC, EVT, c.subject_tenant_id());
        ev2.partition_key = Some(other.to_owned());
        let err = broker
            .publish_batch(&c, &[wire_event(TOPIC, EVT, c.subject_tenant_id()), ev2])
            .await
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("mixed") || msg.contains("partition"),
            "mixed-partition batch must be rejected: {msg}"
        );
    }
}

/// Scenario: producer/batch/1.03-negative-batch-too-large.md
#[tokio::test]
async fn s1_03_batch_too_large() {
    // A batch may carry at most 100 events; 101 is rejected with BatchTooLarge
    // (413). No event from the batch is admitted.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    // 101 events, all on the same partition (single-partition topic).
    let evs: Vec<_> = (0..101)
        .map(|_| wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .collect();
    let err = broker.publish_batch(&c, &evs).await.unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("BatchTooLarge") || msg.contains("too large") || msg.contains("exceeds"),
        "101-event batch must be BatchTooLarge: {msg}"
    );
    // All-or-nothing: nothing admitted.
    assert!(h.stored(TOPIC, 0).await.is_empty());
}

/// Scenario: producer/batch/1.04-negative-batch-late-validation-failure.md
#[tokio::test]
async fn s1_04_late_validation_failure_is_atomic() {
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
        &["test-type"],
    )
    .await;
    let c = ctx();
    let mut valid = wire_event(TOPIC, EVT, c.subject_tenant_id());
    valid.data = Some(serde_json::json!({
        "order_id": "order-atomic",
        "total_cents": 100
    }));
    let mut invalid = wire_event(TOPIC, EVT, c.subject_tenant_id());
    invalid.data = Some(serde_json::json!({
        "order_id": "order-atomic"
    }));

    let err = broker
        .publish_batch(&c, &[valid, invalid])
        .await
        .unwrap_err();

    let msg = format!("{err:?}");
    assert!(
        msg.contains("EventDataInvalid") || msg.contains("total_cents"),
        "late invalid event must reject the batch: {msg}"
    );
    assert!(h.stored(TOPIC, 0).await.is_empty());
}
