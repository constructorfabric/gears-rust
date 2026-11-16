use crate::sequence::Sequence;
use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::consumer::Delivery;
use crate::gts::{EventV1, StoredPosition};
use crate::ids::ConsumerGroupId;

use super::{DeadLetterEnvelope, DeadLetterRecord};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.orders.rejected.x.v1~");

const EVENT_ID: Uuid = Uuid::from_u128(0x0e1e_0001);

fn raw_event() -> EventV1 {
    EventV1::builder_for(
        gts::GtsTypeId::new(EVENT_TYPE),
        serde_json::json!({ "order_id": "order-1", "valid": false }),
    )
    .id(EVENT_ID)
    .subject_tenant(Uuid::from_u128(0x0e1e_0002))
    .subject("order-1")
    .subject_type_of(gts::GtsTypeId::new("gts.x.eb.test.subject.v1~"))
    .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
    .build()
    .unwrap()
    .stamp_position(StoredPosition {
        partition: 7,
        sequence: Sequence::assigned(99),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    })
}

fn delivery() -> Delivery {
    Delivery::new(gts::GtsInstanceId::try_new(TOPIC).unwrap(), 7, 1)
}

#[test]
fn dead_letter_envelope_preserves_record_context_and_payload_type_convention() {
    let consumer_group = ConsumerGroupId::new(Uuid::from_u128(0x0e1e_0003));
    let raw = raw_event();
    let record = DeadLetterRecord::builder(&raw, &delivery(), "permanent validation failure")
        .consumer_group(consumer_group)
        .attempts(6)
        .build();

    let envelope = DeadLetterEnvelope::from_record(record);

    assert_eq!(DeadLetterEnvelope::VERSION, 1);
    assert_eq!(
        DeadLetterEnvelope::PAYLOAD_TYPE,
        "application/vnd.constructorfabric.event-broker.dlq+json"
    );
    assert_eq!(envelope.version, DeadLetterEnvelope::VERSION);
    assert_eq!(envelope.consumer_group, Some(consumer_group));
    assert_eq!(envelope.topic, TOPIC);
    assert_eq!(envelope.event_type, EVENT_TYPE);
    assert_eq!(envelope.subject, "order-1");
    assert_eq!(envelope.subject_type, "gts.x.eb.test.subject.v1~");
    assert_eq!(envelope.partition, 7);
    assert_eq!(envelope.offset, Sequence::assigned(99));
    assert_eq!(envelope.attempts, Some(6));
    assert_eq!(envelope.reason, "permanent validation failure");
    assert_eq!(
        envelope.payload,
        serde_json::json!({ "order_id": "order-1", "valid": false })
    );
    assert_eq!(envelope.event, EVENT_ID);
}

#[test]
fn dead_letter_envelope_round_trips_from_outbox_payload() {
    let raw = raw_event();
    let record = DeadLetterRecord::builder(&raw, &delivery(), "cannot project").build();
    let envelope = DeadLetterEnvelope::from_record(record);

    let payload = envelope.to_vec().expect("serialize envelope");
    let decoded = DeadLetterEnvelope::from_slice(&payload).expect("deserialize envelope");

    assert_eq!(decoded, envelope);
    let coordinates = decoded.source_coordinates();
    assert_eq!(coordinates.topic, TOPIC);
    assert_eq!(coordinates.event_type, EVENT_TYPE);
    assert_eq!(coordinates.subject, "order-1");
    assert_eq!(coordinates.subject_type, "gts.x.eb.test.subject.v1~");
    assert_eq!(coordinates.partition, 7);
    assert_eq!(coordinates.offset, Sequence::assigned(99));
    assert_eq!(coordinates.event, EVENT_ID);
}

#[test]
fn invalid_dead_letter_envelope_payload_returns_consumer_error() {
    let err = DeadLetterEnvelope::from_slice(b"not json")
        .expect_err("invalid payload should not deserialize");

    assert!(err.to_string().contains("deserialize dead-letter envelope"));
}

/// The envelope is the payload a consumer DLQ stores, so its JSON keys are a durable
/// format: an envelope stored earlier is decoded by these keys.
#[test]
fn a_dead_letter_envelope_is_stored_with_these_keys() {
    let raw = raw_event();
    let record = DeadLetterRecord::builder(&raw, &delivery(), "permanent validation failure")
        .consumer_group(ConsumerGroupId::new(Uuid::from_u128(0x0e1e_0003)))
        .occurred_at("2026-01-02T03:05:00Z".parse().unwrap())
        .build();
    let mut envelope = DeadLetterEnvelope::from_record(record);
    envelope.parked_at = "2026-01-02T03:06:00Z".parse().unwrap();

    assert_eq!(
        serde_json::to_value(&envelope).unwrap(),
        serde_json::json!({
            "version": 1,
            "consumer_group": "00000000-0000-0000-0000-00000e1e0003",
            "topic": TOPIC,
            "event_type": EVENT_TYPE,
            "subject": "order-1",
            "subject_type": "gts.x.eb.test.subject.v1~",
            "partition": 7,
            "offset": 99,
            "attempts": 1,
            "reason": "permanent validation failure",
            "payload": { "order_id": "order-1", "valid": false },
            "occurred_at": "2026-01-02T03:05:00Z",
            "parked_at": "2026-01-02T03:06:00Z",
            "event": "00000000-0000-0000-0000-00000e1e0001",
        })
    );
}
