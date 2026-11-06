//! Unit tests for the topic resolution in `event_type.rs`.
//!
//! Kept in a sibling `_tests.rs` file per the repo's tests-in-separate-files
//! lint, linked in from `event_type.rs`.
//!
//! A fixture the registry admits is listed through `PlatformTypesRegistryApi`
//! from a registry that resolves it, so the traits and schema read here are the
//! ones resolution materializes. A fixture the registry would refuse is built
//! as a `TypeSchema` literal instead.

use gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};
use serde_json::json;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{ListEntitiesRequest, PlatformTypesRegistryApiExt, TypeSchema};
use uuid::Uuid;

use super::{topic, validate_partition_key};
use crate::domain::error::DomainError;

/// The base type the broker owns; abstract, and the only declarer of the
/// event trait schema.
const BASE_ID: &str = "gts.cf.core.events.event.v1~";
/// The worked example event type (`event-broker-sdk/tests/worked_examples.rs`).
const DERIVED_ID: &str = "gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~";
/// A topic is an instance, so the example topic's identifier does not end in `~`.
const EXAMPLE_TOPIC: &str = "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1";

/// The base's `x-gts-traits-schema` as the broker declares it - `topic`
/// required, `allowed_subject_types` defaulting to `[]`, `partition_key`
/// defaulting to the tenant member the base also declares. Trait defaults are
/// resolved from this block, so the chain must carry it for resolution to
/// behave as it does in production.
fn base() -> serde_json::Value {
    json!({
        "$id": format!("gts://{BASE_ID}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-abstract": true,
        "properties": {
            "tenant_id": { "type": "string" },
            "data": { "type": ["object", "null"] }
        },
        "x-gts-traits-schema": {
            "type": "object",
            "required": ["topic"],
            "properties": {
                "topic": { "type": "string" },
                "allowed_subject_types": {
                    "type": "array",
                    "default": [],
                    "items": { "type": "string" }
                },
                "partition_key": {
                    "type": "string",
                    "format": "json-pointer",
                    "default": "/tenant_id"
                }
            }
        }
    })
}

/// `DERIVED_ID` as `list_type_schemas` reports it from a registry holding
/// `documents`, keyed by id.
async fn listed(documents: &[(&str, serde_json::Value)]) -> TypeSchema {
    listed_as(DERIVED_ID, documents).await
}

/// `id` as `list_type_schemas` reports it from a registry holding `documents`,
/// keyed by id.
async fn listed_as(id: &str, documents: &[(&str, serde_json::Value)]) -> TypeSchema {
    let registry = MockTypesRegistry::new();
    for (id, document) in documents {
        registry.seed(id, document.clone());
    }
    let listed = registry
        .list_type_schemas(
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            ListEntitiesRequest::matching(
                GtsIdPattern::try_new(&format!("{BASE_ID}*")).expect("a valid pattern"),
            ),
        )
        .await
        .expect("the registry resolves every fixture");
    let matching: Vec<&TypeSchema> = listed
        .iter()
        .filter(|schema| schema.type_id.as_ref() == id)
        .collect();
    let [schema] = matching.as_slice() else {
        panic!("expected one listed {id}, got {matching:?}");
    };
    (*schema).clone()
}

/// A derived type the registry would refuse to admit, as it would reach the
/// broker had it been admitted: `effective_traits` is what resolution would
/// have merged, over a resolved schema carrying the payload `data` narrows.
fn unadmitted(effective_traits: serde_json::Value) -> TypeSchema {
    TypeSchema {
        type_id: GtsTypeId::new(DERIVED_ID),
        type_uuid: Uuid::nil(),
        lifecycle_status: types_registry_sdk::LifecycleStatus::Active,
        origin: None,
        content: None,
        resolved_schema: Some(json!({
            "type": "object",
            "allOf": [
                { "type": "object", "properties": { "tenant_id": { "type": "string" } } },
                {
                    "type": "object",
                    "properties": {
                        "data": {
                            "type": "object",
                            "properties": { "order_id": { "type": "string" } },
                        },
                    },
                },
            ],
        })),
        effective_traits: Some(effective_traits),
        effective_traits_schema: None,
        provenance: None,
    }
}

async fn derived(traits: &serde_json::Value) -> TypeSchema {
    listed(&[
        (BASE_ID, base()),
        (
            DERIVED_ID,
            json!({
                "$id": format!("gts://{DERIVED_ID}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "x-gts-traits": traits,
                "allOf": [
                    { "$ref": format!("gts://{BASE_ID}") },
                    {
                        "type": "object",
                        "properties": {
                            "data": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["order_id"],
                                "properties": {
                                    "order_id": { "type": "string", "format": "uuid" }
                                }
                            }
                        }
                    }
                ]
            }),
        ),
    ])
    .await
}

#[tokio::test]
async fn topic_resolves_the_declared_trait_as_a_gts_type_id() {
    let event_type = derived(&json!({
        "topic": EXAMPLE_TOPIC,
        "allowed_subject_types": ["gts.fabrikam.shop.orders.order.v1~"],
    }))
    .await;

    let resolved = topic(&event_type).expect("the declared topic trait must resolve");

    assert_eq!(resolved, GtsInstanceId::try_new(EXAMPLE_TOPIC).unwrap());
}

#[tokio::test]
async fn topic_inherits_a_value_declared_further_up_the_chain() {
    // A family type may set a topic that its leaves publish to without
    // restating it. The leaf declares no traits of its own, so the topic comes
    // from the intermediate.
    let family = "gts.cf.core.events.event.v1~fabrikam.shop.orders.family.v1~";
    let leaf = "gts.cf.core.events.event.v1~fabrikam.shop.orders.family.v1~\
                fabrikam.shop.orders.order_placed.v1~";
    let event_type = listed_as(
        leaf,
        &[
            (BASE_ID, base()),
            (
                family,
                json!({
                    "$id": format!("gts://{family}"),
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "x-gts-traits": { "topic": EXAMPLE_TOPIC },
                    "allOf": [
                        { "$ref": format!("gts://{BASE_ID}") },
                        {
                            "type": "object",
                            "properties": { "data": { "type": ["object", "null"] } }
                        }
                    ]
                }),
            ),
            (
                leaf,
                json!({
                    "$id": format!("gts://{leaf}"),
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "allOf": [
                        { "$ref": format!("gts://{family}") },
                        {
                            "type": "object",
                            "properties": {
                                "data": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["order_id"],
                                    "properties": {
                                        "order_id": { "type": "string", "format": "uuid" }
                                    }
                                }
                            }
                        }
                    ]
                }),
            ),
        ],
    )
    .await;

    let resolved = topic(&event_type).expect("an inherited topic trait must resolve");

    assert_eq!(resolved, GtsInstanceId::try_new(EXAMPLE_TOPIC).unwrap());
}

