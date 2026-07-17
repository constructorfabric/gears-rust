//! Mirrors scenarios/topics/. Tests migrated per mock-reference-alignment.
use super::helpers::*;
#[cfg(test)]
use toolkit_gts::gts_id;

use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBroker;
use crate::models::PartitionRange;

/// Scenario: topics/1.01-positive-list-topics.md
#[tokio::test]
async fn s1_01_positive_list_topics() {
    // GET /v1/topics → array of topic records (id + partitions).
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    h.register_topic(TOPIC2, 2).await;
    let c = ctx();

    let topics = broker.list_topics(&c).await.unwrap();
    assert_eq!(topics.len(), 2, "both registered topics are visible");

    let audit = topics
        .iter()
        .find(|t| t.id == TOPIC)
        .expect("audit topic must be listed");
    assert_eq!(
        audit.partitions, 4,
        "partition count echoed per topic record"
    );

    let notify = topics
        .iter()
        .find(|t| t.id == TOPIC2)
        .expect("notify topic must be listed");
    assert_eq!(notify.partitions, 2);
}

/// Scenario: topics/1.02-positive-list-topic-segments.md
#[tokio::test]
async fn s1_02_positive_list_topic_segments() {
    // GET /v1/topics/segments?topic=&partition=0 → manifest spanning stored sequences.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    // Publish a few events so partition 0 has a non-empty log.
    for _ in 0..3 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }

    let range = PartitionRange {
        start_offset: None,
        end_offset: None,
        limit: 100,
    };
    let segments = broker
        .list_topic_segments(&c, TOPIC, 0, range)
        .await
        .unwrap();

    assert_eq!(
        segments.len(),
        1,
        "non-empty partition yields a segment manifest"
    );
    let seg = &segments[0];
    assert_eq!(seg.topic, TOPIC, "manifest echoes the requested topic");
    assert_eq!(seg.partition, 0, "manifest echoes the requested partition");
    assert!(
        seg.start_sequence <= seg.end_sequence,
        "manifest sequence span must be well-ordered (start={} end={})",
        seg.start_sequence,
        seg.end_sequence
    );
}

/// Scenario: topics/1.03-negative-segments-unknown-topic.md
#[tokio::test]
async fn s1_03_negative_segments_unknown_topic() {
    // GET segments for an unregistered topic → 404 not_found (SDK: TopicNotFound).
    let broker = crate::mock::MockBroker::new();
    let c = ctx();
    let unknown = gts_id!("cf.core.events.topic.v1~acme.nonexistent.x.x.v1");

    let range = PartitionRange {
        start_offset: None,
        end_offset: None,
        limit: 100,
    };
    let err = broker
        .list_topic_segments(&c, unknown, 0, range)
        .await
        .unwrap_err();

    match err {
        crate::error::EventBrokerError::TopicNotFound { ref topic, .. } => {
            assert_eq!(topic, unknown, "error must name the missing topic");
        }
        other => panic!("expected TopicNotFound, got {other:?}"),
    }
}

/// Scenario: topics/1.04-positive-list-event-types.md
#[tokio::test]
async fn s1_04_positive_list_event_types() {
    // GET /v1/event_types → registered types, each anchored to its parent topic.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    let schema = serde_json::json!({ "type": "object" });
    h.register_event_type(TOPIC, EVT, schema.clone(), &[]).await;
    h.register_event_type(TOPIC, EVT2, schema, &[]).await;

    let types = broker.list_event_types(&c).await.unwrap();
    assert_eq!(types.len(), 2, "both registered event types are listed");
    assert!(
        types.iter().all(|et| et.topic == TOPIC),
        "each event type is anchored to its parent topic"
    );
    assert!(types.iter().any(|et| et.id == EVT));
    assert!(types.iter().any(|et| et.id == EVT2));

    // get_event_type round-trips a known id and rejects an unknown one.
    let one = broker.get_event_type(&c, EVT).await.unwrap();
    assert_eq!(one.id, EVT);
    assert_eq!(one.topic, TOPIC);

    let ghost = gts_id!("cf.core.events.event_type.v1~example.mock.broker.ghost.v1");
    let err = broker.get_event_type(&c, ghost).await.unwrap_err();
    match err {
        crate::error::EventBrokerError::EventTypeUnknown { ref type_id, .. } => {
            assert_eq!(type_id, ghost, "unknown event type id is echoed back");
        }
        other => panic!("expected EventTypeUnknown, got {other:?}"),
    }
}
