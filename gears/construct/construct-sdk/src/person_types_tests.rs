use std::collections::BTreeSet;

use serde_json::{Value, json};
use toolkit_gts::gts_uri;

use super::*;

const OWNED_NODE_REF: &str = gts_uri!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~");
const PROVENANCE_REF: &str = gts_uri!("cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~");

fn schema(type_id: &str) -> Value {
    let (_, text) = PERSON_TYPES
        .iter()
        .find(|(id, _)| *id == type_id)
        .expect("a person type");
    serde_json::from_str(text).expect("a JSON schema")
}

fn payload_schema(type_id: &str) -> Value {
    schema(type_id)["allOf"][1]["properties"]["payload"].clone()
}

fn fact_properties(type_id: &str) -> BTreeSet<String> {
    payload_schema(type_id)["properties"]
        .as_object()
        .expect("payload properties")
        .keys()
        .filter(|name| *name != ORIGIN_PROPERTY)
        .cloned()
        .collect()
}

/// The payload schema with the origin accepted as is: the provenance type belongs to graph storage, which validates
/// it on ingest.
fn payload_validator_without_origin_schema(type_id: &str) -> jsonschema::Validator {
    let mut payload = payload_schema(type_id);
    payload["properties"][ORIGIN_PROPERTY] = json!(true);
    jsonschema::validator_for(&payload).expect("a valid payload schema")
}

fn origin() -> Value {
    json!({ "produced_by": { "subject_id": "connector" }, "produced_at": "2026-10-07T00:00:00Z" })
}

fn with_origin(fact: Value) -> Value {
    let mut payload = fact;
    payload[ORIGIN_PROPERTY] = origin();
    payload
}

#[test]
fn each_schema_names_its_type_and_derives_from_the_owned_node() {
    for (type_id, _) in PERSON_TYPES {
        let document = schema(type_id);
        assert_eq!(document["$id"], format!("gts://{type_id}"));
        assert_eq!(document["allOf"][0]["$ref"], OWNED_NODE_REF);
    }
}

#[test]
fn the_origin_is_required_and_is_graph_storage_provenance() {
    for (type_id, _) in PERSON_TYPES {
        let payload = payload_schema(type_id);
        assert_eq!(payload["required"], json!([ORIGIN_PROPERTY]), "{type_id}");
        assert_eq!(
            payload["properties"][ORIGIN_PROPERTY]["$ref"], PROVENANCE_REF,
            "{type_id}"
        );
    }
}

#[test]
fn every_fact_property_has_a_cardinality_and_only_those() {
    for (type_id, _) in PERSON_TYPES {
        let listed: BTreeSet<String> = PROPERTIES
            .iter()
            .filter(|(listed_type, _, _)| *listed_type == type_id)
            .map(|(_, property, _)| (*property).to_owned())
            .collect();
        assert_eq!(listed, fact_properties(type_id), "{type_id}");
    }
}

#[test]
fn cardinality_tells_a_single_fact_from_a_repeated_one() {
    assert_eq!(
        cardinality(IDENTITY_TYPE, "full_name"),
        Some(Cardinality::One)
    );
    assert_eq!(cardinality(SKILLS_TYPE, "skills"), Some(Cardinality::Many));
    assert_eq!(cardinality(IDENTITY_TYPE, "skills"), None);
    assert_eq!(cardinality(IDENTITY_TYPE, ORIGIN_PROPERTY), None);
}

#[test]
fn a_payload_with_one_fact_and_its_origin_fits() {
    let fitting = [
        (IDENTITY_TYPE, json!({ "orcid_id": "0000-0002-1825-0097" })),
        (
            IDENTITY_TYPE,
            json!({ "public_mentions": "Keynote at a research conference" }),
        ),
        (
            ROLES_TYPE,
            json!({ "work_history": { "organization": "Acme", "start_year": 2019 } }),
        ),
        (
            SKILLS_TYPE,
            json!({ "skills": { "name": "Python", "level": "expert" } }),
        ),
        (PREFERENCES_TYPE, json!({ "tone_preference": "concise" })),
        (
            PREFERENCES_TYPE,
            json!({ "long_term_goals": "Lead a research group" }),
        ),
    ];
    for (type_id, fact) in fitting {
        let payload = with_origin(fact);
        assert!(
            payload_validator_without_origin_schema(type_id).is_valid(&payload),
            "{type_id}: {payload}"
        );
    }
}

#[test]
fn a_payload_that_breaks_the_type_does_not_fit() {
    let misfits = [
        (
            PREFERENCES_TYPE,
            json!({ "tone_preference": "concise" }),
            "no origin",
        ),
        (
            PREFERENCES_TYPE,
            json!({ ORIGIN_PROPERTY: origin() }),
            "origin without a fact",
        ),
        (
            PREFERENCES_TYPE,
            with_origin(json!({ "tone_preference": "concise", "units": "si_metric" })),
            "two facts",
        ),
        (
            PREFERENCES_TYPE,
            with_origin(json!({ "favourite_snack": "crisps" })),
            "unknown property",
        ),
        (
            PREFERENCES_TYPE,
            with_origin(json!({ "tone_preference": "shouty" })),
            "value outside the enum",
        ),
        (
            SKILLS_TYPE,
            with_origin(json!({ "skills": { "level": "expert" } })),
            "entry without its main field",
        ),
        (
            SKILLS_TYPE,
            with_origin(json!({ "skills": "Python" })),
            "entry as plain text",
        ),
        (
            ROLES_TYPE,
            with_origin(json!({ "work_history": { "organization": "Acme", "start_year": 1750 } })),
            "year before 1800",
        ),
    ];
    for (type_id, payload, case) in misfits {
        assert!(
            !payload_validator_without_origin_schema(type_id).is_valid(&payload),
            "{case}: {payload}"
        );
    }
}
