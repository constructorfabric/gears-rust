//! Generates `docs/schemas/` from the declarations in [`crate::gts`], and holds
//! the worked examples to them.
//!
//! The same function produces a committed file and the schema registered into
//! `types-registry`, so a hand-edited file would document a contract the broker
//! does not register. Regenerate rather than edit:
//!
//! ```text
//! GTS_REGEN=1 cargo test -p cf-gears-event-broker-sdk --features serde_json/preserve_order \
//!     --lib gts_tests -- --ignored
//! ```
//!
//! The committed-file comparison tests are `#[ignore]`d, hence `--ignored`: the
//! emitted schema belongs to the gts library and changes with it, so comparing it
//! gates on a third party's output. Drop the `GTS_REGEN=1` to compare instead of
//! rewrite. The contract stays covered by the conformance tests below, which
//! validate behaviour against the freshly emitted base.

use gts::GtsSchema;
use toolkit_gts::{gts_id, gts_type_schema};
use uuid::Uuid;

use crate::gts::{EventV1, SubjectV1, TopicV1};

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
         `GTS_REGEN=1 cargo test -p cf-gears-event-broker-sdk \
         --features serde_json/preserve_order --lib gts_tests -- --ignored`"
    );
}

#[test]
#[ignore = "the emitted schema is the gts library's to choose; run manually with `-- --ignored`"]
fn topic_base_schema_matches_committed_file() {
    committed_matches(
        "gts.cf.core.events.topic.v1~.schema.json",
        &emitted(&TopicV1::gts_schema_with_refs_as_string()),
        "`crate::gts::TopicV1`",
    );
}

#[test]
#[ignore = "the emitted schema is the gts library's to choose; run manually with `-- --ignored`"]
fn event_base_schema_matches_committed_file() {
    committed_matches(
        "gts.cf.core.events.event.v1~.schema.json",
        &emitted(&<EventV1>::gts_schema_with_refs_as_string()),
        "`crate::gts::EventV1`",
    );
}

#[test]
#[ignore = "the emitted schema is the gts library's to choose; run manually with `-- --ignored`"]
fn subject_base_schema_matches_committed_file() {
    committed_matches(
        "gts.cf.core.events.subject.v1~.schema.json",
        &emitted(&SubjectV1::<()>::gts_schema_with_refs_as_string()),
        "`crate::gts::SubjectV1`",
    );
}

#[test]
#[ignore = "the emitted schema is the gts library's to choose; run manually with `-- --ignored`"]
fn event_example_matches_committed_file() {
    committed_matches(
        "examples/gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~.schema.json",
        &emitted(&OrderPlacedV1::gts_schema_with_refs_as_string()),
        "`crate::gts_tests::OrderPlacedV1`",
    );
}

/// The base requires `source` and the server-stamped members although their Rust
/// fields are `Option`s, and `source` keeps its text constraints. Unlike the
/// committed-file compares above, this runs in every test pass.
#[test]
fn the_base_schema_keeps_source_and_the_stamped_members_required() {
    let base = emitted(&<EventV1>::gts_schema_with_refs_as_string());

    assert_eq!(
        base["required"],
        serde_json::json!([
            "id",
            "type",
            "tenant_id",
            "source",
            "subject",
            "subject_type",
            "occurred_at",
            "partition",
            "sequence",
            "sequence_time",
        ])
    );
    assert_eq!(
        base["properties"]["source"],
        serde_json::json!({
            "description": "Origin of the event (e.g., service name). ASCII only.",
            "type": "string",
            "maxLength": 256,
            "pattern": "^[\\x20-\\x7E]+$",
        })
    );
    assert_eq!(
        base["properties"]["partition"],
        serde_json::json!({
            "description": "Server-stamped partition for the event, surfaced on read. Computed at ingest via the partition contract. Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.",
            "type": "integer",
            "format": "int32",
            "minimum": 0,
            "readOnly": true,
        })
    );
    assert_eq!(
        base["properties"]["sequence"],
        serde_json::json!({
            "description": "Server-assigned monotonic ordering key per (topic, partition), surfaced on read. The only sequence consumers paginate by. Distinct from `meta.sequence` (publish-only producer-side chain field). Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.",
            "type": "integer",
            "format": "int64",
            "minimum": 0,
            "readOnly": true,
        })
    );
    assert_eq!(
        base["properties"]["sequence_time"],
        serde_json::json!({
            "description": "Server-stamped timestamp recording when `sequence` was assigned. Surfaced on read. Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.",
            "type": "string",
            "format": "date-time",
            "readOnly": true,
        })
    );
}

