#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Partition};
use event_broker_sdk::models::{Event, IngestStamp};
use event_broker_sdk::{GtsTypeId, Sequence};
use uuid::Uuid;

use crate::body;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).expect("valid instant")
}

fn event() -> Event {
    Event {
        id: Uuid::from_u128(1),
        event_type: GtsTypeId::new("gts.cf.core.events.event.v1~acme.x.happened.v1~"),
        subject_tenant: Uuid::from_u128(2),
        source: "src".to_owned(),
        subject: "sub".to_owned(),
        subject_type: GtsTypeId::new("gts.acme.x.things.thing.v1~"),
        subject_owner: Some(Uuid::from_u128(4)),
        occurred_at: at(1_700_000_000),
        trace_parent: Some("00-tp".to_owned()),
        data: Some(serde_json::json!({ "a": 1 })),
        ingest: Some(IngestStamp {
            tenant: Uuid::from_u128(5),
            actor: Uuid::from_u128(6),
        }),
        partition: Some(9),
        sequence: Some(Sequence::assigned(9)),
        sequence_time: Some(at(1)),
        meta: None,
    }
}

#[test]
fn an_event_round_trips_with_the_records_position() {
    let encoded = body::encode(&event()).unwrap();
    let decoded = body::decode(
        &encoded,
        Partition::new(3),
        Sequence::assigned(42),
        at(1_800_000_000),
    )
    .unwrap();
    // `Event` has no `PartialEq`: its debug form shows every field.
    assert_eq!(
        format!("{decoded:?}"),
        format!(
            "{:?}",
            Event {
                partition: Some(3),
                sequence: Some(Sequence::assigned(42)),
                sequence_time: Some(at(1_800_000_000)),
                ..event()
            }
        )
    );
}

#[test]
fn a_foreign_value_is_internal_and_quotes_nothing() {
    let refused = [
        body::decode(b"{\"secret\":1}", Partition::new(0), Sequence::assigned(1), at(0)),
        body::decode(
            br#"{"v":9,"id":"00000000-0000-0000-0000-000000000001","type_id":"t","subject_tenant":"00000000-0000-0000-0000-000000000002","source":"s","subject":"s","subject_type":"t","occurred_at":"2024-01-01T00:00:00Z"}"#,
            Partition::new(0),
            Sequence::assigned(1),
            at(0),
        ),
    ]
    .map(|r| match r {
        Err(BackendError::Internal(message)) => message,
        other => panic!("expected Internal, got {other:?}"),
    });
    assert_eq!(
        refused,
        [
            "a stored event body could not be decoded",
            "a stored event body has version 9, this plugin reads 1",
        ]
    );
}

/// The value keeps the owner and the ingest stamp, the stamp under `meta`.
#[test]
fn the_value_keeps_the_owner_and_the_ingest_stamp_under_meta() {
    let value: serde_json::Value =
        serde_json::from_slice(&body::encode(&event()).unwrap()).unwrap();
    assert_eq!(
        (&value["subject_owner"], &value["meta"]),
        (
            &serde_json::json!("00000000-0000-0000-0000-000000000004"),
            &serde_json::json!({
                "ingest_tenant": "00000000-0000-0000-0000-000000000005",
                "ingest_actor": "00000000-0000-0000-0000-000000000006",
            }),
        )
    );
}

/// A value without `subject_owner` or `meta`, as one stored before either existed,
/// decodes as a tenant-wide event with no ingest stamp.
#[test]
fn a_value_without_an_owner_or_a_stamp_decodes_as_tenant_wide_and_unstamped() {
    let decoded = body::decode(
        br#"{"v":1,"id":"00000000-0000-0000-0000-000000000001","type_id":"t","subject_tenant":"00000000-0000-0000-0000-000000000002","source":"s","subject":"s","subject_type":"t","occurred_at":"2024-01-01T00:00:00Z"}"#,
        Partition::new(0),
        Sequence::assigned(1),
        at(0),
    )
    .unwrap();
    assert_eq!((decoded.subject_owner, decoded.ingest), (None, None));
}
