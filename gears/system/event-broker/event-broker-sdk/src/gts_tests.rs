//! Generates `docs/schemas/` from the declarations in [`crate::gts`], and holds
//! the worked examples to them.
//!
//! The same function produces a committed file and the schema registered into
//! `types-registry`, so a hand-edited file would document a contract the broker
//! does not register. Regenerate rather than edit:
//!
//! ```text
//! GTS_REGEN=1 cargo test -p cf-gears-event-broker-sdk --lib gts_tests -- --ignored
//! ```
//!
//! The two byte-comparison tests are `#[ignore]`d, hence `--ignored`: the emitted
//! formatting belongs to the gts library and changes with it, so comparing bytes
//! gates on a third party's output. Drop the `GTS_REGEN=1` to compare instead of
//! rewrite. The contract stays covered by the conformance tests below, which
//! validate behaviour against the freshly emitted base.

use gts::GtsSchema;

use crate::gts::{EventV1, TopicV1};

/// Path of a gear-committed schema file, relative to the crate root.
fn schema_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/schemas")
        .join(file)
}

fn emitted(schema: &str) -> serde_json::Value {
    serde_json::from_str(schema).expect("emitted GTS schema is valid JSON")
}

fn committed(file: &str) -> serde_json::Value {
    let path = schema_path(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("committed schema is valid JSON")
}

/// Asserts the committed document at `file` equals `generated`, or rewrites it
/// when `GTS_REGEN` is set. One switch for every document, so a regeneration can
/// never be partial. Two-space JSON with a trailing newline, matching every
/// other committed schema in the repository.
fn committed_matches(file: &str, generated: &serde_json::Value, source: &str) {
    if std::env::var_os("GTS_REGEN").is_some() {
        let path = schema_path(file);
        let body = serde_json::to_string_pretty(generated).expect("serialise schema");
        std::fs::write(&path, format!("{body}\n"))
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }
    assert_eq!(
        &committed(file),
        generated,
        "{file} is out of date; regenerate it from {source} with \
         `GTS_REGEN=1 cargo test -p cf-gears-event-broker-sdk --lib gts_tests -- --ignored`"
    );
}

#[test]
#[ignore = "the emitted formatting is the gts library's to choose; run manually with `-- --ignored`"]
fn topic_base_schema_matches_committed_file() {
    committed_matches(
        "gts.cf.core.events.topic.v1~.schema.json",
        &emitted(&TopicV1::gts_schema_with_refs_as_string()),
        "`crate::gts::TopicV1`",
    );
}

#[test]
#[ignore = "the emitted formatting is the gts library's to choose; run manually with `-- --ignored`"]
fn event_base_schema_matches_committed_file() {
    committed_matches(
        "gts.cf.core.events.event.v1~.schema.json",
        &emitted(&EventV1::<()>::gts_schema_with_refs_as_string()),
        "`crate::gts::EventV1`",
    );
}

#[test]
fn only_the_two_base_types_are_declared() {
    // Inventory iteration order is link order, not declaration order.
    let mut ids: Vec<&str> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        .filter(|id| id.contains(".events."))
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

/// Validates a subscription whose single interest selects `selector` against the
/// committed subscription schema. Goes through `GtsStore` rather than a plain
/// JSON Schema validator, which would ignore `x-gts-pattern-ref`.
fn subscribe_with_selector(selector: &str) -> Result<(), gts::StoreError> {
    let type_id = "gts.cf.core.events.subscription.v1~";
    let mut store = gts::GtsStore::new();
    store.register_schema(
        type_id,
        &committed("gts.cf.core.events.subscription.v1~.schema.json"),
    )?;
    let subscription = serde_json::json!({
        "consumer_group": "gts.cf.core.events.consumer_group.v1~fabrikam.shop.orders.billing.v1",
        "client_agent": "test/1.0",
        "interests": [{
            "topic": "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1",
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "types": [selector],
        }],
    });
    store.validate_payload_with(type_id, &subscription, gts::GtsRefValidation::None)
}

#[test]
fn subscription_schema_accepts_concrete_wildcard_and_instance_event_type_selectors() {
    for pattern in [
        "gts.cf.core.events.event.v1~",
        "gts.cf.core.events.event.v1~*",
        "gts.cf.core.events.event.v1.2~*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.placed.v*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.placed.v1.*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.placed.v1~",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.named.v1",
    ] {
        gts::GtsIdPattern::try_new(pattern).expect("valid GTS pattern fixture");
        subscribe_with_selector(pattern)
            .unwrap_or_else(|error| panic!("selector {pattern} must be accepted: {error}"));
    }
}

#[test]
fn subscription_schema_rejects_malformed_and_non_event_type_selectors() {
    let too_long = format!(
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.{}.v1~",
        "a".repeat(1024)
    );
    for pattern in [
        "not-gts.*",
        "gts.*",
        "gts.cf.core.events.topic.v1~*",
        "gts.cf.core.events.event.v12~*",
        "gts.cf.core.events.event.v1~fabrikam.*.orders.placed.v1~",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.*~*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.plac*",
        "gts.cf.core.events.event.v1~fabrikam.shop.orders.placed.v01~",
        &too_long,
    ] {
        assert!(
            subscribe_with_selector(pattern).is_err(),
            "invalid event type selector {pattern} must be rejected"
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

// ---------------------------------------------------------------------------
// Worked examples
// ---------------------------------------------------------------------------
//
// The worked event schema is a hand-written owner example; the topic is an
// instance document. Both must conform to the generated base schemas. Rust
// derived declarations are exercised separately in `tests/gts_contract.rs`.

/// Checks an owner example's base reference and trait declarations.
fn example_conforms_to_base(example_file: &str, base: &serde_json::Value, base_uri: &str) {
    let example = committed(example_file);

    assert_eq!(
        example["allOf"][0]["$ref"],
        serde_json::json!(base_uri),
        "{example_file} must derive from {base_uri}"
    );

    let trait_schema = &base["x-gts-traits-schema"];
    let traits = &example["x-gts-traits"];
    let validator = jsonschema::validator_for(trait_schema).expect("base trait schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(traits)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{example_file} traits violate the base trait schema: {errors:?}"
    );

    let declared: Vec<&str> = trait_schema["properties"]
        .as_object()
        .expect("trait schema declares properties")
        .keys()
        .map(String::as_str)
        .collect();
    for key in traits.as_object().expect("traits is an object").keys() {
        assert!(
            declared.contains(&key.as_str()),
            "{example_file} declares trait `{key}`, which the base does not"
        );
    }
}

#[test]
fn committed_example_event_type_conforms_to_the_generated_base() {
    example_conforms_to_base(
        "examples/gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~.schema.json",
        &emitted(&EventV1::<()>::gts_schema_with_refs_as_string()),
        toolkit_gts::gts_uri!("cf.core.events.event.v1~"),
    );
}

/// The committed topic example is an instance document, so what holds it to the
/// base is ordinary JSON Schema validation rather than the trait-derivation
/// checks a derived type gets: a topic has no traits and derives from nothing.
#[test]
fn committed_example_topic_validates_against_the_generated_base() {
    let base = emitted(&TopicV1::gts_schema_with_refs_as_string());
    let example = committed("examples/gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1.json");

    let validator = jsonschema::validator_for(&base).expect("base schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(&example)
        .map(|err| err.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "the example topic does not validate against its base: {errors:?}"
    );

    assert!(
        example.get("allOf").is_none() && example.get("x-gts-traits").is_none(),
        "a topic is an instance: it derives from nothing and declares no traits"
    );
}

#[test]
fn a_topic_missing_a_required_property_is_rejected() {
    let base = emitted(&TopicV1::gts_schema_with_refs_as_string());
    let validator = jsonschema::validator_for(&base).expect("base schema compiles");

    // `description` is required: a stream nobody can identify is not registrable.
    let without_description = serde_json::json!({
        "id": "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1"
    });
    assert!(
        validator.iter_errors(&without_description).next().is_some(),
        "a topic without a description must not validate"
    );
}
