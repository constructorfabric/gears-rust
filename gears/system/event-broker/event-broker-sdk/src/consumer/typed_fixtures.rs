//! The order events and payload type the plain (`typed_tests`) and the transactional
//! (`typed_tx_tests`) typed-route tests share.

use gts::{GtsIdPattern, GtsInstanceId, GtsSchema, GtsTypeId};
use serde::{Deserialize, Serialize};
use toolkit_gts::gts_id;
use uuid::Uuid;

use super::{Delivery, HandlerOutcome};
use crate::gts::{EventV1, StoredPosition};
use crate::sequence::Sequence;

pub(super) const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
pub(super) const ORDER_PLACED: &str =
    gts_id!("cf.core.events.event.v1~example.orders.order_placed.x.v1~");
pub(super) const ORDER_CREATED: &str =
    gts_id!("cf.core.events.event.v1~example.orders.order_created.x.v1~");

/// The typed payload of the route under test; `order_id` and `quantity` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct OrderPlacedV1 {
    pub(super) order_id: String,
    pub(super) quantity: u32,
}

impl GtsSchema for OrderPlacedV1 {
    const TYPE_ID: &str = ORDER_PLACED;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

pub(super) fn ginst(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid GTS instance id")
}

pub(super) fn gpat(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

/// Partition 7 at `sequence`.
pub(super) fn position_at(sequence: i64) -> StoredPosition {
    StoredPosition {
        partition: 7,
        sequence: Sequence::assigned(sequence),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    }
}

/// An `ORDER_PLACED` event with id `id` carrying `data`, stamped at `sequence` of
/// partition 7.
pub(super) fn order_event(id: u128, sequence: i64, data: serde_json::Value) -> EventV1 {
    EventV1::builder_for(GtsTypeId::new(ORDER_PLACED), data)
        .id(Uuid::from_u128(id))
        .subject_tenant(Uuid::nil())
        .subject("order-1")
        .subject_type_of(GtsTypeId::new("gts.x.eb.test.subject.v1~"))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap()
        .stamp_position(position_at(sequence))
}

/// The payload `good_at(id, ..)` carries.
pub(super) fn order(id: u128) -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: format!("o-{id}"),
        quantity: 3,
    }
}

/// An event with id `id` at `sequence` that decodes as [`order`]`(id)`.
pub(super) fn good_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}"), "quantity": 3 }),
    )
}

/// An event with id `id` at `sequence` that misses the required `quantity`.
pub(super) fn bad_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}") }),
    )
}

/// `[ok, ok, bad, ok]` at sequences 41 to 44: `k = 2`, `last = 42`, with a tail after
/// the bad event.
pub(super) fn mixed_batch() -> Vec<EventV1> {
    vec![
        good_at(1, 41),
        good_at(2, 42),
        bad_at(3, 43),
        good_at(4, 44),
    ]
}

/// `[bad, ok]` at sequences 41 and 42: `k = 0`.
pub(super) fn bad_first_batch() -> Vec<EventV1> {
    vec![bad_at(1, 41), good_at(2, 42)]
}

/// `[ok, ok, ok]` at sequences 41 to 43: `k = n`.
pub(super) fn good_batch() -> Vec<EventV1> {
    vec![good_at(1, 41), good_at(2, 42), good_at(3, 43)]
}

/// Topic, partition and attempts of the delivery the adapter is called with.
pub(super) fn delivery() -> Delivery {
    Delivery::new(ginst(TOPIC), 7, 2)
}

pub(super) fn not_yet() -> HandlerOutcome {
    HandlerOutcome::Retry {
        reason: "not yet".to_owned(),
    }
}
