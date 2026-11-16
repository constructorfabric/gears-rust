use chrono::{TimeZone, Utc};
use event_broker_sdk::backend::{BackendError, Partition};
use event_broker_sdk::models::{Event, IngestStamp, ProducerMeta};
use event_broker_sdk::{GtsTypeId, Sequence};
use uuid::Uuid;

use crate::body::{decode, encode};

fn event() -> Event {
    Event {
        id: Uuid::from_u128(1),
        event_type: GtsTypeId::new("gts.cf.core.events.event.v1~acme.orders.x.created.v1~"),
        subject_tenant: Uuid::from_u128(2),
        source: "svc".to_owned(),
        subject: "order-7".to_owned(),
        subject_type: GtsTypeId::new("gts.acme.orders.x.order.v1~"),
        subject_owner: Some(Uuid::from_u128(4)),
        occurred_at: Utc.timestamp_opt(1_767_225_600, 0).single().unwrap(),
        trace_parent: Some("00-abc-def-01".to_owned()),
        data: Some(serde_json::json!({ "total": 10 })),
        ingest: Some(IngestStamp {
            tenant: Uuid::from_u128(5),
            actor: Uuid::from_u128(6),
        }),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: Some(ProducerMeta {
            version: 1,
            producer: Some(Uuid::from_u128(3)),
            previous: Some(0),
            sequence: Some(1),
        }),
    }
}

#[test]
fn the_body_is_versioned_json_without_the_producer_meta_or_positions() {
    let body = encode(&event()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "v": 1,
            "id": "00000000-0000-0000-0000-000000000001",
            "type_id": "gts.cf.core.events.event.v1~acme.orders.x.created.v1~",
            "subject_tenant": "00000000-0000-0000-0000-000000000002",
            "source": "svc",
            "subject": "order-7",
            "subject_type": "gts.acme.orders.x.order.v1~",
            "occurred_at": "2026-01-01T00:00:00Z",
            "subject_owner": "00000000-0000-0000-0000-000000000004",
            "trace_parent": "00-abc-def-01",
            "data": { "total": 10 },
            "meta": {
                "ingest_tenant": "00000000-0000-0000-0000-000000000005",
                "ingest_actor": "00000000-0000-0000-0000-000000000006",
            },
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

/// A body without `subject_owner` or `meta`, as one stored before either existed,
/// decodes as a tenant-wide event with no ingest stamp.
#[test]
fn a_body_without_an_owner_or_a_stamp_decodes_as_tenant_wide_and_unstamped() {
    let decoded = decode(
        br#"{"v":1,"id":"00000000-0000-0000-0000-000000000001","type_id":"t","subject_tenant":"00000000-0000-0000-0000-000000000002","source":"s","subject":"s","subject_type":"t","occurred_at":"2024-01-01T00:00:00Z"}"#,
        Partition::new(0),
        1,
        Utc::now(),
    )
    .unwrap();
    assert_eq!((decoded.subject_owner, decoded.ingest), (None, None));
}
