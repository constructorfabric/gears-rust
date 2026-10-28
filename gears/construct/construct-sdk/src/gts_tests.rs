use gts::GtsStore;
use serde_json::{Value, json};

use super::{
    CHAT_MESSAGE_TYPE, ChatMessageV1, ChatRole, ENRICHMENT_FACT_CANDIDATE_TYPE, FactCandidateV1,
    FactField, MASTERY_COURSE_CATALOG_TYPE, RECORD_BASE_TYPE, RECORD_TYPES, RecordV1,
    SUBJECT_REQUIRED_TRAIT, TypeRule, TypeRuleViolation, check_type_rules, record_base_schema,
    record_type_schema,
};

/// The schema of `type_id` as the link-time inventory submits it.
fn registered(type_id: &str) -> Value {
    toolkit_gts::all_inventory_type_schemas()
        .expect("the inventory schemas parse")
        .into_iter()
        .find(|schema| schema["$id"] == format!("gts://{type_id}"))
        .unwrap_or_else(|| panic!("{type_id} is not submitted"))
}

/// A gts store with every record type, as the types registry admits them.
fn store() -> GtsStore {
    let mut store = GtsStore::new();
    for type_id in RECORD_TYPES {
        store
            .register_schema(type_id, &registered(type_id))
            .unwrap_or_else(|err| panic!("{type_id} registers: {err}"));
    }
    store
}

#[test]
fn each_record_type_schema_is_the_one_it_registers() {
    for type_id in RECORD_TYPES {
        assert_eq!(
            record_type_schema(type_id),
            Some(registered(type_id)),
            "{type_id}"
        );
    }
    assert_eq!(record_type_schema("gts.cf.construct.unknown.v1~"), None);
}

#[test]
fn every_record_type_is_in_the_link_time_inventory() {
    for type_id in RECORD_TYPES {
        registered(type_id);
    }
}

#[test]
fn every_record_type_validates_in_a_gts_store_and_only_the_base_is_abstract() {
    let mut store = store();
    for type_id in RECORD_TYPES {
        // The keywords and the chain are checked here; a reference such as
        // `type`'s `x-gts-type-ref` resolves against the full types registry,
        // which this store does not hold.
        let resolved = store
            .validate_schema_with(type_id, gts::GtsRefValidation::None)
            .unwrap_or_else(|err| panic!("{type_id}: {err}"));
        assert_eq!(
            resolved.is_abstract,
            type_id == RECORD_BASE_TYPE,
            "{type_id}"
        );
    }
}

#[test]
fn every_derived_type_derives_from_the_base() {
    for type_id in RECORD_TYPES.into_iter().skip(1) {
        assert!(type_id.starts_with(RECORD_BASE_TYPE), "{type_id}");
        assert_eq!(
            registered(type_id)["allOf"][0]["$ref"],
            format!("gts://{RECORD_BASE_TYPE}"),
            "{type_id}"
        );
    }
}

#[test]
fn the_envelope_is_closed_and_requires_its_members() {
    let base = record_base_schema();
    assert_eq!(base["additionalProperties"], false);
    assert_eq!(
        base["required"],
        json!(["type", "provenance", "version", "observed_at", "payload"])
    );
    assert_eq!(
        base["properties"]["subject_id"]["type"], "string",
        "an absent subject is left out, never null"
    );
    assert_eq!(
        base["properties"]["type"]["x-gts-type-ref"],
        RECORD_BASE_TYPE
    );
}

#[test]
fn a_derived_type_closes_its_payload_and_inlines_its_enums() {
    let payload = &registered(CHAT_MESSAGE_TYPE)["allOf"][1]["properties"]["payload"];
    assert_eq!(payload["additionalProperties"], false);
    assert_eq!(
        payload["properties"]["role"]["enum"],
        json!(["user", "assistant"])
    );
}

#[test]
fn the_base_requires_a_subject_unless_a_type_turns_it_off() {
    let base = registered(RECORD_BASE_TYPE);
    assert_eq!(
        base["x-gts-traits-schema"]["properties"][SUBJECT_REQUIRED_TRAIT]["default"],
        true
    );
    assert_eq!(base["x-gts-traits"][SUBJECT_REQUIRED_TRAIT], true);
}

#[test]
fn only_the_course_catalog_turns_the_subject_off() {
    for type_id in RECORD_TYPES.into_iter().skip(1) {
        assert_eq!(
            registered(type_id)["x-gts-traits"][SUBJECT_REQUIRED_TRAIT],
            type_id != MASTERY_COURSE_CATALOG_TYPE,
            "{type_id}"
        );
    }
}

#[test]
fn observed_at_is_an_rfc_3339_string_on_the_wire() {
    let observed_at = &record_base_schema()["properties"]["observed_at"];
    assert_eq!(observed_at["type"], "string");
    assert_eq!(observed_at["format"], "date-time");
    assert!(observed_at["pattern"].is_string());
}

