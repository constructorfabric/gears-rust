use std::mem::discriminant;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::catalog::PropertyCatalog;
use super::draft::{ADD, PlanDraft, REMOVE, REPLACE, ToolRefusal};
use crate::domain::model_client::ToolCall;
use crate::domain::plan::{NewFact, PlanOrigin, RecordOrigin, Step};
use crate::domain::profile::{Category, Fact, Profile, ProfileVersion};
use crate::domain::record_intake::{Envelope, ReceivedRecord};
use crate::test_support::chat_record;

const TENANT: Uuid = Uuid::from_u128(0x6b1f_0c2a_93d4_4e8f_a1b2_c3d4_e5f6_0718);
const SUBJECT: Uuid = Uuid::from_u128(0x2c9e_7a41_5b3f_4d6e_8f90_1a2b_3c4d_5e6f);

fn catalog() -> PropertyCatalog {
    PropertyCatalog::load().expect("the catalog")
}

fn profile() -> Profile {
    Profile {
        version: ProfileVersion(7),
        facts: vec![
            Fact {
                node_key: format!("construct:{TENANT}:5d2c8a3e-0f41-4b7a-9c6d-2e8f1a3b4c5d"),
                category: Category::Roles,
                property: "role".to_owned(),
                value: json!("teacher"),
            },
            Fact {
                node_key: format!("construct:{TENANT}:a7e3f9b1-2c4d-4e6f-8a0b-1c3d5e7f9a2b"),
                category: Category::Skills,
                property: "skills".to_owned(),
                value: json!({ "name": "Python" }),
            },
            Fact {
                node_key: format!("construct:{TENANT}:c4b6d8e0-1f3a-4c5e-9b7d-0e2f4a6c8e1b"),
                category: Category::Identity,
                property: "full_name".to_owned(),
                value: json!("Jane Doe"),
            },
        ],
    }
}

fn origin() -> PlanOrigin {
    PlanOrigin::Record(RecordOrigin {
        connector: "chat_engine".to_owned(),
        type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
        provenance: "chat_engine/thread-42/msg-7".to_owned(),
        version: "v1".to_owned(),
        observed_at: OffsetDateTime::parse("2026-09-10T09:13:05Z", &Rfc3339).expect("a time"),
    })
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

fn node_key(number: usize) -> String {
    profile().facts[number - 1].node_key.clone()
}

fn is_new_key(key: &str) -> bool {
    key.starts_with(&format!("construct:{TENANT}:"))
        && profile().facts.iter().all(|fact| fact.node_key != key)
}

#[test]
fn the_profile_is_shown_as_numbered_values_of_plain_properties() {
    let catalog = catalog();
    let draft = PlanDraft::new(&catalog, profile());

    assert_eq!(
        draft.numbered_profile(),
        "1. role (roles): teacher\n2. skills (skills): {\"name\":\"Python\"}\n3. full_name (identity): Jane Doe"
    );
}

#[test]
fn a_replace_is_one_step_with_the_old_key_and_a_new_one() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    let result = draft.apply(&call(
        REPLACE,
        json!({ "number": 1, "value": "school principal", "confidence": 0.9 }),
    ));
    let plan = draft.finish(TENANT, SUBJECT, origin());

    assert_eq!(result, Ok("replaced 1".to_owned()));
    let [
        Step::Replace {
            old_key,
            node_key: new_key,
            fact,
            confidence,
        },
    ] = plan.steps.as_slice()
    else {
        panic!("one replace step: {:?}", plan.steps);
    };
    assert_eq!(*old_key, node_key(1));
    assert!(is_new_key(new_key), "{new_key}");
    assert_eq!(
        *fact,
        NewFact {
            category: Category::Roles,
            property: "role".to_owned(),
            value: json!("school principal"),
        }
    );
    assert!((confidence - 0.9).abs() < f64::EPSILON);
}

#[test]
fn an_add_and_a_remove_become_steps_of_one_plan_on_the_profile_version() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    draft
        .apply(&call(
            ADD,
            json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.8 }),
        ))
        .expect("added");
    draft
        .apply(&call(REMOVE, json!({ "number": 2, "confidence": 0.7 })))
        .expect("removed");
    let plan = draft.finish(TENANT, SUBJECT, origin());

    assert_eq!(
        (
            plan.tenant_id,
            plan.subject_id,
            &plan.origin,
            plan.profile_version
        ),
        (TENANT, SUBJECT, &origin(), ProfileVersion(7))
    );
    let [
        Step::Add {
            node_key: added,
            fact,
            ..
        },
        Step::Remove {
            node_key: removed, ..
        },
    ] = plan.steps.as_slice()
    else {
        panic!("an add and a remove: {:?}", plan.steps);
    };
    assert!(is_new_key(added), "{added}");
    assert_eq!(
        *fact,
        NewFact {
            category: Category::Skills,
            property: "skills".to_owned(),
            value: json!({ "name": "Rust" }),
        }
    );
    assert_eq!(*removed, node_key(2));
}

#[test]
fn every_new_value_gets_its_own_new_key_also_when_it_returns() {
    let catalog = catalog();
    let add_rust = call(
        ADD,
        json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.8 }),
    );
    let mut keys = Vec::new();
    for _ in 0..2 {
        let mut draft = PlanDraft::new(&catalog, profile());
        draft.apply(&add_rust).expect("added");
        draft.apply(&add_rust).expect("added again");
        for step in draft.finish(TENANT, SUBJECT, origin()).steps {
            let Step::Add { node_key, .. } = step else {
                panic!("an add");
            };
            keys.push(node_key);
        }
    }

    assert!(keys.iter().all(|key| is_new_key(key)), "{keys:?}");
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 4);
}

