// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for the registered secret-type GTS seed schemas.

use crate::models::SharingMode;
use crate::types::SECRET_TYPE_CATALOG;

/// The registered base traits schema must stay free of `$ref`/`oneOf`
/// shapes (the GTS trait validator resolves neither — a `oneOf` enum
/// breaks types-registry boot validation) and its `allow_sharing` enum
/// must match the serde labels of [`SharingMode`].
#[test]
fn sharing_modes_schema_matches_enum() {
    let registered = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas parse");
    let want_id = serde_json::json!(format!("gts://{}", super::CREDENTIAL_RESOURCE_TYPE));
    let base = registered
        .iter()
        .find(|s| s["$id"] == want_id)
        .expect("secret base type registered");
    let traits_schema = &base["x-gts-traits-schema"];
    let rendered = traits_schema.to_string();
    assert!(
        !rendered.contains("$ref") && !rendered.contains("oneOf"),
        "traits schema must stay $ref/oneOf-free for the GTS validator: {rendered}"
    );
    let labels: Vec<serde_json::Value> = [
        SharingMode::Private,
        SharingMode::Tenant,
        SharingMode::Shared,
    ]
    .iter()
    .map(|m| serde_json::to_value(m).expect("serializes"))
    .collect();
    assert_eq!(
        traits_schema["properties"]["allow_sharing"]["items"]["enum"],
        serde_json::Value::Array(labels),
        "allow_sharing enum drifted from SharingMode's serde labels"
    );
}

/// The secret base type is a pure trait/schema carrier — secrets are always
/// typed by a derived id, never by the bare base — so its registered schema
/// must be marked abstract. It supplies no trait values: `allow_sharing` and
/// `max_size_bytes` are required with no default, so each non-abstract type
/// must declare them itself.
#[test]
fn secret_base_type_is_abstract_and_carries_no_trait_values() {
    let base = base_schema();
    assert_eq!(
        base["x-gts-abstract"],
        serde_json::json!(true),
        "secret base must be x-gts-abstract: true (pure trait carrier)"
    );
    let values = &base["x-gts-traits"];
    assert!(
        values.is_null() || values.as_object().is_some_and(serde_json::Map::is_empty),
        "base must supply no trait values: {values}"
    );
}

/// The generated traits schema lists both mandatory traits as required and
/// gives neither a default.
#[test]
fn traits_schema_requires_sharing_and_size_without_defaults() {
    let base = base_schema();
    let ts = &base["x-gts-traits-schema"];
    let required = ts["required"].as_array().expect("required array");
    for name in ["allow_sharing", "max_size_bytes"] {
        assert!(
            required.contains(&serde_json::json!(name)),
            "{name} required"
        );
        assert!(
            ts["properties"][name].get("default").is_none(),
            "{name} must have no default: {}",
            ts["properties"][name]
        );
    }
    for name in [
        "expirable",
        "utf8_only",
        "rotation_period_secs",
        "value_schema",
    ] {
        assert!(
            !required.contains(&serde_json::json!(name)),
            "{name} must stay optional"
        );
    }
}

fn base_schema() -> serde_json::Value {
    let registered = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas parse");
    let want_id = serde_json::json!(format!("gts://{}", super::CREDENTIAL_RESOURCE_TYPE));
    registered
        .into_iter()
        .find(|s| s["$id"] == want_id)
        .expect("secret base type registered")
}

