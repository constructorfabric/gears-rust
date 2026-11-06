//! Worked producer examples: one concrete event type declared in Rust under
//! [`EventV1`], and one topic instance of [`TopicV1`].
//!
//! A gear declares its event types this way - a `#[gts_type_schema(base = EventV1)]`
//! struct whose fields are the payload, with the type's traits alongside - and
//! registers its topics as instances. Each example here is admitted the way
//! `types-registry` admits it, through a `GtsStore` holding the generated bases,
//! so an example that stops deriving from its base or breaks the trait contract
//! fails here. `fabrikam` is the vendor this repository uses for a third party
//! (`docs/TOOLKIT_PLUGINS.md`).
//!
//! The declarations live in this integration-test binary rather than in the
//! crate, so the crate itself still declares only the two base types.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use event_broker_sdk::gts::{EventV1, TopicV1, data_contract};
use gts::{GtsRefValidation, GtsSchema, GtsStore};
use serde_json::{Value, json};
use toolkit_gts::{gts_id, gts_type_schema};
use uuid::Uuid;

/// An order was placed in a third-party shop. The payload is the struct's
/// fields; the traits route every event of this type to the shop's orders topic
/// and limit its subjects to orders.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~"),
    description = "Worked producer example: one concrete event type. It narrows the base event's `data` member to its own payload contract, and fixes its trait values.",
    properties = "order_id,total_cents,currency,coupon_code",
    traits = json!({
        "topic": gts_id!("cf.core.events.topic.v1~fabrikam.shop._.orders.v1"),
        "allowed_subject_types": [gts_id!("fabrikam.shop.orders.order.v1~")],
    }),
)]
pub struct OrderPlacedV1 {
    pub order_id: Uuid,
    pub total_cents: u64,
    #[schemars(regex(pattern = r"^[A-Z]{3}$"))]
    pub currency: String,
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub coupon_code: Option<String>,
}

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~fabrikam.shop._.orders.v1");

/// A store holding both generated bases and the example event type, as
/// `types-registry` would after the owning gear's init.
fn store() -> GtsStore {
    let mut store = GtsStore::new();
    store
        .register_schema(TopicV1::TYPE_ID, &TopicV1::gts_schema_with_refs())
        .unwrap();
    store
        .register_schema(
            EventV1::<()>::TYPE_ID,
            &EventV1::<()>::gts_schema_with_refs(),
        )
        .unwrap();
    store
        .register_schema(
            OrderPlacedV1::TYPE_ID,
            &OrderPlacedV1::gts_schema_with_refs(),
        )
        .unwrap();
    store
}

fn order_placed(data: Option<Value>) -> Value {
    let mut event = json!({
        "id": Uuid::nil(),
        "type": OrderPlacedV1::TYPE_ID,
        "tenant_id": Uuid::nil(),
        "source": "fabrikam-shop",
        "subject": "order-42",
        "subject_type": gts_id!("fabrikam.shop.orders.order.v1~"),
        "occurred_at": "2026-01-01T00:00:00Z",
        "partition": 0,
        "sequence": 1,
        "sequence_time": "2026-01-01T00:00:01Z",
    });
    if let Some(data) = data {
        event["data"] = data;
    }
    event
}

fn order() -> Value {
    json!({ "order_id": Uuid::nil(), "total_cents": 1250, "currency": "EUR" })
}

#[test]
fn the_rust_declared_event_type_is_admitted_under_the_event_base() {
    let resolved = store()
        .validate_schema_with(OrderPlacedV1::TYPE_ID, GtsRefValidation::None)
        .expect("the derived event type and its traits are admissible");
    assert!(!resolved.is_abstract, "a concrete event type");
    assert_eq!(resolved.effective_traits["topic"], ORDERS_TOPIC);
    assert_eq!(
        resolved.effective_traits["partition_key"], "/tenant_id",
        "an event type declaring no partition key inherits the base default"
    );
}

#[test]
fn an_event_of_a_payload_bearing_type_must_carry_its_payload() {
    let mut store = store();
    store
        .validate_payload_with(
            OrderPlacedV1::TYPE_ID,
            &order_placed(Some(order())),
            GtsRefValidation::None,
        )
        .expect("an event with its payload validates");
    store
        .validate_payload_with(
            OrderPlacedV1::TYPE_ID,
            &order_placed(None),
            GtsRefValidation::None,
        )
        .expect_err("the base's optional `data` becomes required once a type declares a payload");
}

/// Producer-side validation targets the payload alone, through
/// [`data_contract`]; the contract it extracts from a Rust-declared type must be
/// the one the struct declares.
#[test]
fn the_data_contract_of_a_rust_declared_type_is_its_payload() {
    let resolved = store()
        .resolve_schema_refs(&OrderPlacedV1::gts_schema_with_refs())
        .expect("the derived schema resolves against the base");
    let validator =
        jsonschema::validator_for(&data_contract(&resolved)).expect("the data contract compiles");

    assert!(validator.is_valid(&order()));
    let mut unknown_currency = order();
    unknown_currency["currency"] = json!("eur");
    assert!(!validator.is_valid(&unknown_currency));
    assert!(!validator.is_valid(&json!({ "order_id": Uuid::nil() })));
}

#[test]
fn a_topic_instance_validates_against_the_topic_base() {
    let topic = json!({
        "id": ORDERS_TOPIC,
        "description": "Worked producer example: a third-party application's own order-lifecycle stream, registered by that application at its init rather than by the broker.",
        "retention": "P7D",
    });
    store()
        .validate_payload_with(TopicV1::TYPE_ID, &topic, GtsRefValidation::None)
        .expect("the example topic is a valid topic instance");
}