#[test]
fn topic_is_an_internal_error_when_no_trait_is_declared_anywhere_in_the_chain() {
    // `topic` is required by the base's trait schema, so the registry refuses
    // a type that resolves none.
    let event_type =
        unadmitted(json!({ "allowed_subject_types": [], "partition_key": "/tenant_id" }));

    let err = topic(&event_type)
        .expect_err("an event type with no topic trait names no stream and cannot be published to");

    assert!(matches!(err, DomainError::Internal(_)));
    assert_eq!(
        err.to_string(),
        "internal error: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         resolves to no `topic` trait value"
    );
}

#[test]
fn topic_is_an_internal_error_when_the_trait_names_a_type_rather_than_an_instance() {
    // A trailing `~` makes it a type id, and a topic is always an instance: it
    // carries the stream's own data, and nothing derives from it.
    let event_type = unadmitted(json!({
        "topic": "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1~",
        "allowed_subject_types": [],
        "partition_key": "/tenant_id",
    }));

    let err = topic(&event_type).expect_err("a topic trait must name a GTS instance");

    assert_eq!(
        err.to_string(),
        "internal error: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         names `gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1~` as its topic, \
         which is not a GTS instance id: \
         Invalid GTS identifier: gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1~: \
         GTS instance IDs must not end with '~' (a trailing '~' denotes a type id)"
    );
}

