use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use graph_storage_sdk::gts::{
    ATTRIBUTE_BASE_TYPE, NODE_BASE_TYPE, OWNED_NODE_TYPE, PROVENANCE_ATTRIBUTE_TYPE,
};
use jsonschema::{Retrieve, Uri, Validator};
use serde_json::{Value, json};

use super::*;
use crate::gts::PREFERENCE_TYPE;

const GRAPH_STORAGE_SCHEMAS: [(&str, &str); 4] = [
    (
        NODE_BASE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.node.v1~.schema.json"
        ),
    ),
    (
        OWNED_NODE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~.schema.json"
        ),
    ),
    (
        ATTRIBUTE_BASE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.attribute.v1~.schema.json"
        ),
    ),
    (
        PROVENANCE_ATTRIBUTE_TYPE,
        include_str!(
            "../../../graph-storage/graph-storage/schemas/gts.cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~.schema.json"
        ),
    ),
];

struct GraphStorageSchemas;

impl Retrieve for GraphStorageSchemas {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        GRAPH_STORAGE_SCHEMAS
            .iter()
            .find(|(id, _)| format!("gts://{id}") == uri.as_str())
            .map(|(_, text)| serde_json::from_str(text).expect("a JSON schema"))
            .ok_or_else(|| format!("unresolved schema reference `{uri}`").into())
    }
}

static NODE_VALIDATORS: LazyLock<BTreeMap<&str, Validator>> = LazyLock::new(|| {
    PERSON_TYPES
        .iter()
        .map(|person_type| {
            let validator = jsonschema::options()
                .with_retriever(GraphStorageSchemas)
                .build(&schema(person_type.type_id))
                .expect("the type compiles on graph storage's base schemas");
            (person_type.type_id, validator)
        })
        .collect()
});

fn schema(type_id: &str) -> Value {
    let person_type = PERSON_TYPES
        .iter()
        .find(|person_type| person_type.type_id == type_id)
        .expect("a person type");
    serde_json::from_str(person_type.schema).expect("a JSON schema")
}

fn payload_schema(type_id: &str) -> Value {
    schema(type_id)["allOf"][1]["properties"]["payload"].clone()
}

fn type_of(property: &str) -> &'static str {
    PROPERTIES
        .iter()
        .find(|listed| listed.name == property)
        .map(|listed| listed.type_id)
        .expect("a listed property")
}

fn fact_properties(type_id: &str) -> BTreeSet<String> {
    payload_schema(type_id)["properties"]
        .as_object()
        .expect("payload properties")
        .keys()
        .filter(|name| *name != "origin" && *name != "record")
        .cloned()
        .collect()
}

fn node(type_id: &str, payload: &Value) -> Value {
    json!({ "node_key": "construct:fact", "type": type_id, "payload": payload })
}

fn fits(type_id: &str, payload: &Value) -> bool {
    NODE_VALIDATORS[type_id].is_valid(&node(type_id, payload))
}

fn origin() -> Value {
    json!({ "produced_by": { "subject_id": "connector" }, "produced_at": "2026-10-07T00:00:00Z" })
}

fn record() -> Value {
    json!({ "connector": "chat_engine", "provenance": "chat_engine/thread-42/msg-7", "version": "v1" })
}

fn fact(property: &str, value: &Value) -> Value {
    json!({ property: value, "origin": origin() })
}

fn value_fits(property: &str, value: &Value) -> bool {
    fits(type_of(property), &fact(property, value))
}

