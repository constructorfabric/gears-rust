use serde_json::{Value, json};
use toolkit_gts::gts_uri;

use super::*;

const OWNED_NODE_REF: &str = gts_uri!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~");

fn schema(type_id: &str) -> Value {
    let (_, text) = PERSON_TYPES
        .iter()
        .find(|(id, _)| *id == type_id)
        .expect("a person type");
    serde_json::from_str(text).expect("a JSON schema")
}

fn payload_validator(type_id: &str) -> jsonschema::Validator {
    let payload = schema(type_id)["allOf"][1]["properties"]["payload"].clone();
    jsonschema::validator_for(&payload).expect("a valid payload schema")
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
fn a_payload_with_one_known_property_fits() {
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
    for (type_id, payload) in fitting {
        assert!(
            payload_validator(type_id).is_valid(&payload),
            "{type_id}: {payload}"
        );
    }
}

#[test]
fn a_payload_that_breaks_the_type_does_not_fit() {
    let misfits = [
        (PREFERENCES_TYPE, json!({}), "no property"),
        (
            PREFERENCES_TYPE,
            json!({ "tone_preference": "concise", "units": "si_metric" }),
            "two properties",
        ),
        (
            PREFERENCES_TYPE,
            json!({ "favourite_snack": "crisps" }),
            "unknown property",
        ),
        (
            PREFERENCES_TYPE,
            json!({ "tone_preference": "shouty" }),
            "value outside the enum",
        ),
        (
            SKILLS_TYPE,
            json!({ "skills": { "level": "expert" } }),
            "entry without its main field",
        ),
        (
            SKILLS_TYPE,
            json!({ "skills": "Python" }),
            "entry as plain text",
        ),
        (
            ROLES_TYPE,
            json!({ "work_history": { "organization": "Acme", "start_year": 1750 } }),
            "year before 1800",
        ),
    ];
    for (type_id, payload, case) in misfits {
        assert!(
            !payload_validator(type_id).is_valid(&payload),
            "{case}: {payload}"
        );
    }
}
