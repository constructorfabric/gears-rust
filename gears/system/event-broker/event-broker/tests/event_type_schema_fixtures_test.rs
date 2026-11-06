//! The harness resolves macro-declared events and their traits.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use event_broker::test_support::EventBrokerHarness;
use event_broker::test_support::type_registry::registry_holding;
use event_broker_sdk::gts::EventV1;
use gts::GtsSchema;
use serde_json::json;
use toolkit_gts::{gts_id, gts_type_schema};
use uuid::Uuid;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.fixture.topic.v1");

#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~example.eb.fixture.order.v1~"),
    description = "An order partitioned by its identifier.",
    properties = "order_id",
    traits = json!({
        "topic": TOPIC,
        "allowed_subject_types": ["gts.example.eb.fixture.subject.v1~"],
        "partition_key": "/data/order_id",
    }),
)]
pub struct OrderV1 {
    pub order_id: Uuid,
}

#[tokio::test]
async fn a_declared_fixture_preserves_its_payload_contract_and_traits() {
    let harness = EventBrokerHarness::builder()
        .with_types_registry_client(
            registry_holding(
                &[json!({ "id": TOPIC, "description": "Order events" })],
                &[OrderV1::gts_schema_with_refs()],
            ),
            &[(TOPIC, 1)],
        )
        .build()
        .await;
    let event_type = harness
        .broker()
        .get_event_type(harness.security_context(), OrderV1::TYPE_ID)
        .await
        .unwrap();
    assert_eq!(event_type.topic.as_ref(), TOPIC);
    assert_eq!(event_type.partition_key, "/data/order_id");
    assert_eq!(
        event_type.allowed_subject_types[0].pattern(),
        "gts.example.eb.fixture.subject.v1~"
    );
    let validator = jsonschema::validator_for(&event_type.data_schema).unwrap();
    assert!(validator.is_valid(&json!({ "order_id": Uuid::nil() })));
    assert!(!validator.is_valid(&json!({})));
    assert!(!validator.is_valid(&json!({ "order_id": 42 })));
}
