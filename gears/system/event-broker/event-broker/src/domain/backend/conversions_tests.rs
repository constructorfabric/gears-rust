//! The owner and the ingest stamp across the domain <-> backend conversions.

use chrono::{TimeZone, Utc};
use serde_json::json;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use super::{from_sdk_event, partition_input, to_sdk_event};
use crate::domain::ingest::PublishRequest;
use crate::domain::model::{Event, IngestStamp};

const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.conv.created.v1~";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.conv.orders.v1";
const OWNER: Uuid = Uuid::from_u128(0x0e);

fn event(owner: Option<Uuid>, ingest: Option<IngestStamp>) -> Event {
    Event {
        id: Uuid::from_u128(1),
        r#type: crate::test_support::event_type_id(EVENT_TYPE),
        topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
        subject_tenant: Uuid::from_u128(2),
        source: "svc".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.conv.order.v1~"),
        subject_owner: owner,
        occurred_at: Utc.timestamp_opt(1_767_225_600, 0).single().unwrap(),
        trace_parent: None,
        data: json!({ "n": 1 }),
        meta: None,
        ingest,
        partition: None,
        sequence: None,
        sequence_time: None,
    }
}

/// The backend record keeps the owner and the stamp, and reading it back restores
/// both on the domain event.
#[test]
fn the_owner_and_the_ingest_stamp_survive_the_backend_round_trip() {
    let stamp = IngestStamp {
        tenant: Uuid::from_u128(0x7e),
        actor: Uuid::from_u128(0xac),
    };
    let cases = [(Some(OWNER), Some(stamp)), (None, None)];

    let read_back: Vec<_> = cases
        .iter()
        .map(|(owner, ingest)| {
            let record = to_sdk_event(&event(*owner, *ingest));
            let sdk = (
                record.subject_owner,
                record.ingest.map(|stamp| (stamp.tenant, stamp.actor)),
            );
            let back = from_sdk_event(&GtsInstanceId::try_new(TOPIC).unwrap(), record)
                .expect("the record reads back");
            (sdk, (back.subject_owner, back.ingest))
        })
        .collect();

    assert_eq!(
        read_back,
        vec![
            (
                (
                    Some(OWNER),
                    Some((Uuid::from_u128(0x7e), Uuid::from_u128(0xac)))
                ),
                (Some(OWNER), Some(stamp)),
            ),
            ((None, None), (None, None)),
        ]
    );
}

/// A partition pointer at `/subject_owner` reads the request's owner, and an
/// unowned request does not resolve it.
#[test]
fn a_partition_pointer_at_the_owner_reads_the_requests_owner() {
    let request = |owner: Option<Uuid>| PublishRequest {
        id: Uuid::from_u128(1),
        r#type: crate::test_support::event_type_id(EVENT_TYPE),
        subject_tenant: Uuid::from_u128(2),
        source: "svc".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.conv.order.v1~"),
        subject_owner: owner,
        occurred_at: Utc.timestamp_opt(1_767_225_600, 0).single().unwrap(),
        trace_parent: None,
        data: json!({ "n": 1 }),
        meta: None,
    };

    let keys = [Some(OWNER), None]
        .map(|owner| partition_input(&request(owner), "/subject_owner").map_err(|e| e.to_string()));

    assert_eq!(
        keys,
        [
            Ok("00000000-0000-0000-0000-00000000000e".to_owned()),
            Err("PartitionKeyUnresolved: event type \
                 'gts.cf.core.events.event.v1~x.eb.conv.created.v1~' partitions on \
                 `/subject_owner`, which this event does not resolve: internal: partition-key \
                 pointer `/subject_owner` resolves to null"
                .to_owned()),
        ]
    );
}
