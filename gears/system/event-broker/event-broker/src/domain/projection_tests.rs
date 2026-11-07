//! Tests for [`crate::domain::projection`], the single place a resolved GTS type
//! schema becomes one of the broker's API shapes.
//!
//! Fixtures are built the way production builds them: the bases are the schemas
//! the broker's own declarations emit into the GTS inventory, so the event
//! base's `x-gts-traits-schema` - and therefore trait defaulting - is exercised
//! rather than bypassed, and each fixture is listed through
//! `PlatformTypesRegistryApi` from a registry that resolves it. A fixture the
//! registry would refuse to admit is built as a `TypeSchema` literal instead.

use gts::{GtsIdPattern, GtsTypeId};
use serde_json::json;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{Instance, ListEntitiesRequest, PlatformTypesRegistryApiExt, TypeSchema};
use uuid::Uuid;

use crate::api::rest::handlers::ingest::dto::EventTypeDto;
use crate::domain::error::DomainError;
use crate::domain::projection;

const TOPIC_BASE: &str = "gts.cf.core.events.topic.v1~";
const EVENT_BASE: &str = "gts.cf.core.events.event.v1~";

const AUDIT_TOPIC: &str = "gts.cf.core.events.topic.v1~example.proj.broker.audit.v1";
const NOTIFY_TOPIC: &str = "gts.cf.core.events.topic.v1~example.proj.broker.notify.v1";

const ORDER_TYPE: &str = "gts.cf.core.events.event.v1~example.proj.shop.order.v1~";
const ORDER_EU_TYPE: &str =
    "gts.cf.core.events.event.v1~example.proj.shop.order.v1~example.proj.shop.order_eu.v1~";

/// The base event's `data` member as `data_contract` composes it into the first
/// branch of a payload contract; spelled out so that changing the base surfaces
/// as a failure here.
fn base_data_member() -> serde_json::Value {
    json!({ "type": ["object", "null"] })
}

// -- Fixture construction ------------------------------------------------------

/// A registry holding the inventory bases and `documents`, keyed by id.
fn registry(documents: &[(&str, serde_json::Value)]) -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed_inventory();
    for (id, document) in documents {
        registry.seed(id, document.clone());
    }
    registry
}

/// `type_id` as `list_type_schemas` reports it from a registry holding the
/// inventory bases and `documents`.
async fn listed_event_type(type_id: &str, documents: &[(&str, serde_json::Value)]) -> TypeSchema {
    let listed = registry(documents)
        .list_type_schemas(
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            ListEntitiesRequest::matching(
                GtsIdPattern::try_new(&format!("{EVENT_BASE}*")).expect("a valid pattern"),
            ),
        )
        .await
        .expect("the registry resolves every fixture");
    let matching: Vec<&TypeSchema> = listed
        .iter()
        .filter(|schema| schema.type_id.as_ref() == type_id)
        .collect();
    let [schema] = matching.as_slice() else {
        panic!("expected one listed {type_id}, got {matching:?}");
    };
    (*schema).clone()
}

/// An event type the registry would refuse to admit, as it would reach the
/// broker had it been admitted: `effective_traits` is what resolution would
/// have merged, and the resolved schema is the event base with no narrowing.
fn unadmitted_event_type(type_id: &str, effective_traits: serde_json::Value) -> TypeSchema {
    TypeSchema {
        type_id: GtsTypeId::new(type_id),
        type_uuid: Uuid::nil(),
        lifecycle_status: types_registry_sdk::LifecycleStatus::Active,
        origin: None,
        content: Some(body(type_id, EVENT_BASE, &effective_traits)),
        resolved_schema: Some(json!({
            "type": "object",
            "allOf": [{ "type": "object", "properties": { "data": base_data_member() } }],
        })),
        effective_traits: Some(effective_traits),
        effective_traits_schema: None,
        provenance: None,
    }
}