#[test]
fn only_the_three_base_types_are_declared() {
    // Inventory iteration order is link order, not declaration order. The derived
    // types the tests declare are excluded by their ids, so a stray derived type the
    // SDK declares by mistake is still caught.
    let test_declared = [
        <OrderSubjectV1 as GtsSchema>::TYPE_ID,
        <OrderPlacedV1 as GtsSchema>::TYPE_ID,
    ];
    let mut ids: Vec<&str> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        .filter(|id| id.contains(".events.") && !test_declared.contains(id))
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![
            toolkit_gts::gts_id!("cf.core.events.event.v1~"),
            toolkit_gts::gts_id!("cf.core.events.subject.v1~"),
            toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        ],
        "the broker declares exactly the topic, event and subject base types"
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
            <EventV1>::TYPE_ID,
            emitted(&<EventV1>::gts_schema_with_refs_as_string()),
        ),
        (
            SubjectV1::<()>::TYPE_ID,
            emitted(&SubjectV1::<()>::gts_schema_with_refs_as_string()),
        ),
    ] {
        store
            .register_schema(id, &schema)
            .expect("register base schema");
    }
    for id in [
        TopicV1::TYPE_ID,
        <EventV1>::TYPE_ID,
        SubjectV1::<()>::TYPE_ID,
    ] {
        store
            .validate_schema_with(id, gts::GtsRefValidation::None)
            .expect("generated base schema must use valid GTS keywords");
    }
}

/// A subject kind as the gear that owns the entity declares it.
#[gts_type_schema(
    dir_path = "schemas",
    base = SubjectV1,
    type_id = gts_id!("cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~"),
    description = "An order placed in the fabrikam shop.",
    properties = "",
)]
pub(crate) struct OrderSubjectV1;

/// `event-broker/tests/bare_harness_test.rs` seeds a copy of this schema; change both together.
#[test]
fn a_derived_subject_kind_names_its_full_id_and_derives_from_the_base() {
    assert_eq!(
        <OrderSubjectV1 as GtsSchema>::TYPE_ID,
        "gts.cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~"
    );

    let derived = emitted(&OrderSubjectV1::gts_schema_with_refs_as_string());
    assert_eq!(
        derived,
        serde_json::json!({
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
        })
    );

    let mut store = gts::GtsStore::new();
    store
        .register_schema(
            SubjectV1::<()>::TYPE_ID,
            &emitted(&SubjectV1::<()>::gts_schema_with_refs_as_string()),
        )
        .expect("register the subject base");
    store
        .register_schema(OrderSubjectV1::TYPE_ID, &derived)
        .expect("register the derived subject kind");
    store
        .validate_schema_with(OrderSubjectV1::TYPE_ID, gts::GtsRefValidation::None)
        .expect("a derived subject kind must validate against the subject base");
}

#[test]
fn subject_debug_names_the_properties_type_but_never_prints_them() {
    let subject = SubjectV1::<serde_json::Value> {
        r#type: gts::GtsTypeId::new(OrderSubjectV1::TYPE_ID),
        id: "order-42".to_owned(),
        properties: serde_json::json!({ "secret": "PLANTED-VALUE-7f3a" }),
    };

    let printed = format!("{subject:?}");

    assert_eq!(
        printed,
        format!(
            "SubjectV1 {{ type: {:?}, id: \"order-42\", properties: {:?} }}",
            subject.r#type,
            std::any::type_name::<serde_json::Value>()
        )
    );
    assert!(
        !printed.contains("PLANTED-VALUE-7f3a"),
        "Debug printed the properties: {printed}"
    );
}