#[test]
fn a_call_that_breaks_a_rule_is_refused_and_adds_no_step() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());
    draft
        .apply(&call(REMOVE, json!({ "number": 2, "confidence": 0.7 })))
        .expect("removed");
    draft
        .apply(&call(
            ADD,
            json!({ "property": "timezone", "value": "UTC", "confidence": 0.9 }),
        ))
        .expect("added");
    let cases = [
        (
            call("merge", json!({})),
            ToolRefusal::UnknownTool(String::new()),
        ),
        (
            call(
                ADD,
                json!({ "property": "skills", "value": { "name": "Go" } }),
            ),
            ToolRefusal::BadArguments(String::new()),
        ),
        (
            call(
                ADD,
                json!({ "property": "skills", "value": { "name": "Go" }, "confidence": 1.5 }),
            ),
            ToolRefusal::BadConfidence,
        ),
        (
            call(
                REPLACE,
                json!({ "number": 9, "value": "x", "confidence": 0.5 }),
            ),
            ToolRefusal::UnknownNumber(0),
        ),
        (
            call(REMOVE, json!({ "number": 2, "confidence": 0.5 })),
            ToolRefusal::NumberUsed(0),
        ),
        (
            call(
                ADD,
                json!({ "property": "favourite_colour", "value": "teal", "confidence": 0.5 }),
            ),
            ToolRefusal::UnknownProperty {
                property: String::new(),
                known: String::new(),
            },
        ),
        (
            call(
                ADD,
                json!({ "property": "orcid_id", "value": "orcid of Jane", "confidence": 0.5 }),
            ),
            ToolRefusal::ValueDoesNotFit {
                property: String::new(),
                reason: String::new(),
            },
        ),
        (
            call(
                REPLACE,
                json!({ "number": 3, "value": "", "confidence": 0.5 }),
            ),
            ToolRefusal::ValueDoesNotFit {
                property: String::new(),
                reason: String::new(),
            },
        ),
        (
            call(
                ADD,
                json!({ "property": "role", "value": "dean", "confidence": 0.5 }),
            ),
            ToolRefusal::SecondValue {
                property: String::new(),
                number: 0,
            },
        ),
        (
            call(
                ADD,
                json!({ "property": "timezone", "value": "Europe/Berlin", "confidence": 0.5 }),
            ),
            ToolRefusal::SecondValueInPlan {
                property: String::new(),
            },
        ),
    ];

    for (refused, expected) in &cases {
        let refusal = draft.apply(refused).expect_err(&refused.name);
        assert_eq!(
            discriminant(&refusal),
            discriminant(expected),
            "{}: {refusal}",
            refused.arguments
        );
    }
    assert_eq!(draft.finish(TENANT, SUBJECT, origin()).steps.len(), 2);
}

#[test]
fn a_refusal_tells_the_model_what_to_do_instead() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    let second_role = draft
        .apply(&call(
            ADD,
            json!({ "property": "role", "value": "dean", "confidence": 0.5 }),
        ))
        .expect_err("one role");
    let unknown = draft
        .apply(&call(
            ADD,
            json!({ "property": "favourite_colour", "value": "teal", "confidence": 0.5 }),
        ))
        .expect_err("no such property");

    assert_eq!(
        second_role.to_string(),
        "`role` holds one value, and value 1 is it; use replace 1"
    );
    assert!(unknown.to_string().contains("research_areas"), "{unknown}");
}

#[test]
fn a_single_value_can_be_added_after_its_old_value_is_removed() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    draft
        .apply(&call(REMOVE, json!({ "number": 1, "confidence": 0.9 })))
        .expect("removed");

    assert_eq!(
        draft.apply(&call(
            ADD,
            json!({ "property": "role", "value": "dean", "confidence": 0.9 })
        )),
        Ok("added role".to_owned())
    );
}

#[test]
fn the_model_input_holds_no_id() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());
    let mut shown = vec![draft.numbered_profile()];
    shown.extend(
        draft
            .tools()
            .iter()
            .map(|tool| format!("{} {} {}", tool.name, tool.description, tool.parameters)),
    );
    for tool_call in [
        call(
            REPLACE,
            json!({ "number": 1, "value": "dean", "confidence": 0.9 }),
        ),
        call(REMOVE, json!({ "number": 1, "confidence": 0.9 })),
        call(REMOVE, json!({ "number": 4, "confidence": 0.9 })),
        call(
            ADD,
            json!({ "property": "full_name", "value": "J. Doe", "confidence": 0.9 }),
        ),
        call(
            ADD,
            json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.9 }),
        ),
    ] {
        shown.push(match draft.apply(&tool_call) {
            Ok(result) => result,
            Err(refusal) => refusal.to_string(),
        });
    }
    let ids: Vec<String> = profile()
        .facts
        .iter()
        .map(|fact| fact.node_key.clone())
        .chain([
            TENANT.to_string(),
            SUBJECT.to_string(),
            "construct:".to_owned(),
        ])
        .collect();

    for text in &shown {
        for id in &ids {
            assert!(!text.contains(id.as_str()), "{text} shows {id}");
        }
        assert!(!text.contains("gts."), "{text} shows a type id");
    }
}

#[test]
fn the_record_origin_comes_from_the_received_record() {
    let record = ReceivedRecord {
        tenant_id: TENANT,
        connector: "chat_engine".to_owned(),
        envelope: Envelope {
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            provenance: "chat_engine/thread-42/msg-7".to_owned(),
            version: "v1".to_owned(),
            subject_id: Some(SUBJECT),
        },
        record: chat_record(SUBJECT, "chat_engine/thread-42/msg-7", "v1"),
    };

    assert_eq!(
        PlanOrigin::Record(RecordOrigin::of(&record).expect("an origin")),
        origin()
    );
}
