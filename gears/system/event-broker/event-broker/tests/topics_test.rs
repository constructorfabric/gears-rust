//! Topic introspection (scenarios topics/1.01-1.04, features 0003 and 0007
//! §2.6): the topic list, a partition's segment manifest, the event-type
//! list. In-process: real services and router, `SQLite` gear database, memory
//! backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use chrono::DateTime;
use event_broker::test_support::{EventBrokerHarness, Json, StaticTypesRegistry};
use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use event_broker_sdk::{Event, Sequence};
use gts::GtsInstanceId;
use serde_json::{Value, json};

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.topics.stream.v1";
const OTHER_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.topics.longpoll.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.topics.happy.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.topics.subject.v1~";

async fn harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            { "id": OTHER_TOPIC, "partitions": 1 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "allowed_subject_types": [SUBJECT_TYPE],
            },
        ])))
        .build()
        .await
}

// scenario: topics/1.01-positive-list-topics.md
#[tokio::test]
async fn list_topics_returns_a_paged_list_of_every_registered_topic() {
    let harness = harness().await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({
            "items": [
                {
                    "id": OTHER_TOPIC,
                    "description": "a topic this test publishes to",
                    "retention": null,
                },
                {
                    "id": TOPIC,
                    "description": "a topic this test publishes to",
                    "retention": null,
                },
            ],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

// scenario: topics/1.02-positive-list-topic-segments.md
#[tokio::test]
async fn segments_of_an_empty_partition_report_an_empty_manifest() {
    let harness = harness().await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({
            "topic": TOPIC,
            "partition": 0,
            "start_sequence": 0,
            "end_sequence": 0,
            "start_time": null,
            "end_time": null,
            "segments": [{ "start_sequence": 0, "end_sequence": 0, "event_count": 0 }],
        })
    );
}

// scenario: topics/1.03-negative-segments-unknown-topic.md
#[tokio::test]
async fn segments_of_an_unknown_topic_answer_404() {
    let harness = harness().await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query(
            "topic",
            "gts.cf.core.events.topic.v1~x.eb.topics.nonexistent.v1",
        )
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "topic 'gts.cf.core.events.topic.v1~x.eb.topics.nonexistent.v1' is not registered",
            "instance": "/event-broker/v1/topics/segments",
            "context": {
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.topics.nonexistent.v1",
            },
        })
    );
}

// scenario: topics/1.04-positive-list-event-types.md
#[tokio::test]
async fn list_event_types_of_a_topic_returns_a_paged_list() {
    let harness = harness().await;

    let resp = harness
        .api_v1()
        .get_event_types()
        .with_query("$filter", format!("topic%20eq%20'{TOPIC}'"))
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({
            "items": [{
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "description": null,
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/tenant_id",
                "data_schema": {
                    "allOf": [
                        { "type": ["object", "null"] },
                        { "type": ["object", "null"] },
                    ],
                },
            }],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

/// Feature 0003 / 0007 §2.6: the manifest reports the stored range, from the
/// first stored event's sequence and storage time to the last's. The
/// `segments` entries are the backend's own shape.
#[tokio::test]
async fn segments_report_the_stored_range() {
    let harness = harness().await;
    for (id, subject) in [
        ("0190f1a2-0000-7000-8000-000000000101", "s1"),
        ("0190f1a2-0000-7000-8000-000000000102", "s2"),
        ("0190f1a2-0000-7000-8000-000000000103", "s3"),
    ] {
        let resp = harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": id,
                "type": EVENT_TYPE,
                "tenant_id": "0190f1a2-0000-7000-8000-0000000000a1",
                "source": "topics-test",
                "subject": subject,
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-10-01T12:00:00Z",
            })))
            .send()
            .await;
        resp.assert_status(202);
        assert_eq!(resp.text(), "");
    }
    let stored = stored_events(&harness, 3).await;
    let first_stored_at = stored[0]
        .sequence_time
        .expect("a stored event has a storage time");
    let last_stored_at = stored[2]
        .sequence_time
        .expect("a stored event has a storage time");

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(200);
    let body = resp.json();
    let start_time = body["start_time"].clone();
    let end_time = body["end_time"].clone();
    assert_eq!(
        body,
        json!({
            "topic": TOPIC,
            "partition": 0,
            "start_sequence": 1,
            "end_sequence": 3,
            "start_time": start_time,
            "end_time": end_time,
            "segments": [{ "start_sequence": 1, "end_sequence": 3, "event_count": 3 }],
        })
    );
    assert_eq!(instant(&start_time), first_stored_at);
    assert_eq!(instant(&end_time), last_stored_at);
}

/// Feature 0003: a partition at or above the topic's count is
/// `InvalidPartition`.
#[tokio::test]
async fn segments_of_a_partition_past_the_count_answer_400() {
    let harness = harness().await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC)
        .with_query("partition", "1")
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidPartition: partition must be in [0, 1) for this topic",
            "instance": "/event-broker/v1/topics/segments",
            "context": {
                "format": "InvalidPartition: partition must be in [0, 1) for this topic",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// What the backend holds on `TOPIC` partition 0, once `count` events have
/// drained from the ingest outbox into it.
async fn stored_events(harness: &EventBrokerHarness, count: usize) -> Vec<Event> {
    let at = TopicPartition::new(GtsInstanceId::try_new(TOPIC).unwrap(), Partition::new(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = harness
            .backend()
            .read(&at, Sequence::NONE, 1024, Wait::No)
            .await
            .expect("read the stored events");
        if events.len() >= count || tokio::time::Instant::now() >= deadline {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn instant(value: &Value) -> DateTime<chrono::Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("an RFC 3339 time"))
        .expect("an RFC 3339 time")
        .to_utc()
}
