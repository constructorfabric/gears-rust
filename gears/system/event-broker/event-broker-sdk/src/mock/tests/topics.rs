//! Mirrors scenarios/topics/. Tests migrated per mock-reference-alignment.
use super::helpers::*;
#[cfg(test)]
use toolkit_gts::gts_id;

use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBrokerApi;
use crate::models::PartitionRange;

/// `description` of the base event's `data` member, which every event type's
/// composed payload contract carries as its first branch. Spelled out rather
/// than read back from the declaration so that changing the base surfaces as a
/// failure here.
const BASE_DATA_DESCRIPTION: &str = "Event payload, validated at ingest against the resolved schema of the event's type. The only field where UTF-8 (or any non-ASCII bytes) is permitted; all other event fields are ASCII per platform convention. May be absent for body-less events (e.g., notification-only events whose semantics are fully carried by `type` + `subject`).";

/// Scenario: topics/1.01-positive-list-topics.md
#[tokio::test]
async fn s1_01_positive_list_topics() {
    // GET /v1/topics → one DTO per registered topic. Each is projected from the
    // stored topic instance document, so the reported values are the topic's own
    // and an undeclared `retention` surfaces as absent.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    h.register_topic(TOPIC2, 2).await;
    let c = ctx();

    let mut topics = broker.list_topics(&c).await.unwrap();
    // Registration order is not listing order - the mock holds topics in a map.
    topics.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));

    assert_eq!(
        serde_json::to_value(&topics).unwrap(),
        serde_json::json!([
            {
                "id": TOPIC,
                "description": format!("Mock topic {TOPIC}"),
                "retention": null,
            },
            {
                "id": TOPIC2,
                "description": format!("Mock topic {TOPIC2}"),
                "retention": null,
            },
        ])
    );
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
            .publish(&c, &wire_event(EVT, c.subject_tenant_id()))
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
    // GET /v1/event-types → one DTO per registered type, projected from its
    // derived type schema. The topic each one is anchored to is a resolved
    // `topic` trait, and `data_schema` is the payload contract composed out of
    // the base event's `data` member and the type's narrowing of it - so the
    // first branch below is proof the chain resolved through the base.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    h.register_event_type(TOPIC, EVT, serde_json::json!({ "type": "object" }), &[])
        .await;
    h.register_event_type(
        TOPIC,
        EVT2,
        serde_json::json!({ "type": "object", "required": ["kind"] }),
        &["test-type"],
    )
    .await;

    let mut types = broker.list_event_types(&c).await.unwrap();
    // Registration order is not listing order - the mock holds types in a map.
    types.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));

    assert_eq!(
        serde_json::to_value(&types).unwrap(),
        serde_json::json!([
            {
                "id": EVT,
                "partition_key": "/tenant_id",
                "topic": TOPIC,
                "description": null,
                "allowed_subject_types": [],
                "data_schema": {
                    "allOf": [
                        {
                            "additionalProperties": true,
                            "default": null,
                            "description": BASE_DATA_DESCRIPTION,
                            "type": ["object", "null"],
                        },
                        { "type": "object" },
                    ],
                },
            },
            {
                "id": EVT2,
                "partition_key": "/tenant_id",
                "topic": TOPIC,
                "description": null,
                "allowed_subject_types": ["test-type"],
                "data_schema": {
                    "allOf": [
                        {
                            "additionalProperties": true,
                            "default": null,
                            "description": BASE_DATA_DESCRIPTION,
                            "type": ["object", "null"],
                        },
                        { "type": "object", "required": ["kind"] },
                    ],
                },
            },
        ])
    );

    // get_event_type serves the same DTO for a known id, and rejects an unknown
    // one.
    let one = broker.get_event_type(&c, EVT).await.unwrap();
    assert_eq!(
        serde_json::to_value(&one).unwrap(),
        serde_json::to_value(&types[0]).unwrap()
    );

    let ghost = gts_id!("cf.core.events.event.v1~example.mock.broker.ghost.v1~");
    let err = broker.get_event_type(&c, ghost).await.unwrap_err();
    match err {
        crate::error::EventBrokerError::EventTypeUnknown { ref type_id, .. } => {
            assert_eq!(type_id, ghost, "unknown event type id is echoed back");
        }
        other => panic!("expected EventTypeUnknown, got {other:?}"),
    }
}
