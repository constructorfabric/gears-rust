use crate::sequence::Sequence;
use chrono::{DateTime, Utc};
use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::consumer::Delivery;
use crate::gts::{EventV1, StoredPosition};
use crate::ids::{ConsumerGroupId, TopicId};

use super::DeadLetterRecord;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.orders.rejected.x.v1~");
const SUBJECT_TYPE: &str = "gts.x.eb.test.subject.v1~";
const EVENT_ID: Uuid = Uuid::from_u128(0x0d1e_0001);
const GROUP_ID: Uuid = Uuid::from_u128(0x0d1e_0002);

fn occurred_at() -> DateTime<Utc> {
    "2026-01-02T03:04:05Z".parse().unwrap()
}

fn dead_lettered_at() -> DateTime<Utc> {
    "2026-01-02T03:05:00Z".parse().unwrap()
}

fn unstamped_event() -> EventV1 {
    EventV1::builder_for(
        gts::GtsTypeId::new(EVENT_TYPE),
        serde_json::json!({ "order_id": "order-1" }),
    )
    .id(EVENT_ID)
    .tenant_id(Uuid::nil())
    .subject("order-1")
    .subject_type_id(gts::GtsTypeId::new(SUBJECT_TYPE))
    .trace_parent("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
    .occurred_at(occurred_at())
    .build()
    .unwrap()
}

/// An event as a consumer is handed it: stored at partition 3, sequence 42.
fn raw_event() -> EventV1 {
    unstamped_event().stamp_position(StoredPosition {
        partition: 3,
        sequence: Sequence::assigned(42),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    })
}

fn delivery() -> Delivery {
    Delivery::new(gts::GtsInstanceId::try_new(TOPIC).unwrap(), 3, 2)
}

#[test]
fn dead_letter_record_exposes_context_fields_for_diagnosis_and_replay() {
    let group_id = ConsumerGroupId::new(GROUP_ID);
    let topic_id = TopicId::from_gts(TOPIC);
    let raw = raw_event();

    let record = DeadLetterRecord::builder(&raw, &delivery(), "schema mismatch")
        .group_id(group_id)
        .topic_id(topic_id)
        .attempts(2)
        .build();

    assert_eq!(record.group_id, Some(group_id));
    assert_eq!(record.topic_id, Some(topic_id));
    assert_eq!(record.topic, TOPIC);
    assert_eq!(record.event_type, EVENT_TYPE);
    assert_eq!(record.subject, "order-1");
    assert_eq!(record.subject_type, SUBJECT_TYPE);
    assert_eq!(record.partition, 3);
    assert_eq!(record.offset, Sequence::assigned(42));
    assert_eq!(record.payload, serde_json::json!({ "order_id": "order-1" }));
    assert_eq!(record.reason, "schema mismatch");
    assert_eq!(record.attempts, Some(2));
    assert_eq!(record.event_id, EVENT_ID);
}

#[test]
fn dead_letter_record_allows_absent_group_and_topic_id() {
    let raw = raw_event();

    let record = DeadLetterRecord::builder(&raw, &delivery(), "no registry context")
        .without_topic_id()
        .build();

    assert_eq!(record.group_id, None);
    assert_eq!(record.topic_id, None);
    assert_eq!(record.topic, TOPIC);
    assert_eq!(record.reason, "no registry context");
}

/// The event is stored at partition 3 and the delivery says partition 5, so the record's
/// partition shows which side it came from.
#[test]
fn a_dead_letter_record_takes_topic_partition_and_attempts_from_the_delivery() {
    let raw = raw_event();
    let delivery = Delivery::new(gts::GtsInstanceId::try_new(TOPIC).unwrap(), 5, 4);

    let record = DeadLetterRecord::builder(&raw, &delivery, "permanent failure")
        .occurred_at(dead_lettered_at())
        .build();

    assert_eq!(record.group_id, None);
    assert_eq!(record.topic_id, Some(TopicId::from_gts(TOPIC)));
    assert_eq!(record.topic, TOPIC);
    assert_eq!(record.event_type, EVENT_TYPE);
    assert_eq!(record.subject, "order-1");
    assert_eq!(record.subject_type, SUBJECT_TYPE);
    assert_eq!(record.partition, 5);
    assert_eq!(record.offset, Sequence::assigned(42));
    assert_eq!(record.attempts, Some(4));
    assert_eq!(record.reason, "permanent failure");
    assert_eq!(record.payload, serde_json::json!({ "order_id": "order-1" }));
    assert_eq!(record.occurred_at, dead_lettered_at());
    assert_eq!(record.event_id, EVENT_ID);
}

#[test]
fn a_body_less_event_dead_letters_with_a_null_payload() {
    let raw = EventV1::builder_without_data(gts::GtsTypeId::new(EVENT_TYPE))
        .id(EVENT_ID)
        .tenant_id(Uuid::nil())
        .subject("order-1")
        .subject_type_id(gts::GtsTypeId::new(SUBJECT_TYPE))
        .occurred_at(occurred_at())
        .build()
        .unwrap()
        .stamp_position(StoredPosition {
            partition: 3,
            sequence: Sequence::assigned(42),
            sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
        });

    let record = DeadLetterRecord::from_event(&raw, &delivery(), "permanent failure");

    assert_eq!(record.payload, serde_json::Value::Null);
    assert_eq!(record.offset, Sequence::assigned(42));
    assert_eq!(record.event_id, EVENT_ID);
}

#[test]
fn an_unstamped_event_dead_letters_at_no_position() {
    let raw = unstamped_event();

    let record = DeadLetterRecord::builder(&raw, &delivery(), "permanent failure")
        .occurred_at(dead_lettered_at())
        .build();

    assert_eq!(record.offset, Sequence::NONE);
    assert_eq!(record.partition, 3);
    assert_eq!(record.attempts, Some(2));
    assert_eq!(record.payload, serde_json::json!({ "order_id": "order-1" }));
    assert_eq!(record.event_id, EVENT_ID);
}

/// The payload may carry personal or secret data, so the builder's `Debug` leaves it out.
#[test]
fn the_builder_debug_output_leaves_the_payload_out() {
    let planted = "planted-secret-payload-value";
    let raw = EventV1::builder_for(
        gts::GtsTypeId::new(EVENT_TYPE),
        serde_json::json!({ "order_id": "order-1", "card": planted }),
    )
    .id(EVENT_ID)
    .tenant_id(Uuid::nil())
    .subject("order-1")
    .subject_type_id(gts::GtsTypeId::new(SUBJECT_TYPE))
    .occurred_at(occurred_at())
    .build()
    .unwrap()
    .stamp_position(StoredPosition {
        partition: 3,
        sequence: Sequence::assigned(42),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    });
    assert_eq!(
        raw.data(),
        Some(&serde_json::json!({ "order_id": "order-1", "card": planted }))
    );

    let builder = DeadLetterRecord::builder(&raw, &delivery(), "schema mismatch")
        .group_id(ConsumerGroupId::new(GROUP_ID))
        .occurred_at(dead_lettered_at());

    let debug = format!("{builder:?}");
    assert_eq!(
        debug,
        concat!(
            r#"DeadLetterRecordBuilder { reason: "schema mismatch", "#,
            "group_id: Some(ConsumerGroupId(00000000-0000-0000-0000-00000d1e0002)), ",
            "topic_id: Some(TopicId(88505383-6439-57ce-8683-8b20022a2177)), ",
            r#"topic: GtsInstanceId(GtsEntityId("gts.cf.core.events.topic.v1~example.orders.x.x.v1")), "#,
            r#"event_type: GtsTypeId(GtsEntityId("gts.cf.core.events.event.v1~example.orders.rejected.x.v1~")), "#,
            r#"subject: "order-1", "#,
            r#"subject_type: GtsTypeId(GtsEntityId("gts.x.eb.test.subject.v1~")), "#,
            "partition: 3, offset: Sequence(42), attempts: Some(2), ",
            "occurred_at: 2026-01-02T03:05:00Z, ",
            "event_id: 00000000-0000-0000-0000-00000d1e0001, .. }",
        )
    );
    assert!(!debug.contains(planted));
}
