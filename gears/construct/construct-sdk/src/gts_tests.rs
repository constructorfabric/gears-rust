use gts::GtsStore;
use serde_json::{Value, json};

use super::{
    CHAT_MESSAGE_TYPE, ChatMessageV1, ChatRole, ENRICHMENT_FACT_CANDIDATE_TYPE, FactCandidateV1,
    MASTERY_COURSE_CATALOG_TYPE, RECORD_BASE_TYPE, RECORD_TYPES, RecordV1, SUBJECT_REQUIRED_TRAIT,
    record_base_schema,
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
fn every_record_type_is_in_the_link_time_inventory() {
    for type_id in RECORD_TYPES {
        registered(type_id);
    }
}

#[test]
fn every_record_type_validates_in_a_gts_store_and_only_the_base_is_abstract() {
    let mut store = store();
    for type_id in RECORD_TYPES {
        let resolved = store
            .validate_schema(type_id)
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
    assert_eq!(base["properties"]["type"]["x-gts-ref"], RECORD_BASE_TYPE);
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
        "provenance": "openalex/A5012345678",
        "version": "v1",
        "observed_at": "2026-09-10T07:30:00Z",
        "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b",
        "payload": {
            "claim": "A claim.",
            "fact_type": "role",
            "confidence": 0.5,
            "trust_tier": "found",
            "source_url": "https://example.org/a",
            "date": "2026-09-09",
            "citations": []
        }
    });

    let read = serde_json::from_value::<RecordV1<FactCandidateV1>>(record);

    let err = read.expect_err("evidence is optional but read as required");
    assert!(err.to_string().contains("evidence"), "{err}");
}
