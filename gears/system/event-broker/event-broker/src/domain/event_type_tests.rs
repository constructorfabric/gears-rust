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
/// The committed worked example under `docs/schemas/examples/`.
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
            "subject_tenant": { "type": "string" },
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
                    "default": "/subject_tenant"
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
        ownership: types_registry_sdk::Ownership::Global,
        origin: None,
        content: None,
        resolved_schema: Some(json!({
            "type": "object",
            "allOf": [
                { "type": "object", "properties": { "subject_tenant": { "type": "string" } } },
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
        unadmitted(json!({ "allowed_subject_types": [], "partition_key": "/subject_tenant" }));

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
        "partition_key": "/subject_tenant",
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
        "partition_key": "/subject_tenant",
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

#[tokio::test]
async fn a_type_naming_the_old_tenant_pointer_is_excluded_at_spec_load() {
    // The base names the event's tenant `subject_tenant`, so `/tenant_id` names
    // no declared member and is refused like any other such pointer.
    let event_type = derived_with_payload(&json!({
        "topic": EXAMPLE_TOPIC,
        "partition_key": "/tenant_id",
    }))
    .await;

    let err = validate_partition_key(&event_type)
        .expect_err("a pointer naming the old tenant member cannot be admitted");

    assert_eq!(
        err.to_string(),
        "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         declares partition key `/tenant_id`, which names no declared member `tenant_id`"
    );
}

#[test]
fn a_value_that_is_not_a_pointer_is_rejected() {
    // The base's trait schema declares `partition_key` a JSON Pointer, so the
    // registry refuses a type whose value is not one.
    let event_type = unadmitted(json!({
        "topic": EXAMPLE_TOPIC,
        "allowed_subject_types": [],
        "partition_key": "subject_tenant",
    }));

    let err = validate_partition_key(&event_type).expect_err("a bare field name is not a pointer");

    assert_eq!(
        err.to_string(),
        "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         declares partition key `subject_tenant`, which is not a JSON Pointer into the event"
    );
}

// -- the subject-owner partition key ------------------------------------------

/// The base as `base()` declares it, plus the owner member the real base declares
/// through the GTS `Option` convention: required, and `null` allowed.
fn base_declaring_the_owner() -> serde_json::Value {
    let mut base = base();
    base["properties"]["subject_owner"] = json!({ "type": ["string", "null"], "format": "uuid" });
    base["required"] = json!(["subject_tenant", "subject_owner"]);
    base
}

/// A branch that narrows the owner to a required UUID string.
fn owner_override() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["subject_owner"],
        "properties": { "subject_owner": { "type": "string", "format": "uuid" } }
    })
}

/// `DERIVED_ID` keyed on `/subject_owner`, its own `allOf` holding `branches`
/// after the base reference.
async fn keyed_on_the_owner(branches: &[serde_json::Value]) -> TypeSchema {
    let mut all_of = vec![json!({ "$ref": format!("gts://{BASE_ID}") })];
    all_of.extend(branches.iter().cloned());
    listed(&[
        (BASE_ID, base_declaring_the_owner()),
        (
            DERIVED_ID,
            json!({
                "$id": format!("gts://{DERIVED_ID}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "x-gts-traits": { "topic": EXAMPLE_TOPIC, "partition_key": "/subject_owner" },
                "allOf": all_of,
            }),
        ),
    ])
    .await
}

/// The rejection a type keyed on `/subject_owner` gets without an override.
const NO_OWNER_OVERRIDE: &str = "InvalidSpec: \
    gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ declares partition key \
    `/subject_owner`, which needs a branch of the type that lists `subject_owner` in \
    `required` and declares it as type `string`";

/// Resolution keeps `allOf` as it is: the inlined base keeps its nullable,
/// required owner and the derived branch its own narrowing, side by side. The
/// override check relies on finding that branch, not a merged `required`.
#[tokio::test]
async fn resolution_keeps_the_owner_override_beside_the_base_under_all_of() {
    let event_type = keyed_on_the_owner(&[owner_override()]).await;

    let resolved = event_type
        .resolved_schema
        .as_ref()
        .expect("a resolved schema");

    assert_eq!(
        resolved["allOf"],
        json!([
            {
                "type": "object",
                "properties": {
                    "data": { "type": ["object", "null"] },
                    "subject_owner": { "type": ["string", "null"], "format": "uuid" },
                    "subject_tenant": { "type": "string" }
                },
                "required": ["subject_tenant", "subject_owner"]
            },
            {
                "type": "object",
                "required": ["subject_owner"],
                "properties": { "subject_owner": { "type": "string", "format": "uuid" } }
            }
        ])
    );
}

#[tokio::test]
async fn an_owner_key_with_an_override_on_the_type_is_accepted() {
    let event_type = keyed_on_the_owner(&[owner_override()]).await;

    validate_partition_key(&event_type).expect("the type narrows the owner to required");
}

#[tokio::test]
async fn an_owner_key_with_the_override_on_an_intermediate_type_is_accepted() {
    // The family narrows the owner, and the leaf declares nothing about it, so
    // the override sits one `allOf` deeper than the leaf's own branches.
    let family = "gts.cf.core.events.event.v1~fabrikam.shop.orders.family.v1~";
    let leaf = "gts.cf.core.events.event.v1~fabrikam.shop.orders.family.v1~\
                fabrikam.shop.orders.order_placed.v1~";
    let event_type = listed_as(
        leaf,
        &[
            (BASE_ID, base_declaring_the_owner()),
            (
                family,
                json!({
                    "$id": format!("gts://{family}"),
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "x-gts-traits": { "topic": EXAMPLE_TOPIC, "partition_key": "/subject_owner" },
                    "allOf": [{ "$ref": format!("gts://{BASE_ID}") }, owner_override()]
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
                            "properties": { "data": { "type": "object" } }
                        }
                    ]
                }),
            ),
        ],
    )
    .await;

    validate_partition_key(&event_type).expect("an inherited override counts");
}

