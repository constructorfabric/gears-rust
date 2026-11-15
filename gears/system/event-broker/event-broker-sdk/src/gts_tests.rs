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

use crate::gts::{EventV1, EventV1Traits, TopicV1};

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
            "subject_tenant",
            "source",
            "subject",
            "subject_type",
            "subject_owner",
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
            "description": "Server-stamped partition for the event, surfaced on read. Assigned by the storage backend from the key the event type's `partition_key` trait resolves to. Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.",
            "type": "integer",
            "format": "int32",
            "minimum": 0,
            "readOnly": true,
        })
    );
    assert_eq!(
        base["properties"]["sequence"],
        serde_json::json!({
            "description": "Server-assigned monotonic ordering key per (topic, partition), surfaced on read. Consumer offsets (an exact SEEK position and a committed cursor) are values of it. Distinct from `meta.sequence` (publish-only producer-side chain field). Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.",
            "type": "integer",
            "format": "int64",
            "minimum": 0,
            "readOnly": true,
        })
    );
    assert_eq!(
        base["properties"]["sequence_time"],
        serde_json::json!({
            "description": "Server-stamped timestamp recording when `sequence` was assigned. Surfaced on read. Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.",
            "type": "string",
            "format": "date-time",
            "readOnly": true,
        })
    );
}

/// The base's top-level description names the code a publish carrying a
/// `readOnly` member is refused with. Unlike the committed-file compares above,
/// this runs in every test pass.
#[test]
fn the_base_schema_description_names_the_invalid_body_refusal() {
    let base = emitted(&<EventV1>::gts_schema_with_refs_as_string());

    assert_eq!(
        base["description"],
        serde_json::json!(
            "Event schema. Single canonical resource shape used by both publish input (POST /v1/events, POST /v1/events:batch) and read responses (the `payload` of each `event` frame of GET /v1/events:stream and GET /v1/events:sse). Per-direction semantics are encoded via JSON Schema field-level markers: `writeOnly` fields (meta) are accepted on publish and stripped on read; `readOnly` fields (partition, sequence, sequence_time) are server-stamped on read and rejected with 400 InvalidBody if supplied on publish. The top-level `required` list is the union of publish-required and read-required fields; strict-validator producers must filter `readOnly` fields before submission."
        )
    );
}

#[test]
fn only_the_topic_and_event_base_types_are_declared() {
    // Inventory iteration order is link order, not declaration order. The derived
    // type the tests declare is excluded by its id, so a stray derived type the SDK
    // declares by mistake is still caught.
    let test_declared = [<OrderPlacedV1 as GtsSchema>::TYPE_ID];
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
            <EventV1>::TYPE_ID,
            emitted(&<EventV1>::gts_schema_with_refs_as_string()),
        ),
    ] {
        store
            .register_schema(id, &schema)
            .expect("register base schema");
    }
    for id in [TopicV1::TYPE_ID, <EventV1>::TYPE_ID] {
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
            "subject_tenant": "00000000-0000-0000-0000-000000000001",
            "types": [selector],
        }],
    });
    store.validate_payload_with(type_id, &subscription, gts::GtsRefValidation::None)
}

/// Validates a subscription whose single interest carries `subject_types` against
/// the committed subscription schema, through `GtsStore` as above.
fn subscribe_with_subject_types(subject_types: &[String]) -> Result<(), gts::StoreError> {
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
            "subject_tenant": "00000000-0000-0000-0000-000000000001",
            "types": ["gts.cf.core.events.event.v1~fabrikam.shop.orders.*"],
            "subject_types": subject_types,
        }],
    });
    store.validate_payload_with(type_id, &subscription, gts::GtsRefValidation::None)
}

/// Validates a subscription whose single interest is `interest` against the
/// committed subscription schema, through `GtsStore` as above.
fn subscribe_with_interest(interest: serde_json::Value) -> Result<(), gts::StoreError> {
    let type_id = "gts.cf.core.events.subscription.v1~";
    let mut store = gts::GtsStore::new();
    store.register_schema(
        type_id,
        &committed("gts.cf.core.events.subscription.v1~.schema.json"),
    )?;
    let subscription = serde_json::json!({
        "consumer_group": "gts.cf.core.events.consumer_group.v1~fabrikam.shop.orders.billing.v1",
        "client_agent": "test/1.0",
        "interests": [interest],
    });
    store.validate_payload_with(type_id, &subscription, gts::GtsRefValidation::None)
}

#[test]
fn subscription_schema_accepts_an_interest_without_a_tenant_and_each_ownership_filter() {
    for ownership_filter in ["only_mine", "entire_tenant", "tenant_subtree_included"] {
        subscribe_with_interest(serde_json::json!({
            "topic": "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1",
            "ownership_filter": ownership_filter,
            "types": ["gts.cf.core.events.event.v1~fabrikam.shop.orders.*"],
        }))
        .unwrap_or_else(|error| panic!("{ownership_filter} must be accepted: {error}"));
    }
}

#[test]
fn subscription_schema_refuses_max_depth_and_an_unknown_ownership_filter() {
    for (label, extra) in [
        ("max_depth", serde_json::json!({ "max_depth": 0 })),
        (
            "ownership_filter",
            serde_json::json!({ "ownership_filter": "everyone" }),
        ),
    ] {
        let mut interest = serde_json::json!({
            "topic": "gts.cf.core.events.topic.v1~fabrikam.shop._.orders.v1",
            "types": ["gts.cf.core.events.event.v1~fabrikam.shop.orders.*"],
        });
        if let (Some(interest), Some(extra)) = (interest.as_object_mut(), extra.as_object()) {
            interest.extend(extra.clone());
        }
        assert!(
            subscribe_with_interest(interest).is_err(),
            "{label} must be refused"
        );
    }
}

#[test]
fn subscription_schema_accepts_a_subject_type_selector() {
    subscribe_with_subject_types(&["gts.fabrikam.shop.orders.customer.v1~".to_owned()])
        .unwrap_or_else(|error| panic!("a subject type selector must be accepted: {error}"));
}

#[test]
fn subscription_schema_refuses_33_subject_types() {
    let selectors: Vec<String> = (0..33)
        .map(|i| format!("gts.fabrikam.shop.orders.subject{i}.v1~"))
        .collect();

    let result = subscribe_with_subject_types(&selectors);

    let Err(error) = result else {
        panic!("33 subject types must be refused");
    };
    // The `maxItems` keyword's message, which quotes the array it refuses.
    assert_eq!(
        error.to_string(),
        format!(
            "Validation failed: {} has more than 32 items",
            serde_json::to_string(&selectors).unwrap()
        )
    );
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
    traits = EventV1Traits::builder()
        .with_topic(gts_id!("cf.core.events.topic.v1~fabrikam.shop._.orders.v1"))
        .with_allowed_subject_types(["gts.fabrikam.shop.orders.order.v1~"])
        .build(),
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
            "subject_tenant": "01930000-0000-7000-8000-000000000002",
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": "gts.fabrikam.shop.orders.order.v1~",
            "subject_owner": null,
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