fn admit_event_with_subject_pattern(pattern: &str) -> Result<gts::ResolvedType, gts::StoreError> {
    let mut store = gts::GtsStore::new();
    let base = emitted(&<EventV1>::gts_schema_with_refs_as_string());
    store.register_schema(<EventV1>::TYPE_ID, &base).unwrap();
    let type_id = "gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~";
    let derived = crate::gts::derived_event_type_schema(
        type_id,
        "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1",
        serde_json::json!({}),
        &[pattern],
    );
    store.register_schema(type_id, &derived).unwrap();
    store.validate_schema_with(type_id, gts::GtsRefValidation::None)
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
    let schema = emitted(&<EventV1>::gts_schema_with_refs_as_string());
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
// The event-type example under `docs/schemas/examples/` is generated from
// `OrderPlacedV1` below, like the base documents. The topic example is an
// instance document, held to its base by validation. `fabrikam` is the vendor
// this repository uses for a third party (`docs/TOOLKIT_PLUGINS.md`), and is one
// of the four registered with `gts-validator`.

/// The worked producer example; `docs/schemas/examples/` holds the document it generates.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~"),
    description = "Worked producer example: one concrete event type. It narrows the base event's `data` member to its own payload contract, and fixes its trait values. Nothing here is a field a publisher sends - the traits govern how the broker treats events of this type, and GTS forbids trait keywords in an instance.",
    properties = "order_id,total_cents,currency,coupon_code",
    traits = serde_json::json!({
        "topic": gts_id!("cf.core.events.topic.v1~fabrikam.shop._.orders.v1"),
        "allowed_subject_types": ["gts.fabrikam.shop.orders.order.v1~"],
    }),
)]
pub(crate) struct OrderPlacedV1 {
    pub order_id: Uuid,
    #[schemars(range(min = 0))]
    pub total_cents: i64,
    #[schemars(regex(pattern = r"^[A-Z]{3}$"))]
    pub currency: String,
    // Present on every event, `null` when unset: the macro's serde neither skips nor
    // defaults a field; the schema requires it and admits `null`.
    #[schemars(required, extend("type" = ["string", "null"]), length(max = 64))]
    pub coupon_code: Option<String>,
}

/// Holds a committed derived-type example to the base it claims to derive from:
/// it must `$ref` that base, its trait values must satisfy the base's generated
/// `x-gts-traits-schema`, and it must declare no trait the base does not.
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
        &emitted(&<EventV1>::gts_schema_with_refs_as_string()),
        toolkit_gts::gts_uri!("cf.core.events.event.v1~"),
    );
}

