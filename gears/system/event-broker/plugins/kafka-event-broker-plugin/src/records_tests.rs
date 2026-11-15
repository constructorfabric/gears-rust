use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::{FenceKey, LogId};
use uuid::Uuid;

use crate::records::{self, LogRecord};

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1").expect("valid id")
}

#[test]
fn a_log_record_round_trips() {
    let record = LogRecord::new(
        &topic(),
        Uuid::from_u128(0x5ee1),
        LogId::from_uuid(Uuid::from_u128(7)),
        4,
    );
    let encoded = record.encode();
    assert_eq!(
        String::from_utf8(encoded.clone()).unwrap_or_default(),
        r#"{"v":1,"topic":"gts.cf.core.events.topic.v1~acme.orders.x.topic.v1","uuid":"00000000-0000-0000-0000-000000005ee1","log_id":"00000000-0000-0000-0000-000000000007","partitions":4}"#
    );
    assert_eq!(LogRecord::decode(&encoded), Some(record));
}

#[test]
fn a_log_record_carries_its_topics_reference() {
    let encoded = LogRecord::new(
        &topic(),
        Uuid::from_u128(0x5ee1),
        LogId::from_uuid(Uuid::from_u128(0x10)),
        3,
    )
    .encode();

    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&encoded).ok(),
        Some(serde_json::json!({
            "v": 1,
            "topic": "gts.cf.core.events.topic.v1~acme.orders.x.topic.v1",
            "uuid": "00000000-0000-0000-0000-000000005ee1",
            "log_id": "00000000-0000-0000-0000-000000000010",
            "partitions": 3,
        }))
    );
    assert_eq!(
        LogRecord::decode(&encoded),
        Some(LogRecord {
            v: 1,
            topic: "gts.cf.core.events.topic.v1~acme.orders.x.topic.v1".to_owned(),
            uuid: Uuid::from_u128(0x5ee1),
            log_id: Uuid::from_u128(0x10),
            partitions: 3,
        })
    );
}

#[test]
fn a_log_record_without_a_reference_does_not_decode() {
    assert_eq!(
        LogRecord::decode(
            br#"{"v":1,"topic":"gts.cf.core.events.topic.v1~acme.orders.x.topic.v1","log_id":"00000000-0000-0000-0000-000000000007","partitions":4}"#
        ),
        None
    );
}

#[test]
fn a_log_record_of_another_version_or_shape_is_not_read() {
    assert_eq!(
        [
            LogRecord::decode(br#"{"v":2,"topic":"t","uuid":"00000000-0000-0000-0000-000000005ee1","log_id":"00000000-0000-0000-0000-000000000007","partitions":4}"#),
            LogRecord::decode(br#"{"v":1,"topic":"t","uuid":"00000000-0000-0000-0000-000000005ee1","log_id":"00000000-0000-0000-0000-000000000007","partitions":0}"#),
            LogRecord::decode(b"not json"),
        ],
        [None, None, None]
    );
}

#[test]
fn a_fence_record_round_trips() {
    assert_eq!(records::encode_fence(258), [1, 0, 0, 0, 0, 0, 0, 1, 2]);
    assert_eq!(records::decode_fence(&records::encode_fence(-3)), Some(-3));
    assert_eq!(
        [
            records::decode_fence(&[2, 0, 0, 0, 0, 0, 0, 1, 2]),
            records::decode_fence(&[1, 0]),
            records::decode_fence(&[]),
        ],
        [None, None, None]
    );
}

#[test]
fn keys_are_the_ids_bytes() {
    assert_eq!(
        records::fence_key(FenceKey::new(0x0102)),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2]
    );
    assert_eq!(
        records::log_key(
            &GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.kafka.keys.a.v1")
                .expect("valid id")
        ),
        b"gts.cf.core.events.topic.v1~x.kafka.keys.a.v1"
    );
}