#[test]
fn a_chat_record_reads_into_the_typed_envelope_and_back() {
    let record = json!({
        "type": CHAT_MESSAGE_TYPE,
        "provenance": "chat_engine/thread-42/msg-7",
        "version": "2026-09-10T09:13:00Z",
        "observed_at": "2026-09-10T09:13:05Z",
        "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b",
        "payload": {
            "source": "chat_engine",
            "message_id": "msg-7",
            "thread_id": "thread-42",
            "role": "user",
            "text": "Hello"
        }
    });

    let typed: RecordV1<ChatMessageV1> =
        serde_json::from_value(record.clone()).expect("a valid record reads");

    assert_eq!(typed.payload.role, ChatRole::User);
    assert_eq!(typed.observed_at.unix_timestamp(), 1_789_031_585);
    assert_eq!(serde_json::to_value(&typed).expect("writes"), record);
}

/// The generated schemas are committed under `schemas/`, so a change to a
/// registered type version shows as a diff in review. After an intended
/// change, run the test with `UPDATE_SCHEMAS=1` to write them again.
#[test]
fn the_generated_schemas_match_the_committed_ones() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
    let update = std::env::var_os("UPDATE_SCHEMAS").is_some();
    for type_id in RECORD_TYPES {
        let path = dir.join(format!("{type_id}.schema.json"));
        let generated = registered(type_id);
        if update {
            std::fs::create_dir_all(&dir).expect("the schemas directory");
            let text = serde_json::to_string_pretty(&generated).expect("JSON");
            std::fs::write(&path, format!("{text}\n")).expect("write the schema");
        }
        let committed: Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display())),
        )
        .expect("the committed schema is JSON");
        assert_eq!(
            generated,
            committed,
            "{type_id} differs from {}; if the change is intended, run with UPDATE_SCHEMAS=1",
            path.display()
        );
    }
}

/// Pins the `gts-macros` 0.12 limit in the module doc: a payload that leaves
/// an optional member out does not read into its struct. When this fails, the
/// macro reads optional members, and the limit and this test can go.
#[test]
fn a_payload_without_an_optional_member_does_not_read_into_its_struct_yet() {
    let record = json!({
        "type": ENRICHMENT_FACT_CANDIDATE_TYPE,
        "provenance": "sha256:274b22ce748a45969ec5ead82845fc9324ffa01755758d9f2ab44c98f690fc15",
        "version": "sha256:f0606b676be2895716327ce8c55faa64bfbcd9e6231d2a25f081e6e8bb716e79",
        "observed_at": "2026-09-10T07:30:00Z",
        "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b",
        "payload": {
            "field": "board",
            "value": "Jane Doe serves on the advisory board.",
            "confidence": 0.83,
            "trust_tier": "found",
            "identity_confidence": 0.91
        }
    });

    let read = serde_json::from_value::<RecordV1<FactCandidateV1>>(record);

    let err = read.expect_err("evidence is optional but read as required");
    assert!(err.to_string().contains("evidence"), "{err}");
}

/// Every field of the type's enum, read from its registered schema.
fn every_fact_field() -> Vec<FactField> {
    serde_json::from_value(
        registered(ENRICHMENT_FACT_CANDIDATE_TYPE)["allOf"][1]["properties"]["payload"]
            ["properties"]["field"]["enum"]
            .clone(),
    )
    .expect("the field enum reads")
}

/// A fact candidate with valid digests, for `field` and `value`.
fn candidate(field: FactField, value: &Value) -> Value {
    json!({
        "provenance": format!("sha256:{}", "a".repeat(64)),
        "version": format!("sha256:{}", "b".repeat(64)),
        "payload": { "field": field, "value": value }
    })
}

#[test]
fn only_research_areas_and_skills_are_list_fields() {
    let lists: Vec<FactField> = every_fact_field()
        .into_iter()
        .filter(|field| field.is_list())
        .collect();
    assert_eq!(lists, vec![FactField::ResearchAreas, FactField::Skills]);
}

/// The rule intake runs takes a list exactly for the list fields, and one
/// string exactly for every other field.
#[test]
fn every_field_takes_the_value_form_its_kind_says() {
    let list = json!(["One item"]);
    let one = json!("One statement.");
    for field in every_fact_field() {
        let (fits, other) = if field.is_list() {
            (&list, &one)
        } else {
            (&one, &list)
        };
        assert_eq!(
            check_type_rules(ENRICHMENT_FACT_CANDIDATE_TYPE, &candidate(field, fits)),
            Ok(()),
            "{field:?}"
        );
        assert_eq!(
            check_type_rules(ENRICHMENT_FACT_CANDIDATE_TYPE, &candidate(field, other)),
            Err(TypeRuleViolation {
                place: "/payload/value".to_owned(),
                rule: TypeRule::Type
            }),
            "{field:?}"
        );
    }
}

