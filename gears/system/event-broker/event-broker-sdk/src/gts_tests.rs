//! Behaviour of the base schemas declared in [`crate::gts`], validated through
//! the `gts` library as `types-registry` validates them. The worked examples of
//! a Rust-declared event type and a topic instance are in
//! `tests/worked_examples.rs`.

use gts::GtsSchema;

use crate::gts::{EventV1, TopicV1};

fn emitted(schema: &str) -> serde_json::Value {
    serde_json::from_str(schema).expect("emitted GTS schema is valid JSON")
}

#[test]
fn only_the_two_base_types_are_declared() {
    // Inventory iteration order is link order, not declaration order.
    let mut ids: Vec<&str> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        // Base types only: a test may declare derived event types of its own.
        .filter(|id| id.contains(".events.") && id.matches('~').count() == 1)
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![
            toolkit_gts::gts_id!("cf.core.events.event.v1~"),
            toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        ],
        "the broker declares exactly the topic and event base types"
    );
}

#[test]
fn generated_base_schemas_validate_with_gts() {
    let mut store = gts::GtsStore::new();
    for (id, schema) in [
        (
            TopicV1::TYPE_ID,
            emitted(&TopicV1::gts_schema_with_refs_as_string()),
        ),
        (
            EventV1::<()>::TYPE_ID,
            emitted(&EventV1::<()>::gts_schema_with_refs_as_string()),
        ),
    ] {
        store
            .register_schema(id, &schema)
            .expect("register base schema");
    }
    for id in [TopicV1::TYPE_ID, EventV1::<()>::TYPE_ID] {
        store
            .validate_schema_with(id, gts::GtsRefValidation::None)
            .expect("generated base schema must use valid GTS keywords");
    }
}

/// The event type the subject-pattern cases vary: a Rust declaration whose
/// generated document each case re-registers with one `allowed_subject_types`
/// pattern, since the pattern under test is the only thing that changes.
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~"),
    description = "Subject-pattern admission probe.",
    properties = "",
    traits = serde_json::json!({
        "topic": toolkit_gts::gts_id!("cf.core.events.topic.v1~fabrikam.shop._.orders.v1"),
    }),
)]
struct SubjectPatternProbeV1;

fn admit_event_with_subject_pattern(pattern: &str) -> Result<gts::ResolvedType, gts::StoreError> {
    let mut store = gts::GtsStore::new();
    store
        .register_schema(
            EventV1::<()>::TYPE_ID,
            &EventV1::<()>::gts_schema_with_refs(),
        )
        .unwrap();
    let mut derived = SubjectPatternProbeV1::gts_schema_with_refs();
    derived["x-gts-traits"]["allowed_subject_types"] = serde_json::json!([pattern]);
    store
        .register_schema(SubjectPatternProbeV1::TYPE_ID, &derived)
        .unwrap();
    store.validate_schema_with(SubjectPatternProbeV1::TYPE_ID, gts::GtsRefValidation::None)
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

#[test]
fn event_identifier_fields_accept_concrete_ids_but_reject_wildcards() {
    let schema = emitted(&EventV1::<()>::gts_schema_with_refs_as_string());
    let validator = gts::XGtsRefValidator::new();
    for field in ["type", "subject_type"] {
        // Keeps the base's `$id`, which a `/$id` constraint resolves to.
        let constraint = serde_json::json!({
            "$id": schema["$id"],
            "properties": { field: schema["properties"][field] },
        });
        let id = serde_json::json!({
            field: "gts.cf.core.events.event.v1~fabrikam.shop.orders.placed.v1~"
        });
        assert!(validator.validate_instance(&id, &constraint, "").is_empty());
        let wildcard =
            serde_json::json!({ field: "gts.cf.core.events.event.v1~fabrikam.shop.orders.*" });
        assert!(
            !validator
                .validate_instance(&wildcard, &constraint, "")
                .is_empty(),
            "{field} is an identifier, not a wildcard selector"
        );
    }
}
