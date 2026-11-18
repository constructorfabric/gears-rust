//! Tests for the GTS resource-type vocabulary.

use super::{CATEGORY_SCHEMA, DECLARATION_SCHEMA, Resource, VALUE_SCHEMA};

#[test]
fn every_constant_round_trips_through_the_typed_view() {
    for wire in [DECLARATION_SCHEMA, VALUE_SCHEMA, CATEGORY_SCHEMA] {
        let typed = Resource::from_wire(wire);
        assert_eq!(
            typed.as_wire(),
            wire,
            "`{wire}` must survive a round trip through the typed view"
        );
        assert!(
            !matches!(typed, Resource::Unknown(_)),
            "`{wire}` is a modelled resource and must not fall through to Unknown"
        );
    }
}

#[test]
fn an_unmodelled_resource_is_preserved_not_discarded() {
    // A resource type this SDK does not know must survive intact, so a consumer
    // can still report it and a later version can model it without data loss.
    let typed = Resource::from_wire("gts.cf.core.settings.change_set.v1~");
    assert_eq!(
        typed,
        Resource::Unknown("gts.cf.core.settings.change_set.v1~".to_owned())
    );
    assert_eq!(typed.as_wire(), "gts.cf.core.settings.change_set.v1~");
}

#[test]
fn declaration_and_value_are_different_resources() {
    // The projection leans on this distinction to tell "no such setting" from
    // "no credential configured"; if these ever collapsed, that would break.
    assert_ne!(
        Resource::from_wire(DECLARATION_SCHEMA),
        Resource::from_wire(VALUE_SCHEMA)
    );
}

#[test]
fn the_base_schema_is_identified_by_the_wire_constant() {
    // Two spellings of one identifier: the constant callers read off policies
    // and audit records, and the `$id` the registry holds. If they drift, keys
    // parse against a base that is not the one registered.
    use super::{SETTING_TYPE_BASE, setting_type_base_schema};
    let schema = setting_type_base_schema();
    assert_eq!(
        schema["$id"],
        serde_json::json!(format!("gts://{SETTING_TYPE_BASE}"))
    );
    assert_eq!(schema["x-gts-abstract"], serde_json::json!(true));
    assert_eq!(
        schema["properties"]["payload"],
        serde_json::json!({}),
        "a derived type may narrow the payload to any shape"
    );
    assert!(
        schema.get("default").is_none(),
        "the Schema Default lives on the declaration, never in the type"
    );
}

#[test]
fn the_base_is_submitted_to_the_inventory() {
    // What an embedded types registry drains at its init (the production path is
    // the crate collector, checked below). Absent from the inventory, no
    // declaration could ever register a derived type.
    use super::SETTING_TYPE_BASE;
    let wanted = serde_json::json!(format!("gts://{SETTING_TYPE_BASE}"));
    assert!(
        toolkit_gts::all_inventory_type_schemas()
            .expect("inventory renders")
            .into_iter()
            .any(|schema| schema["$id"] == wanted),
        "the setting base is in the link-time inventory"
    );
}

/// The inventory entry registered under `type_id`, rendered.
fn inventory_schema(type_id: &str) -> Option<serde_json::Value> {
    let wanted = serde_json::json!(format!("gts://{type_id}"));
    toolkit_gts::all_inventory_type_schemas()
        .expect("inventory renders")
        .into_iter()
        .find(|schema| schema["$id"] == wanted)
}

