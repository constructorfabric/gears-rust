//! Event declarations must register as narrowings of the broker's base type.

use event_broker_sdk::gts::{EventV1, TopicV1, data_contract};
use gts::{GtsRefValidation, GtsSchema, GtsStore};
use serde_json::json;
use toolkit_gts::{gts_id, gts_type_schema};
use uuid::Uuid;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.generic.topic.v1");

#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~example.eb.generic.order_placed.v1~"),
    description = "An order was placed",
    properties = "order_id",
    traits = json!({ "topic": TOPIC, "allowed_subject_types": ["gts.*"] }),
)]
pub struct OrderPlacedV1 {
    pub order_id: Uuid,
}

#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~example.eb.generic.notification.v1~"),
    description = "A notification without a payload",
    properties = "",
    traits = json!({ "topic": TOPIC }),
)]
pub struct NotificationV1;

fn store() -> GtsStore {
    let mut store = GtsStore::new();
    for (id, schema) in [
        (TopicV1::TYPE_ID, TopicV1::gts_schema_with_refs()),
        (
            EventV1::<()>::TYPE_ID,
            EventV1::<()>::gts_schema_with_refs(),
        ),
        (
            OrderPlacedV1::TYPE_ID,
            OrderPlacedV1::gts_schema_with_refs(),
        ),
        (
            NotificationV1::TYPE_ID,
            NotificationV1::gts_schema_with_refs(),
        ),
    ] {
        store.register_schema(id, &schema).unwrap();
    }
    store
}

#[test]
fn a_derived_event_registers_and_validates_its_payload() {
    let mut store = store();
    let resolved = store
        .validate_schema_with(OrderPlacedV1::TYPE_ID, GtsRefValidation::None)
        .unwrap();
    let validator = jsonschema::validator_for(&data_contract(&resolved.schema)).unwrap();
    assert!(validator.is_valid(&json!({ "order_id": Uuid::nil() })));
    for invalid in [json!({}), json!({ "order_id": 42 }), json!(null), json!([])] {
        assert!(!validator.is_valid(&invalid), "{invalid}");
    }
}

#[test]
fn a_bodyless_derived_event_registers_without_requiring_data() {
    let mut store = store();
    store
        .validate_schema_with(NotificationV1::TYPE_ID, GtsRefValidation::None)
        .unwrap();
    let mut event = json!({
        "id": Uuid::nil(),
        "type": NotificationV1::TYPE_ID,
        "tenant_id": Uuid::nil(),
        "source": "test",
        "subject": "order",
        "subject_type": OrderPlacedV1::TYPE_ID,
        "occurred_at": "2026-11-06T00:00:00Z",
        "partition": 0,
        "sequence": 1,
        "sequence_time": "2026-11-06T00:00:00Z",
    });
    store
        .validate_payload_with(NotificationV1::TYPE_ID, &event, GtsRefValidation::None)
        .unwrap();
    event["type"] = json!(OrderPlacedV1::TYPE_ID);
    assert!(
        store
            .validate_payload_with(OrderPlacedV1::TYPE_ID, &event, GtsRefValidation::None)
            .is_err(),
        "a payload-bearing derived event requires data"
    );
}

fn admit_event_with_subject_pattern(pattern: &str) -> Result<gts::ResolvedType, gts::StoreError> {
    let mut store = gts::GtsStore::new();
    let base = EventV1::<()>::gts_schema_with_refs();
    store
        .register_schema(EventV1::<()>::TYPE_ID, &base)
        .unwrap();
    let type_id = NotificationV1::TYPE_ID;
    let mut derived = NotificationV1::gts_schema_with_refs();
    derived["x-gts-traits"]["allowed_subject_types"] = json!([pattern]);
    store.register_schema(type_id, &derived).unwrap();
    store.validate_schema_with(type_id, gts::GtsRefValidation::None)
}

#[test]
fn event_type_admission_accepts_concrete_wildcard_and_instance_subject_patterns() {
    for pattern in [
        "gts.*",
        "gts.fabrikam.shop.orders.*",
        "gts.fabrikam.shop.orders.order.v*",
        "gts.fabrikam.shop.orders.order.v1.*",
        "gts.fabrikam.shop.orders.order.v1~",
        "gts.fabrikam.shop.orders.order.v1.2~*",
        "gts.fabrikam.shop.orders.order.v1~fabrikam.shop.orders.*",
        "gts.fabrikam.shop.orders.order.v1~fabrikam.shop.orders.special.v1~",
        "gts.fabrikam.shop.orders.order.v1~fabrikam.shop.orders.named.v1",
    ] {
        gts::GtsIdPattern::try_new(pattern).expect("valid GTS pattern fixture");
        admit_event_with_subject_pattern(pattern)
            .unwrap_or_else(|error| panic!("subject pattern {pattern} must be admitted: {error}"));
    }
}

#[test]
fn event_type_admission_rejects_malformed_subject_patterns() {
    let too_long = format!("gts.fabrikam.shop.orders.{}.v1~", "a".repeat(1024));
    for pattern in [
        "not-gts.*",
        "gts.fabrikam.*.orders.order.v1~",
        "gts.fabrikam.shop.orders.*~*",
        "gts.fabrikam.shop.orders.ord*",
        "gts.fabrikam.shop.orders.order.v01~",
        &too_long,
    ] {
        assert!(
            admit_event_with_subject_pattern(pattern).is_err(),
            "invalid subject type pattern {pattern} must be rejected"
        );
    }
}
