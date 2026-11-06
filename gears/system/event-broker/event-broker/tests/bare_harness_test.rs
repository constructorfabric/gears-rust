//! What a bare `EventBrokerHarness` knows. The harness's registry holds only the broker's own base
//! types, so a gear contract linked into the test binary's GTS inventory stays unknown to it until
//! the gear passes a registry that holds that contract. This file plays such a gear: it submits a
//! probe topic and event type to the inventory, the way a gear's own declarations do.
//! In-process: real services, `SQLite` gear database, memory backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use event_broker::test_support::EventBrokerHarness;
use event_broker::test_support::type_registry::registry_holding;
use event_broker_sdk::EventBrokerError;
use event_broker_sdk::gts::{EventV1, TopicV1};
use gts::GtsIdPattern;
use serde_json::json;
use toolkit_gts::GtsSchema;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{ListEntitiesRequest, PlatformTypesRegistryApiExt};

const PROBE_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.inventory.probe.v1";
const PROBE_EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.inventory.probe.v1~";

toolkit_gts::inventory::submit! {
    toolkit_gts::InventoryInstance {
        type_id: "gts.cf.core.events.topic.v1~",
        instance_id: PROBE_TOPIC,
        payload_fn: || json!({
            "id": PROBE_TOPIC,
            "description": "a gear contract linked into this binary",
        }),
    }
}

event_broker::fixture_event_type!(
    /// The probe event type: a declaration in this binary's inventory, as a gear's own
    /// event types are.
    ProbeEventV1 = "cf.core.events.event.v1~x.eb.inventory.probe.v1~",
    topic = PROBE_TOPIC,
);

fn pattern(pattern: &str) -> ListEntitiesRequest {
    ListEntitiesRequest::matching(GtsIdPattern::try_new(pattern).expect("a valid GTS pattern"))
}

#[tokio::test]
async fn a_bare_harness_knows_no_gear_contract_linked_into_the_binary() {
    let harness = EventBrokerHarness::builder().build().await;
    let broker = harness.broker();
    let ctx = harness.security_context();

    let (type_id, detail) = match broker.get_event_type(ctx, PROBE_EVENT_TYPE).await {
        Err(EventBrokerError::EventTypeUnknown { type_id, detail }) => (type_id, detail),
        other => panic!("the probe event type must be unknown to a bare harness: {other:?}"),
    };
    assert_eq!(type_id, PROBE_EVENT_TYPE);
    assert_eq!(detail, "no such event type");

    let topics: Vec<String> = broker
        .list_topics(ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|topic| topic.id.as_ref().to_owned())
        .collect();
    assert_eq!(topics, Vec::<String>::new());

    let event_types: Vec<String> = broker
        .list_event_types(ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|event_type| event_type.id.as_ref().to_owned())
        .collect();
    assert_eq!(event_types, Vec::<String>::new());
}

#[tokio::test]
async fn registry_holding_seeds_only_the_broker_base_types() {
    let registry = registry_holding(&[], &[]);
    let ctx = PlatformSecurityContext::outbound_marker();

    let mut event_schemas: Vec<String> = registry
        .list_type_schemas(&ctx, pattern("gts.cf.core.events.event.v1~*"))
        .await
        .unwrap()
        .into_iter()
        .map(|schema| schema.type_id.as_ref().to_owned())
        .collect();
    event_schemas.sort();
    assert_eq!(event_schemas, vec![EventV1::<()>::TYPE_ID.to_owned()]);

    let mut topic_instances: Vec<String> = registry
        .list_instances(&ctx, pattern("gts.cf.core.events.topic.v1~*"))
        .await
        .unwrap()
        .into_iter()
        .map(|instance| instance.id.as_ref().to_owned())
        .collect();
    topic_instances.sort();
    assert_eq!(topic_instances, Vec::<String>::new());

    let mut topic_schemas: Vec<String> = registry
        .list_type_schemas(&ctx, pattern("gts.cf.core.events.topic.v1~"))
        .await
        .unwrap()
        .into_iter()
        .map(|schema| schema.type_id.as_ref().to_owned())
        .collect();
    topic_schemas.sort();
    assert_eq!(topic_schemas, vec![TopicV1::TYPE_ID.to_owned()]);
}
