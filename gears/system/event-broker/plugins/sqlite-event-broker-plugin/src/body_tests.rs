use chrono::{TimeZone, Utc};
use event_broker_sdk::backend::{BackendError, Partition};
use event_broker_sdk::models::{Event, ProducerMeta};
use event_broker_sdk::{GtsTypeId, Sequence};
use uuid::Uuid;

use crate::body::{decode, encode};

fn event() -> Event {
    Event {
        id: Uuid::from_u128(1),
        type_id: GtsTypeId::new("gts.cf.core.events.event.v1~acme.orders.x.created.v1~"),
        tenant_id: Uuid::from_u128(2),
        source: "svc".to_owned(),
        subject: "order-7".to_owned(),
        subject_type: GtsTypeId::new("gts.acme.orders.x.order.v1~"),
        occurred_at: Utc.timestamp_opt(1_767_225_600, 0).single().unwrap(),
        trace_parent: Some("00-abc-def-01".to_owned()),
        data: Some(serde_json::json!({ "total": 10 })),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: Some(ProducerMeta {
            version: 1,
            producer_id: Some(Uuid::from_u128(3)),
            previous: Some(0),
            sequence: Some(1),
        }),
    }
}

#[test]
fn the_body_is_versioned_json_without_meta_or_positions() {
    let body = encode(&event()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "v": 1,
            "id": "00000000-0000-0000-0000-000000000001",
            "type_id": "gts.cf.core.events.event.v1~acme.orders.x.created.v1~",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "source": "svc",
            "subject": "order-7",
            "subject_type": "gts.acme.orders.x.order.v1~",
            "occurred_at": "2026-01-01T00:00:00Z",
            "trace_parent": "00-abc-def-01",
            "data": { "total": 10 },
        })
    );
}

#[test]
fn a_body_decodes_at_its_position() {
    let stored_at = Utc.timestamp_opt(1_767_225_700, 0).single().unwrap();
    let decoded = decode(&encode(&event()).unwrap(), Partition::new(3), 42, stored_at).unwrap();
    let expected = Event {
        partition: Some(3),
        sequence: Some(Sequence::assigned(42)),
        sequence_time: Some(stored_at),
        meta: None,
        ..event()
    };
    assert_eq!(format!("{decoded:?}"), format!("{expected:?}"));
}

#[test]
fn an_unknown_version_is_refused() {
    let body = String::from_utf8(encode(&event()).unwrap())
        .unwrap()
        .replace(r#""v":1"#, r#""v":2"#);
    match decode(body.as_bytes(), Partition::new(0), 1, Utc::now()) {
        Err(BackendError::Internal(detail)) => {
            assert_eq!(
                detail,
                "a stored event body has version 2, this plugin reads 1"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}