/// The resource types with the function each stub is published from.
fn resource_stubs() -> [(&'static str, serde_json::Value); 3] {
    [
        (CATEGORY_SCHEMA, super::category_schema()),
        (DECLARATION_SCHEMA, super::declaration_schema()),
        (VALUE_SCHEMA, super::value_schema()),
    ]
}

#[test]
fn every_resource_type_is_submitted_under_its_constant() {
    // The permission catalogue names these as `resource_type`, and the platform
    // role-definition validator resolves a rule's target type through the types
    // registry: a resource type that never reached it cannot be named by a role.
    // One document, two ways to reach it: what a consumer reads from the SDK
    // and what the gear publishes must be the same stub.
    for (type_id, stub) in resource_stubs() {
        assert_eq!(
            stub["$id"],
            serde_json::json!(format!("gts://{type_id}")),
            "{type_id}: the `$id` is the registered identifier"
        );
        assert_eq!(
            inventory_schema(type_id).as_ref(),
            Some(&stub),
            "`{type_id}` is in the link-time inventory as the stub"
        );
    }
    assert_eq!(CATEGORY_SCHEMA, "gts.cf.core.settings.category.v1~");
    assert_eq!(DECLARATION_SCHEMA, "gts.cf.core.settings.declaration.v1~");
    assert_eq!(VALUE_SCHEMA, "gts.cf.core.settings.value.v1~");
}

#[test]
fn every_resource_type_stub_is_a_valid_draft_07_schema() {
    for (type_id, stub) in resource_stubs() {
        assert_eq!(
            stub["$schema"], "http://json-schema.org/draft-07/schema#",
            "{type_id}: the gear's schemas are draft-07"
        );
        assert!(
            jsonschema::draft7::meta::is_valid(&stub),
            "{type_id} is not a valid draft-07 schema"
        );
        assert!(
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft7)
                .build(&stub)
                .is_ok(),
            "{type_id} compiles as draft-07"
        );
        for annotation in ["title", "description"] {
            assert!(
                stub[annotation]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
                "{type_id} carries a {annotation}"
            );
        }
    }
}

#[test]
fn every_resource_type_stub_constrains_nothing() {
    // This is what makes a rollout and a rollback safe. The registry admits a
    // new revision of a published type only when it accepts everything the
    // published one accepts, and it orders no publishers: a body that tracked
    // the gear's representations would differ between two builds, and the older
    // build would be refused at publication and would not start. A body that
    // states no property is the same document in every build, so publishing it
    // again is always a repeat. Adding any of the keywords below ends that.
    for (type_id, stub) in resource_stubs() {
        assert_eq!(stub["type"], "object", "{type_id}");
        let keys: Vec<&str> = stub
            .as_object()
            .expect("a schema object")
            .keys()
            .map(String::as_str)
            .collect();
        for keyword in ["properties", "required", "additionalProperties"] {
            assert!(
                !keys.contains(&keyword),
                "{type_id}: a stub states no `{keyword}`"
            );
        }
        let mut sorted = keys;
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            ["$id", "$schema", "description", "title", "type"],
            "{type_id}: nothing but the identifier, the dialect, two annotations and the type"
        );
    }
}

/// What the types registry is handed for this crate: the crate's own collector,
/// which the gear's publication merges into its manifest.
fn collector_ids() -> Vec<String> {
    crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn the_crate_collector_holds_every_declaration() {
    // The gear publishes these through `gts_publication(...)`: a declaration
    // that reached only the process-wide inventory would not be published.
    let ids = collector_ids();
    for wanted in [
        super::SETTING_TYPE_BASE,
        DECLARATION_SCHEMA,
        VALUE_SCHEMA,
        CATEGORY_SCHEMA,
    ] {
        assert!(ids.iter().any(|id| id == wanted), "`{wanted}` is published");
    }
    for value_type in crate::catalogue::CATALOGUE {
        assert!(
            ids.iter().any(|id| id == value_type.id),
            "`{}` is published",
            value_type.id
        );
    }
}

#[test]
fn the_crate_collector_holds_nothing_else() {
    // The setting base, the resource types and the value-type catalogue:
    // a stray entry would be published under this gear's name.
    assert_eq!(
        collector_ids().len(),
        4 + crate::catalogue::CATALOGUE.len(),
        "{:?}",
        collector_ids()
    );
}

#[test]
fn every_settings_schema_in_the_inventory_is_in_the_collector() {
    // The gear publishes the collector, not the process-wide inventory: a
    // schema of this namespace that was submitted only the plain way would
    // reach an embedded registry and never a remote one.
    let ids = collector_ids();
    for entry in toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema> {
        if entry.type_id.starts_with("gts.cf.core.settings.") {
            assert!(
                ids.iter().any(|id| id == entry.type_id),
                "`{}` is in the process-wide inventory but not in the crate collector",
                entry.type_id
            );
        }
    }
}