/// A store holding the base type plus the given custom derived type.
fn store_with_custom(traits: Option<serde_json::Value>) -> (gts::GtsStore, String) {
    let mut store = gts::GtsStore::new();
    let registered = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas parse");
    for s in &registered {
        let id = s["$id"].as_str().expect("$id").trim_start_matches("gts://");
        if id.starts_with(super::CREDENTIAL_RESOURCE_TYPE) {
            store.register_schema(id, s).expect("register built-in");
        }
    }
    let custom_id = format!(
        "{}acme.apps.creds.custom.v1~",
        super::CREDENTIAL_RESOURCE_TYPE
    );
    let mut schema = serde_json::json!({
        "$id": format!("gts://{custom_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{"$ref": format!("gts://{}", super::CREDENTIAL_RESOURCE_TYPE)}],
    });
    if let Some(t) = traits {
        schema["x-gts-traits"] = t;
    }
    store
        .register_schema(&custom_id, &schema)
        .expect("register custom");
    (store, custom_id)
}

/// Every built-in type validates against the registry rules and resolves
/// both mandatory traits.
#[test]
fn every_builtin_type_validates_and_resolves_required_traits() {
    let (mut store, _) = store_with_custom(Some(serde_json::json!({
        "allow_sharing": ["private"], "max_size_bytes": 1
    })));
    for d in SECRET_TYPE_CATALOG {
        let resolved = store
            .validate_schema(d.gts_id)
            .unwrap_or_else(|e| panic!("{}: must validate: {e}", d.name));
        assert!(
            resolved.effective_traits.get("allow_sharing").is_some(),
            "{}",
            d.name
        );
        assert_eq!(
            resolved.effective_traits["max_size_bytes"],
            serde_json::json!(d.max_size_bytes),
            "{}",
            d.name
        );
    }
}

#[test]
fn custom_type_without_max_size_is_refused() {
    let (mut store, id) = store_with_custom(Some(serde_json::json!({"allow_sharing": ["tenant"]})));
    let err = store
        .validate_schema(&id)
        .expect_err("must be refused")
        .to_string();
    assert!(err.contains("required traits must be resolved"), "{err}");
}

#[test]
fn custom_type_without_allow_sharing_is_refused() {
    let (mut store, id) = store_with_custom(Some(serde_json::json!({"max_size_bytes": 1024})));
    let err = store
        .validate_schema(&id)
        .expect_err("must be refused")
        .to_string();
    assert!(err.contains("required traits must be resolved"), "{err}");
}

#[test]
fn custom_type_declaring_both_traits_validates() {
    let (mut store, id) = store_with_custom(Some(serde_json::json!({
        "allow_sharing": ["tenant"], "max_size_bytes": 1024
    })));
    store.validate_schema(&id).expect("must validate");
}

/// Every catalog entry must be backed by a registered GTS type schema
/// whose `x-gts-traits` mirror the catalog descriptor — a divergence
/// between the compiled-in traits and the registered types trips here.
#[test]
fn every_catalog_type_has_a_registered_schema_with_matching_traits() {
    let registered = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas parse");
    for d in SECRET_TYPE_CATALOG {
        let want_id = serde_json::json!(format!("gts://{}", d.gts_id));
        let schema = registered
            .iter()
            .find(|s| s["$id"] == want_id)
            .unwrap_or_else(|| panic!("no registered schema for {}", d.name));
        let traits = &schema["x-gts-traits"];
        assert_eq!(traits["expirable"], serde_json::json!(d.expirable));
        assert_eq!(traits["utf8_only"], serde_json::json!(d.utf8_only));
        assert_eq!(
            traits["allow_sharing"].as_array().map(Vec::len),
            Some(d.allow_sharing.len())
        );
        match d.value_schema {
            Some(src) => {
                let want: serde_json::Value =
                    serde_json::from_str(src).expect("catalog value_schema parses");
                assert_eq!(
                    traits["value_schema"], want,
                    "{}: value_schema trait",
                    d.name
                );
            }
            None => assert!(
                traits.get("value_schema").is_none(),
                "{}: unexpected value_schema trait",
                d.name
            ),
        }
    }
}

/// The registered trait values must deserialize into the runtime traits
/// carrier — this is exactly what the gear's registry-driven resolver
/// does with `effective_traits()`, so a shape drift trips here first.
#[test]
fn registered_traits_deserialize_into_secret_type_traits() {
    let registered = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas parse");
    for d in SECRET_TYPE_CATALOG {
        let want_id = serde_json::json!(format!("gts://{}", d.gts_id));
        let schema = registered
            .iter()
            .find(|s| s["$id"] == want_id)
            .unwrap_or_else(|| panic!("no registered schema for {}", d.name));
        let traits: super::SecretTypeTraits =
            serde_json::from_value(schema["x-gts-traits"].clone())
                .unwrap_or_else(|e| panic!("{}: traits must deserialize: {e}", d.name));
        for mode in d.allow_sharing {
            assert!(traits.allows_sharing(*mode), "{}: {mode:?}", d.name);
        }
        assert_eq!(traits.expirable, d.expirable, "{}", d.name);
        assert_eq!(traits.utf8_only, d.utf8_only, "{}", d.name);
        assert_eq!(
            traits.max_size_bytes,
            u64::try_from(d.max_size_bytes).expect("catalog size fits u64"),
            "{}",
            d.name
        );
        assert_eq!(traits.value_schema.is_some(), d.value_schema.is_some());
    }
}