/// A derived schema body: the trait values it fixes, plus a `$ref` to the type
/// it chains under. The shape both `types-registry` and the mock provision.
fn body(type_id: &str, parent_type_id: &str, traits: &serde_json::Value) -> serde_json::Value {
    json!({
        "$id": format!("gts://{type_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": traits,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{parent_type_id}") }],
    })
}

/// A registered topic: the instance document as `list_instances` reports it.
async fn topic_instance(document: serde_json::Value) -> Instance {
    let id = document["id"]
        .as_str()
        .expect("fixture document carries an id")
        .to_owned();
    let listed = registry(&[(&id, document)])
        .list_instances(
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            ListEntitiesRequest::matching(
                GtsIdPattern::try_new(&format!("{TOPIC_BASE}*")).expect("a valid pattern"),
            ),
        )
        .await
        .expect("the registry lists topics");
    let [instance] = listed.as_slice() else {
        panic!("expected one listed topic, got {listed:?}");
    };
    assert_eq!(instance.id.as_ref(), id);
    instance.clone()
}

fn internal_detail(err: DomainError) -> String {
    match err {
        DomainError::Internal(detail) => detail,
        other => panic!("expected Internal, got {other:?}"),
    }
}

// -- topic ---------------------------------------------------------------------

#[tokio::test]
async fn topic_projects_a_document_that_declares_no_retention() {
    // `retention` is optional on the base, so a topic that declares none projects
    // with it absent - the broker's configured default applies instead.
    let instance = topic_instance(json!({
        "id": AUDIT_TOPIC,
        "description": "Audit stream.",
    }))
    .await;

    let topic = projection::topic(&instance).unwrap();
    assert_eq!(topic.id.as_ref(), AUDIT_TOPIC);
    assert_eq!(topic.description, "Audit stream.");
    assert_eq!(topic.retention, None);
}

#[tokio::test]
async fn topic_projects_a_declared_retention_in_canonical_units() {
    // Authored with a day component on purpose: the projection parses the value
    // into a duration, so the reported form below is canonical hours.
    let instance = topic_instance(json!({
        "id": NOTIFY_TOPIC,
        "description": "Notification stream.",
        "retention": "P7D",
    }))
    .await;

    let topic = projection::topic(&instance).unwrap();
    assert_eq!(topic.id.as_ref(), NOTIFY_TOPIC);
    assert_eq!(topic.description, "Notification stream.");
    assert_eq!(
        topic.retention.map(|r| r.to_string()).as_deref(),
        Some("PT168H")
    );
}

#[tokio::test]
async fn topic_without_a_description_is_internal() {
    // The base requires `description`, so a document without one is a
    // registration `types-registry` should not have admitted.
    let instance = topic_instance(json!({ "id": AUDIT_TOPIC })).await;

    assert_eq!(
        internal_detail(projection::topic(&instance).unwrap_err()),
        "gts.cf.core.events.topic.v1~example.proj.broker.audit.v1 carries no string \
         `description`"
    );
}

#[tokio::test]
async fn topic_with_an_unparseable_retention_is_internal() {
    let instance = topic_instance(json!({
        "id": AUDIT_TOPIC,
        "description": "Audit stream.",
        "retention": "7 days",
    }))
    .await;

    assert!(
        internal_detail(projection::topic(&instance).unwrap_err()).starts_with(
            "gts.cf.core.events.topic.v1~example.proj.broker.audit.v1 has a `retention` that is \
             not an ISO 8601 duration"
        )
    );
}

// A topic resolves nothing along an inheritance chain: it is an instance, and
// an instance cannot derive from an instance. Trait inheritance is covered by
// `event_type_inherits_traits_from_an_ancestor`, which exercises the same
// chain-merge on the shape that has one.

// -- event_type ----------------------------------------------------------------

#[tokio::test]
async fn event_type_defaults_allowed_subject_types_to_empty() {
    // No level of the chain declares the trait, so the base's default stands.
    let schema = listed_event_type(
        ORDER_TYPE,
        &[(
            ORDER_TYPE,
            body(ORDER_TYPE, EVENT_BASE, &json!({ "topic": AUDIT_TOPIC })),
        )],
    )
    .await;

    assert_eq!(
        serde_json::to_value(EventTypeDto::from(projection::event_type(&schema).unwrap())).unwrap(),
        json!({
            "id": ORDER_TYPE,
            "topic": AUDIT_TOPIC,
            "partition_key": "/tenant_id",
            "description": null,
            "allowed_subject_types": [],
            // The type narrows nothing, so the base's `data` member is the whole
            // payload contract.
            "data_schema": { "allOf": [base_data_member()] },
        })
    );
}

#[tokio::test]
async fn event_type_inherits_traits_from_an_ancestor() {
    // The leaf re-anchors the type to another topic but says nothing about
    // subject types, so the parent's list resolves.
    let parent = body(
        ORDER_TYPE,
        EVENT_BASE,
        &json!({
            "topic": AUDIT_TOPIC,
            "allowed_subject_types": ["gts.cf.core.events.subject.v1~example.proj.shop.order.v1~"],
        }),
    );
    let schema = listed_event_type(
        ORDER_EU_TYPE,
        &[
            (ORDER_TYPE, parent),
            (
                ORDER_EU_TYPE,
                body(ORDER_EU_TYPE, ORDER_TYPE, &json!({ "topic": NOTIFY_TOPIC })),
            ),
        ],
    )
    .await;

    assert_eq!(
        serde_json::to_value(EventTypeDto::from(projection::event_type(&schema).unwrap())).unwrap(),
        json!({
            "id": ORDER_EU_TYPE,
            "topic": NOTIFY_TOPIC,
            "partition_key": "/tenant_id",
            "description": null,
            "allowed_subject_types": [
                "gts.cf.core.events.subject.v1~example.proj.shop.order.v1~",
            ],
            // Neither level narrows `data`, so the base's own member is the whole
            // payload contract - reached through two levels of resolved nesting.
            "data_schema": { "allOf": [base_data_member()] },
        })
    );
}

#[tokio::test]
async fn event_type_composes_the_data_narrowings_with_the_leaf_last() {
    let schema = listed_event_type(
        ORDER_TYPE,
        &[(
            ORDER_TYPE,
            json!({
                "$id": format!("gts://{ORDER_TYPE}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "x-gts-traits": { "topic": AUDIT_TOPIC },
                "type": "object",
                "allOf": [
                    { "$ref": format!("gts://{EVENT_BASE}") },
                    {
                        "type": "object",
                        "properties": {
                            "data": { "type": "object", "required": ["order_id"] },
                        },
                    },
                ],
            }),
        )],
    )
    .await;

    assert_eq!(
        serde_json::to_value(EventTypeDto::from(projection::event_type(&schema).unwrap())).unwrap(),
        json!({
            "id": ORDER_TYPE,
            "topic": AUDIT_TOPIC,
            "partition_key": "/tenant_id",
            "description": null,
            "allowed_subject_types": [],
            "data_schema": {
                "allOf": [
                    base_data_member(),
                    { "type": "object", "required": ["order_id"] },
                ],
            },
        })
    );
}

#[test]
fn event_type_without_a_topic_trait_is_internal() {
    // `topic` is required by the base's trait schema, so the registry refuses
    // a type that resolves none.
    let schema = unadmitted_event_type(
        ORDER_TYPE,
        json!({ "allowed_subject_types": [], "partition_key": "/tenant_id" }),
    );

    assert_eq!(
        internal_detail(projection::event_type(&schema).unwrap_err()),
        "gts.cf.core.events.event.v1~example.proj.shop.order.v1~ resolves to no `topic` trait value"
    );
}

#[test]
fn event_type_naming_a_type_shaped_topic_is_internal() {
    // A topic is an instance, so an id with the trailing `~` names a type and
    // cannot be the stream an event type publishes to.
    let type_shaped = "gts.cf.core.events.topic.v1~example.proj.broker.audit.v1~";
    // The base narrows `topic` to an instance of the topic base type, so the
    // registry refuses this type.
    let schema = unadmitted_event_type(
        ORDER_TYPE,
        json!({
            "topic": type_shaped,
            "allowed_subject_types": [],
            "partition_key": "/tenant_id",
        }),
    );

    assert_eq!(
        internal_detail(projection::event_type(&schema).unwrap_err()),
        "gts.cf.core.events.event.v1~example.proj.shop.order.v1~ names \
         `gts.cf.core.events.topic.v1~example.proj.broker.audit.v1~` as its topic, which is not a \
         GTS instance id: Invalid GTS identifier: \
         gts.cf.core.events.topic.v1~example.proj.broker.audit.v1~: \
         GTS instance IDs must not end with '~' (a trailing '~' denotes a type id)"
    );
}