#[test]
fn an_event_type_listed_without_materialized_traits_is_internal() {
    let mut event_type = unadmitted(json!({ "topic": EXAMPLE_TOPIC }));
    event_type.effective_traits = None;

    let err = topic(&event_type).expect_err("a topic cannot be read from traits that are absent");

    assert!(matches!(err, DomainError::Internal(_)));
    assert_eq!(
        err.to_string(),
        "internal error: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         carries no materialized `effective_traits`"
    );
}

#[test]
fn an_event_type_listed_without_a_resolved_schema_is_internal() {
    let mut event_type = unadmitted(json!({
        "topic": EXAMPLE_TOPIC,
        "allowed_subject_types": [],
        "partition_key": "/tenant_id",
    }));
    event_type.resolved_schema = None;

    let err = validate_partition_key(&event_type)
        .expect_err("a pointer cannot be checked against a schema that is absent");

    assert!(matches!(err, DomainError::Internal(_)));
    assert_eq!(
        err.to_string(),
        "internal error: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         carries no materialized `resolved_schema`"
    );
}

// -- validate_partition_key ----------------------------------------------------

/// The base's own `data` member, so a pointer into the payload has a parent to
/// descend through.
async fn derived_with_payload(traits: &serde_json::Value) -> TypeSchema {
    listed(&[
        (BASE_ID, base()),
        (
            DERIVED_ID,
            json!({
                "$id": format!("gts://{DERIVED_ID}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "x-gts-traits": traits,
                "allOf": [
                    { "$ref": format!("gts://{BASE_ID}") },
                    {
                        "type": "object",
                        "properties": {
                            "data": {
                                "type": "object",
                                "properties": { "order_id": { "type": "string" } },
                            },
                        },
                    },
                ],
            }),
        ),
    ])
    .await
}

#[tokio::test]
async fn a_pointer_into_the_payload_is_accepted() {
    let event_type = derived_with_payload(&json!({
        "topic": EXAMPLE_TOPIC,
        "partition_key": "/data/order_id",
    }))
    .await;

    validate_partition_key(&event_type).expect("a pointer naming a declared payload member holds");
}

#[tokio::test]
async fn a_pointer_naming_a_base_member_is_accepted() {
    // The default. It names nothing the derived type declares itself, so this is
    // the case that proves inherited members count.
    let event_type = derived_with_payload(&json!({ "topic": EXAMPLE_TOPIC })).await;

    validate_partition_key(&event_type).expect("a pointer naming an inherited member holds");
}

#[tokio::test]
async fn a_pointer_naming_nothing_is_rejected_and_names_itself() {
    let event_type = derived_with_payload(&json!({
        "topic": EXAMPLE_TOPIC,
        "partition_key": "/data/no_such_member",
    }))
    .await;

    let err = validate_partition_key(&event_type)
        .expect_err("a pointer naming an undeclared member cannot be admitted");

    assert_eq!(
        err.to_string(),
        "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         declares partition key `/data/no_such_member`, which names no declared member \
         `no_such_member`"
    );
}

#[test]
fn a_value_that_is_not_a_pointer_is_rejected() {
    // The base's trait schema declares `partition_key` a JSON Pointer, so the
    // registry refuses a type whose value is not one.
    let event_type = unadmitted(json!({
        "topic": EXAMPLE_TOPIC,
        "allowed_subject_types": [],
        "partition_key": "tenant_id",
    }));

    let err = validate_partition_key(&event_type).expect_err("a bare field name is not a pointer");

    assert_eq!(
        err.to_string(),
        "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         declares partition key `tenant_id`, which is not a JSON Pointer into the event"
    );
}