fn samples() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "google_scholar_id",
            json!("JicYPdAAAAAJ"),
            json!("Jane Doe"),
        ),
        (
            "openalex_author_id",
            json!("A5012345678"),
            json!("5012345678"),
        ),
        (
            "orcid_id",
            json!("0000-0002-1825-0097"),
            json!("0000-0002-1825"),
        ),
        (
            "public_profiles",
            json!("https://example.org/jane"),
            json!("example.org/jane"),
        ),
        (
            "scopus_author_id",
            json!("57190000000"),
            json!("A57190000000"),
        ),
        ("timezone", json!("Europe/Berlin"), json!("Berlin time")),
        ("answer_language", json!("en-GB"), json!("English")),
        ("ui_language", json!("de"), json!("German")),
        ("dialect", json!("bg-BG"), json!("bulgarian")),
        ("languages", json!("es"), json!("Spanish")),
        ("school_grade", json!("grade_9"), json!("Grade 9")),
        ("average_session_length", json!(45), json!(0)),
        ("captions", json!(true), json!("yes")),
        ("dyslexia_friendly", json!(true), json!("yes")),
        ("high_contrast", json!(false), json!("off")),
        ("reduced_motion", json!(true), json!(1)),
        ("challenge_starting_level", json!("warm_up"), json!("hard")),
        ("chunk_size", json!("short_5_10m"), json!("short")),
        ("citation_style", json!("apa"), json!("AMA")),
        ("collaboration_style", json!("reviewer"), json!("leader")),
        ("communication_channel", json!("chat"), json!("email")),
        ("explainability", json!("on_request"), json!("sometimes")),
        ("feedback_style", json!("socratic"), json!("harsh")),
        ("group_vs_individual", json!("group"), json!("both")),
        ("hint_depth", json!("light"), json!("none")),
        ("interface_layout", json!("compact"), json!("dense")),
        ("jargon_tolerance", json!("avoid_jargon"), json!("high")),
        (
            "local_environment_constraint",
            json!("ipad_only"),
            json!("android_only"),
        ),
        ("math_notation", json!("dy_dx"), json!("leibniz")),
        (
            "notification_type",
            json!("weekly_recap"),
            json!("daily_recap"),
        ),
        ("preferred_format", json!("bullets"), json!("podcasts")),
        ("presentation_order", json!("theory_first"), json!("random")),
        ("programming_language_target", json!("python"), json!("r")),
        ("reading_level", json!("advanced"), json!("expert")),
        (
            "technology_constraint",
            json!("mobile_only"),
            json!("tablet_only"),
        ),
        ("tone_preference", json!("concise"), json!("professional")),
        ("units", json!("imperial"), json!("metric")),
        ("education_level", json!("higher_ed"), json!("university")),
        ("first_publication_year", json!(1800), json!(1799)),
        ("latest_publication_year", json!(2024), json!(1799)),
        ("h_index", json!(0), json!(-1)),
        ("i10_index", json!(12), json!(-1)),
        ("total_citations", json!(0), json!(-1)),
        ("total_publications", json!(3), json!(-1)),
        ("mean_citedness_2y", json!(1.5), json!(-0.1)),
        (
            "focus_windows",
            json!({ "day": "mon", "start": "09:00", "end": "11:30" }),
            json!({ "days": ["mon", "tue"], "start": "09:00", "end": "11:30" }),
        ),
        (
            "study_window",
            json!({ "day": "sat", "start": "18:00", "end": "20:00" }),
            json!({ "day": "sat", "start": "6pm", "end": "20:00" }),
        ),
        (
            "time_availability_pattern",
            json!({ "day": "wed", "start": "07:00", "end": "08:00" }),
            json!({ "day": "someday", "start": "07:00", "end": "08:00" }),
        ),
        (
            "work_history",
            json!({ "organization": "Acme", "start_year": 1800 }),
            json!({ "organization": "Acme", "start_year": 1799 }),
        ),
        (
            "affiliation_history",
            json!({ "organization": "ETH Zurich", "country": "CH" }),
            json!({ "organization": "ETH Zurich", "country": "Switzerland" }),
        ),
        (
            "board_memberships",
            json!({ "organization": "IEEE", "role": "Member", "start_year": 2019 }),
            json!({ "organization": "IEEE", "start_year": 1799 }),
        ),
        (
            "roles_and_affiliations",
            json!({ "role": "Reviewer", "organization": "ACM" }),
            json!({ "organization": "ACM" }),
        ),
        (
            "activity_by_year",
            json!({ "year": 2020, "works": 3, "citations": 40 }),
            json!({ "year": 1799, "works": 3 }),
        ),
        (
            "skills",
            json!({ "name": "Python", "level": "expert", "years": 6 }),
            json!({ "name": "Python", "level": "guru" }),
        ),
        (
            "publications",
            json!({ "title": "Attention Is All You Need", "doi": "10.48550/arXiv.1706.03762" }),
            json!({ "title": "Attention Is All You Need", "doi": "arXiv.1706.03762" }),
        ),
        (
            "research_areas",
            json!({ "name": "Computational biology", "weight": 0.7 }),
            json!({ "name": "Computational biology", "weight": 1.5 }),
        ),
        (
            "awards",
            json!({ "name": "Best paper", "year": 2021 }),
            json!({ "year": 2021 }),
        ),
    ]
}