/// The generated example is admitted by the type store and holds events to its `data`
/// contract. A stored event is used because the base requires the `readOnly` members.
#[test]
fn the_worked_example_is_admitted_and_holds_events_to_its_data() {
    let mut store = gts::GtsStore::new();
    store
        .register_schema(
            <EventV1>::TYPE_ID,
            &emitted(&<EventV1>::gts_schema_with_refs_as_string()),
        )
        .expect("register the event base");
    store
        .register_schema(
            OrderPlacedV1::TYPE_ID,
            &emitted(&OrderPlacedV1::gts_schema_with_refs_as_string()),
        )
        .expect("register the worked example");
    store
        .validate_schema_with(OrderPlacedV1::TYPE_ID, gts::GtsRefValidation::None)
        .expect("the worked example must be admitted");

    let stored_event = |data: serde_json::Value| {
        serde_json::json!({
            "id": "01930000-0000-7000-8000-000000000001",
            "type": OrderPlacedV1::TYPE_ID,
            "tenant_id": "01930000-0000-7000-8000-000000000002",
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": "gts.fabrikam.shop.orders.order.v1~",
            "occurred_at": "2026-11-06T10:00:00Z",
            "data": data,
            "partition": 0,
            "sequence": 1,
            "sequence_time": "2026-11-06T10:00:01Z",
        })
    };
    let order_id = "01930000-0000-7000-8000-000000000003";

    store
        .validate_payload_with(
            OrderPlacedV1::TYPE_ID,
            &stored_event(serde_json::json!({
                "order_id": order_id,
                "total_cents": 4299,
                "currency": "EUR",
                "coupon_code": null,
            })),
            gts::GtsRefValidation::None,
        )
        .expect("an event within the data contract must validate");

    // The macro's serde requires every field, so the schema refuses an absent one.
    let absent_coupon = store.validate_payload_with(
        OrderPlacedV1::TYPE_ID,
        &stored_event(serde_json::json!({
            "order_id": order_id,
            "total_cents": 4299,
            "currency": "EUR",
        })),
        gts::GtsRefValidation::None,
    );
    let Err(gts::StoreError::ValidationError(message)) = absent_coupon else {
        panic!("an event without `coupon_code` must be refused, got {absent_coupon:?}");
    };
    assert_eq!(
        message,
        "Validation failed: \"coupon_code\" is a required property"
    );

    let lowercase = store.validate_payload_with(
        OrderPlacedV1::TYPE_ID,
        &stored_event(serde_json::json!({
            "order_id": order_id,
            "total_cents": 4299,
            "currency": "eur",
            "coupon_code": null,
        })),
        gts::GtsRefValidation::None,
    );
    let Err(gts::StoreError::ValidationError(message)) = lowercase else {
        panic!("a lowercase currency must be refused, got {lowercase:?}");
    };
    assert_eq!(
        message,
        "Validation failed: \"eur\" does not match \"^[A-Z]{3}$\""
    );

    let negative = store.validate_payload_with(
        OrderPlacedV1::TYPE_ID,
        &stored_event(serde_json::json!({
            "order_id": order_id,
            "total_cents": -1,
            "currency": "EUR",
            "coupon_code": null,
        })),
        gts::GtsRefValidation::None,
    );
    let Err(gts::StoreError::ValidationError(message)) = negative else {
        panic!("a negative total must be refused, got {negative:?}");
    };
    assert_eq!(
        message,
        "Validation failed: -1 is less than the minimum of 0"
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

/// A derived event type may only *narrow* its base. An empty payload schema
/// admits any JSON value, which is wider than the base's `["object", "null"]`,
/// and `types-registry` refuses the document with "Schema at `$.data` changes
/// type incompatibly" - which stops the whole gear from booting, since the
/// registry admits its configuration-seeded entities at startup.
///
/// That failure reached a running process once because nothing here covered
/// this helper at all: every fixture and the mock build their event types
/// through it, and none of them validates against the base.
#[test]
fn an_unconstrained_payload_renders_as_the_bases_own_type_not_as_any() {
    let base_data =
        committed("gts.cf.core.events.event.v1~.schema.json")["properties"]["data"]["type"].clone();

    let derived = crate::gts::derived_event_type_schema(
        "gts.cf.core.events.event.v1~example.eb.narrowing.foo.v1~",
        "gts.cf.core.events.topic.v1~example.eb.narrowing.topic.v1",
        serde_json::json!({}),
        &["gts.example.eb.narrowing.subject.v1~"],
    );

    let narrowing = &derived["allOf"][1]["properties"]["data"];
    assert_eq!(
        narrowing["type"], base_data,
        "stating no constraints means whatever the base allows, so the member \
         carries the base's own type rather than an empty schema"
    );
    assert_ne!(
        *narrowing,
        serde_json::json!({}),
        "an empty schema here is what registration refuses"
    );
}

/// The other half: a payload contract the caller supplies is written through
/// untouched, since narrowing is exactly what it is for.
#[test]
fn a_supplied_payload_contract_is_written_through_unchanged() {
    let contract = serde_json::json!({
        "type": "object",
        "required": ["order_id"],
        "properties": { "order_id": { "type": "string", "format": "uuid" } },
    });

    let derived = crate::gts::derived_event_type_schema(
        "gts.cf.core.events.event.v1~example.eb.narrowing.foo.v1~",
        "gts.cf.core.events.topic.v1~example.eb.narrowing.topic.v1",
        contract.clone(),
        &[],
    );

    assert_eq!(derived["allOf"][1]["properties"]["data"], contract);
}