#[tokio::test]
async fn an_owner_key_without_an_override_is_rejected() {
    // The base lists the owner in `required` but admits `null`, so it is no
    // override.
    let event_type = keyed_on_the_owner(&[]).await;

    let err =
        validate_partition_key(&event_type).expect_err("a nullable owner cannot key a partition");

    assert_eq!(err.to_string(), NO_OWNER_OVERRIDE);
}

#[tokio::test]
async fn an_owner_override_that_keeps_null_or_is_not_required_is_rejected() {
    let mut nullable = owner_override();
    nullable["properties"]["subject_owner"]["type"] = json!(["string", "null"]);
    let mut not_required = owner_override();
    not_required["required"] = json!([]);

    let mut refusals = Vec::new();
    for branch in [nullable, not_required] {
        let event_type = keyed_on_the_owner(&[branch]).await;
        refusals.push(validate_partition_key(&event_type).map_err(|err| err.to_string()));
    }

    assert_eq!(
        refusals,
        vec![
            Err(NO_OWNER_OVERRIDE.to_owned()),
            Err(NO_OWNER_OVERRIDE.to_owned()),
        ]
    );
}

#[tokio::test]
async fn an_owner_override_of_another_type_is_rejected() {
    // The registry refuses a branch that retypes the base's owner, so this is
    // the type as it would reach the broker had it been admitted.
    let mut event_type = keyed_on_the_owner(&[owner_override()]).await;
    event_type
        .resolved_schema
        .as_mut()
        .expect("a resolved schema")["allOf"][1]["properties"]["subject_owner"]["type"] =
        json!("integer");

    let err =
        validate_partition_key(&event_type).expect_err("an integer owner is not a principal id");

    assert_eq!(err.to_string(), NO_OWNER_OVERRIDE);
}

#[tokio::test]
async fn a_pointer_at_an_object_or_array_member_is_rejected() {
    let mut refusals = Vec::new();
    for member in ["/data/lines", "/data/customer"] {
        let event_type = listed(&[
            (BASE_ID, base()),
            (
                DERIVED_ID,
                json!({
                    "$id": format!("gts://{DERIVED_ID}"),
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "x-gts-traits": { "topic": EXAMPLE_TOPIC, "partition_key": member },
                    "allOf": [
                        { "$ref": format!("gts://{BASE_ID}") },
                        {
                            "type": "object",
                            "properties": {
                                "data": {
                                    "type": "object",
                                    "properties": {
                                        "lines": { "type": "array", "items": { "type": "string" } },
                                        "customer": { "type": ["object", "null"] }
                                    }
                                }
                            }
                        }
                    ]
                }),
            ),
        ])
        .await;
        refusals.push(validate_partition_key(&event_type).map_err(|err| err.to_string()));
    }

    assert_eq!(
        refusals,
        vec![
            Err(
                "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
                 declares partition key `/data/lines`, which names a member declared as an \
                 object or array, which has no stable hash input"
                    .to_owned()
            ),
            Err(
                "InvalidSpec: gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
                 declares partition key `/data/customer`, which names a member declared as an \
                 object or array, which has no stable hash input"
                    .to_owned()
            ),
        ]
    );
}