#[test]
fn the_type_rules_apply_only_to_the_fact_candidate() {
    let record = json!({ "provenance": "not a digest", "payload": { "value": 1 } });
    assert_eq!(check_type_rules(CHAT_MESSAGE_TYPE, &record), Ok(()));
    assert_eq!(check_type_rules(RECORD_BASE_TYPE, &record), Ok(()));
    assert_eq!(
        check_type_rules(ENRICHMENT_FACT_CANDIDATE_TYPE, &record),
        Err(TypeRuleViolation {
            place: "/provenance".to_owned(),
            rule: TypeRule::Pattern
        })
    );
}

#[test]
fn a_digest_is_sha256_and_64_lower_case_hex_digits() {
    let with = |provenance: &str| {
        let mut record = candidate(FactField::Board, &json!("A statement."));
        record["provenance"] = json!(provenance);
        check_type_rules(ENRICHMENT_FACT_CANDIDATE_TYPE, &record)
    };
    assert_eq!(
        with(&format!("sha256:{}", "0123456789abcdef".repeat(4))),
        Ok(())
    );
    for broken in [
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "a".repeat(65)),
        format!("sha256:{}", "g".repeat(64)),
        format!("SHA256:{}", "a".repeat(64)),
        format!("sha512:{}", "a".repeat(64)),
        "sha256:".to_owned(),
        "a".repeat(64),
    ] {
        assert_eq!(
            with(&broken),
            Err(TypeRuleViolation {
                place: "/provenance".to_owned(),
                rule: TypeRule::Pattern
            }),
            "{broken}"
        );
    }
}

/// The base's optional `id` takes a GTS instance id, refuses a type id and a
/// malformed one, and reads into the envelope and back.
#[test]
fn a_record_kept_in_a_store_carries_its_instance_id() {
    let base = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&json!({
            "type": "object",
            "properties": { "id": record_base_schema()["properties"]["id"].clone() }
        }))
        .expect("the id schema compiles");
    let instance = format!("{CHAT_MESSAGE_TYPE}cf.construct.examples.chat_message.v1");
    assert!(base.is_valid(&json!({ "id": instance })));
    assert!(
        !base.is_valid(&json!({ "id": CHAT_MESSAGE_TYPE })),
        "a type id"
    );
    assert!(!base.is_valid(&json!({ "id": "not a gts id" })));

    let record = json!({
        "id": instance,
        "type": CHAT_MESSAGE_TYPE,
        "provenance": "chat_engine/thread-42/msg-7",
        "version": "2026-09-10T09:13:00Z",
        "observed_at": "2026-09-10T09:13:05Z",
        "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b",
        "payload": {
            "source": "chat_engine",
            "message_id": "msg-7",
            "thread_id": "thread-42",
            "role": "user",
            "text": "Hello"
        }
    });
    let typed: RecordV1<ChatMessageV1> =
        serde_json::from_value(record.clone()).expect("a stored record reads");
    assert_eq!(
        typed.store_id.as_ref().map(ToString::to_string).as_deref(),
        Some(instance.as_str())
    );
    assert_eq!(serde_json::to_value(&typed).expect("writes"), record);
}

/// A list item that starts or ends with whitespace is refused in code too,
/// for a validator whose `$` matches before a final newline.
#[test]
fn a_list_item_that_is_not_stripped_breaks_the_rules() {
    for (index, item) in [(0, " Lead"), (1, "Trail\n"), (0, "\u{1c}Separator")] {
        let mut items = vec![json!("First"), json!("Second")];
        items[index] = json!(item);
        assert_eq!(
            check_type_rules(
                ENRICHMENT_FACT_CANDIDATE_TYPE,
                &candidate(FactField::Skills, &json!(items))
            ),
            Err(TypeRuleViolation {
                place: format!("/payload/value/{index}"),
                rule: TypeRule::Pattern
            }),
            "{item:?}"
        );
    }
}

/// The check does not rely on the schema having run.
#[test]
fn an_unknown_field_or_a_non_string_item_breaks_the_rules() {
    let mut record = candidate(FactField::Board, &json!("A statement."));
    record["payload"]["field"] = json!("hobby");
    assert_eq!(
        check_type_rules(ENRICHMENT_FACT_CANDIDATE_TYPE, &record),
        Err(TypeRuleViolation {
            place: "/payload/field".to_owned(),
            rule: TypeRule::Enum
        })
    );
    assert_eq!(
        check_type_rules(
            ENRICHMENT_FACT_CANDIDATE_TYPE,
            &candidate(FactField::Skills, &json!(["One", 7]))
        ),
        Err(TypeRuleViolation {
            place: "/payload/value/1".to_owned(),
            rule: TypeRule::Type
        })
    );
}

#[test]
fn a_rule_is_named_by_its_json_schema_keyword() {
    assert_eq!(TypeRule::Pattern.keyword(), "pattern");
    assert_eq!(TypeRule::Type.keyword(), "type");
    assert_eq!(TypeRule::Enum.keyword(), "enum");
}