fn sample(property: &str) -> Value {
    samples()
        .into_iter()
        .find(|(name, _, _)| *name == property)
        .map_or_else(|| panic!("no sample of {property}"), |(_, valid, _)| valid)
}

fn sample_fact(type_id: &str) -> Value {
    samples()
        .into_iter()
        .find(|(name, _, _)| type_of(name) == type_id)
        .map(|(name, valid, _)| fact(name, &valid))
        .expect("a sample of the type")
}

/// One value in a payload: a fact property, a field of an entry, or a member of `record`.
struct Field {
    type_id: &'static str,
    place: Place,
    schema: Value,
}

enum Place {
    Property(&'static str),
    Entry(&'static str, String),
    Record(String),
}

impl Field {
    fn name(&self) -> String {
        match &self.place {
            Place::Property(property) => (*property).to_owned(),
            Place::Entry(property, field) => format!("{property}.{field}"),
            Place::Record(member) => format!("record.{member}"),
        }
    }

    fn takes(&self, value: Value) -> bool {
        let payload = match &self.place {
            Place::Property(property) => fact(property, &value),
            Place::Entry(property, field) => {
                let mut entry = sample(property);
                entry[field] = value;
                fact(property, &entry)
            }
            Place::Record(member) => {
                let mut payload = sample_fact(self.type_id);
                payload["record"] = record();
                payload["record"][member] = value;
                payload
            }
        };
        fits(self.type_id, &payload)
    }
}

fn members(schema: &Value) -> Vec<(String, Value)> {
    schema["properties"]
        .as_object()
        .map(|members| members.clone().into_iter().collect())
        .unwrap_or_default()
}

fn fields() -> Vec<Field> {
    let mut found = Vec::new();
    for property in PROPERTIES {
        let schema =
            property_schema(&schema(property.type_id), property.name).expect("a property schema");
        found.extend(members(&schema).into_iter().map(|(field, schema)| Field {
            type_id: property.type_id,
            place: Place::Entry(property.name, field),
            schema,
        }));
        found.push(Field {
            type_id: property.type_id,
            place: Place::Property(property.name),
            schema,
        });
    }
    for person_type in PERSON_TYPES {
        let record = &payload_schema(person_type.type_id)["properties"]["record"];
        found.extend(members(record).into_iter().map(|(member, schema)| Field {
            type_id: person_type.type_id,
            place: Place::Record(member),
            schema,
        }));
    }
    found
}

const LONGEST_TEXT: usize = 2048;

/// Text of `length` characters that `pattern` takes, or `None` when the pattern itself bounds the length.
fn text_of_length(pattern: &str, length: usize) -> Option<String> {
    let filled = |prefix: &str, filler: &str| {
        Some(format!("{prefix}{}", filler.repeat(length - prefix.len())))
    };
    match pattern {
        "" | "\\S" => filled("", "x"),
        "^[A-Za-z0-9_-]+$" | "^[a-z0-9_]+$" => filled("", "a"),
        "^\\d+$" => filled("", "1"),
        "^A\\d+$" => filled("A", "1"),
        "^W\\d+$" => filled("W", "1"),
        "^https?://" => filled("https://", "a"),
        "^10\\.\\d{4,9}/\\S+$" => filled("10.1234/", "x"),
        "^(UTC|[A-Za-z_]+(/[A-Za-z0-9_+-]+){1,2})$" => filled("Europe/", "a"),
        "^([01]\\d|2[0-3]):[0-5]\\d$"
        | "^[a-z]{2,3}(-[A-Z][a-z]{3})?(-[A-Z]{2})?$"
        | "^[a-z]{2,3}(-[A-Z]{2})?$"
        | "^[A-Z]{2}$"
        | "^\\d{4}-\\d{4}-\\d{4}-\\d{3}[\\dX]$" => None,
        other => panic!("no text is built for the pattern {other}"),
    }
}

#[test]
fn each_schema_names_its_type_and_derives_from_the_owned_node() {
    for person_type in PERSON_TYPES {
        let document = schema(person_type.type_id);
        assert_eq!(document["$id"], format!("gts://{}", person_type.type_id));
        assert_eq!(
            document["allOf"][0]["$ref"],
            format!("gts://{OWNED_NODE_TYPE}")
        );
    }
}

#[test]
fn the_origin_is_required_and_is_graph_storage_provenance() {
    for person_type in PERSON_TYPES {
        let payload = payload_schema(person_type.type_id);
        assert_eq!(
            payload["required"],
            json!(["origin"]),
            "{}",
            person_type.type_id
        );
        assert_eq!(
            payload["properties"]["origin"]["$ref"],
            format!("gts://{PROVENANCE_ATTRIBUTE_TYPE}"),
            "{}",
            person_type.type_id
        );
    }
}

#[test]
fn a_whole_node_of_each_type_fits_its_chain_with_a_real_origin() {
    for person_type in PERSON_TYPES {
        let payload = sample_fact(person_type.type_id);
        let mut keyless = node(person_type.type_id, &payload);
        keyless.as_object_mut().expect("a node").remove("node_key");
        let mut timeless = payload.clone();
        timeless["origin"]
            .as_object_mut()
            .expect("an origin")
            .remove("produced_at");

        assert!(fits(person_type.type_id, &payload), "{payload}");
        assert!(
            !NODE_VALIDATORS[person_type.type_id].is_valid(&keyless),
            "a node without its key"
        );
        assert!(
            !fits(person_type.type_id, &timeless),
            "an origin without its time"
        );
    }
}

#[test]
fn every_fact_property_has_a_cardinality_and_only_those() {
    for person_type in PERSON_TYPES {
        let listed: BTreeSet<String> = PROPERTIES
            .iter()
            .filter(|listed| listed.type_id == person_type.type_id)
            .map(|listed| listed.name.to_owned())
            .collect();
        assert_eq!(
            listed,
            fact_properties(person_type.type_id),
            "{}",
            person_type.type_id
        );
    }
}

#[test]
fn each_property_holds_one_value_unless_a_person_can_have_several() {
    let many: BTreeSet<&str> = [
        "name_variants",
        "public_mentions",
        "public_profiles",
        "assistive_tools",
        "communication_channel",
        "current_focus_areas",
        "focus_windows",
        "local_environment_constraint",
        "long_term_goals",
        "notification_type",
        "preferred_format",
        "short_term_goals",
        "study_window",
        "technology_constraint",
        "time_availability_pattern",
        "time_constraint",
        "affiliation_history",
        "board_memberships",
        "group_memberships",
        "roles_and_affiliations",
        "work_history",
        "activity_by_year",
        "awards",
        "languages",
        "publications",
        "research_areas",
        "skills",
    ]
    .into_iter()
    .collect();

    for property in PROPERTIES {
        let expected = if many.contains(property.name) {
            Cardinality::Many
        } else {
            Cardinality::One
        };
        assert_eq!(property.cardinality, expected, "{}", property.name);
    }
}

#[test]
fn a_payload_holds_exactly_one_fact_and_its_origin() {
    let two_facts = json!({
        "tone_preference": "concise",
        "units": "si_metric",
        "origin": origin(),
    });
    let misfits = [
        (json!({ "tone_preference": "concise" }), "no origin"),
        (json!({ "origin": origin() }), "origin without a fact"),
        (two_facts, "two facts"),
        (
            fact("favourite_snack", &json!("crisps")),
            "unknown property",
        ),
    ];

    for (payload, case) in misfits {
        assert!(!fits(PREFERENCE_TYPE, &payload), "{case}: {payload}");
    }
    assert!(fits(
        PREFERENCE_TYPE,
        &fact("tone_preference", &json!("concise"))
    ));
}

#[test]
fn a_fact_from_a_record_names_the_record_beside_its_origin() {
    let from_record = json!({
        "tone_preference": "concise",
        "origin": origin(),
        "record": record(),
    });
    let two_facts_and_no_record = json!({
        "tone_preference": "concise",
        "units": "si_metric",
        "origin": origin(),
    });
    let record_without_a_fact = json!({ "origin": origin(), "record": record() });
    let record_without_its_version = json!({
        "tone_preference": "concise",
        "origin": origin(),
        "record": { "connector": "chat_engine", "provenance": "chat_engine/thread-42/msg-7" },
    });

    assert!(fits(PREFERENCE_TYPE, &from_record));
    for (payload, case) in [
        (two_facts_and_no_record, "two facts"),
        (record_without_a_fact, "a record without a fact"),
        (record_without_its_version, "a record without its version"),
    ] {
        assert!(!fits(PREFERENCE_TYPE, &payload), "{case}: {payload}");
    }
}

#[test]
fn every_type_takes_the_same_record() {
    let records: BTreeSet<String> = PERSON_TYPES
        .iter()
        .map(|person_type| payload_schema(person_type.type_id)["properties"]["record"].to_string())
        .collect();

    assert_eq!(records.len(), 1, "{records:?}");
}

#[test]
fn every_property_schema_stands_alone() {
    for property in PROPERTIES {
        let schema =
            property_schema(&schema(property.type_id), property.name).expect("a property schema");

        assert!(
            !schema.to_string().contains("$ref"),
            "{}: {schema}",
            property.name
        );
        jsonschema::validator_for(&schema).expect("a schema that compiles alone");
    }
    for window in ["focus_windows", "study_window", "time_availability_pattern"] {
        let schema = property_schema(&schema(PREFERENCE_TYPE), window).expect("a window");
        assert!(
            jsonschema::is_valid(&schema, &sample(window)),
            "{window}: {schema}"
        );
    }
}

#[test]
fn each_constrained_value_fits_its_form_and_nothing_else() {
    for (property, valid, invalid) in samples() {
        assert!(value_fits(property, &valid), "{property}: {valid}");
        assert!(!value_fits(property, &invalid), "{property}: {invalid}");
    }
}

#[test]
fn every_text_value_has_a_length_limit_it_enforces() {
    for field in fields() {
        let schema = &field.schema;
        if schema["type"] != "string" || schema.get("enum").is_some() {
            continue;
        }
        let pattern = schema["pattern"].as_str().unwrap_or_default();
        let Some(probe) = text_of_length(pattern, 40) else {
            continue;
        };
        let limit = schema["maxLength"]
            .as_u64()
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or_else(|| panic!("{} has no maxLength", field.name()));

        assert!(
            field.takes(json!(probe)),
            "{} refuses {probe}",
            field.name()
        );
        assert!(
            field.takes(json!(text_of_length(pattern, limit))),
            "{} refuses text at its limit",
            field.name()
        );
        assert!(
            !field.takes(json!(text_of_length(pattern, limit + 1))),
            "{} takes text over its limit",
            field.name()
        );
        assert!(
            !field.takes(json!(text_of_length(pattern, LONGEST_TEXT + 1))),
            "{} takes text longer than any field may hold",
            field.name()
        );
        if pattern == "\\S" {
            assert!(
                !field.takes(json!("   ")),
                "{} takes blank text",
                field.name()
            );
        }
    }
}

/// The minimum, the maximum, and a value just below and just above them, of a numeric field.
fn bounds(field: &Field) -> Option<[Value; 4]> {
    let schema = &field.schema;
    match schema["type"].as_str()? {
        "integer" => {
            let bound = |name: &str| {
                schema[name]
                    .as_i64()
                    .unwrap_or_else(|| panic!("{} has no {name}", field.name()))
            };
            let (lowest, highest) = (bound("minimum"), bound("maximum"));
            Some([
                json!(lowest),
                json!(highest),
                json!(lowest - 1),
                json!(highest + 1),
            ])
        }
        "number" => {
            let bound = |name: &str| {
                schema[name]
                    .as_f64()
                    .unwrap_or_else(|| panic!("{} has no {name}", field.name()))
            };
            let (lowest, highest) = (bound("minimum"), bound("maximum"));
            Some([
                json!(lowest),
                json!(highest),
                json!(lowest - 0.5),
                json!(highest + 0.5),
            ])
        }
        _ => None,
    }
}

#[test]
fn every_number_has_bounds_it_enforces() {
    for field in fields() {
        let Some([lowest, highest, below, above]) = bounds(&field) else {
            continue;
        };

        assert!(field.takes(lowest), "{} refuses its minimum", field.name());
        assert!(field.takes(highest), "{} refuses its maximum", field.name());
        assert!(
            !field.takes(below),
            "{} takes a value below its minimum",
            field.name()
        );
        assert!(
            !field.takes(above),
            "{} takes a value above its maximum",
            field.name()
        );
    }
}
