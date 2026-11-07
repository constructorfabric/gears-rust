//! What a bare `EventBrokerHarness` knows. The harness's registry holds only the broker's own base
//! types, so a gear contract linked into the test binary's GTS inventory stays unknown to it until
//! the gear passes a registry that holds that contract. This file plays such a gear: it submits a
//! probe topic and event type to the inventory, the way a gear's own declarations do.
//! In-process: real services, `SQLite` gear database, memory backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use event_broker::test_support::EventBrokerHarness;
use event_broker::test_support::type_registry::registry_holding;
use event_broker_sdk::EventBrokerError;
use event_broker_sdk::gts::{EventV1, SubjectV1, TopicV1};
use gts::GtsIdPattern;
use serde_json::json;
use toolkit_gts::GtsSchema;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{ListEntitiesRequest, PlatformTypesRegistryApiExt};

const PROBE_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.inventory.probe.v1";
const PROBE_EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.inventory.probe.v1~";
const ORDER_SUBJECT: &str = "gts.cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~";

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

toolkit_gts::inventory::submit! {
    toolkit_gts::InventoryTypeSchema {
        type_id: PROBE_EVENT_TYPE,
        schema_fn: || {
            event_broker_sdk::gts::derived_event_type_schema(
                PROBE_EVENT_TYPE,
                PROBE_TOPIC,
                json!({}),
                &[],
            )
            .to_string()
        },
    }
}

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
    assert_eq!(event_schemas, vec![<EventV1>::TYPE_ID.to_owned()]);

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

    let mut subject_schemas: Vec<String> = registry
        .list_type_schemas(&ctx, pattern("gts.cf.core.events.subject.v1~*"))
        .await
        .unwrap()
        .into_iter()
        .map(|schema| schema.type_id.as_ref().to_owned())
        .collect();
    subject_schemas.sort();
    assert_eq!(subject_schemas, vec![SubjectV1::<()>::TYPE_ID.to_owned()]);
}

#[tokio::test]
async fn a_derived_subject_kind_seeded_beside_the_bases_resolves_against_the_subject_base() {
    let registry = registry_holding(&[], &[]);
    let ctx = PlatformSecurityContext::outbound_marker();
    // What `#[gts_type_schema(base = SubjectV1, properties = "")]` emits for a unit kind:
    // a copy of the expected body in `event-broker-sdk/src/gts_tests.rs`
    // `a_derived_subject_kind_names_its_full_id_and_derives_from_the_base`; change both
    // together.
    let order_subject = json!({
        "$id": "gts://gts.cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "An order placed in the fabrikam shop.",
        "type": "object",
        "allOf": [
            { "$ref": "gts://gts.cf.core.events.subject.v1~" },
            {
                "type": "object",
                "properties": {
                    "properties": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {},
                        "required": [],
                    },
                },
            },
        ],
    });
    registry.seed(ORDER_SUBJECT, order_subject.clone());

    let schemas = registry
        .list_type_schemas(&ctx, pattern(ORDER_SUBJECT))
        .await
        .unwrap_or_else(|err| panic!("the derived subject kind must resolve: {err:?}"));
    let [schema] = schemas.as_slice() else {
        panic!("expected exactly the derived subject kind, got {schemas:?}");
    };
    assert_eq!(schema.type_id.as_ref(), ORDER_SUBJECT);
    assert_eq!(schema.content, Some(order_subject));
    // The subject base, inlined: resolution needs it in the registry.
    assert_eq!(
        schema.resolved_schema,
        Some(json!({
            "$id": "gts://gts.cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "allOf": [
                {
                    "additionalProperties": false,
                    "description": "Subject type. The kind of entity an event is about; an event names it in `subject_type`. By convention, a gear declares each subject kind it owns as a derived type schema under this base; `subject_type` accepts any GTS type id.",
                    "properties": {
                        "id": {
                            "description": "The subject's identifier, as events carry it in `subject`. ASCII only.",
                            "maxLength": 1024,
                            "minLength": 1,
                            "pattern": "^[\\x20-\\x7E]+$",
                            "type": "string",
                        },
                        "properties": { "type": "object" },
                        "type": {
                            "description": "Full GTS subject-type identifier: the derived type this subject is an instance of.",
                            "type": "string",
                            "x-gts-type-ref": "/$id",
                        },
                    },
                    "required": ["type", "id", "properties"],
                    "type": "object",
                },
                {
                    "properties": {
                        "properties": {
                            "additionalProperties": false,
                            "properties": {},
                            "required": [],
                            "type": "object",
                        },
                    },
                    "type": "object",
                },
            ],
            "description": "An order placed in the fabrikam shop.",
            "type": "object",
        }))
    );
}
