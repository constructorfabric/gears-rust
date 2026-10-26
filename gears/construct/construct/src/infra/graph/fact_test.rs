//! A fact's node: its type, payload, origin and record.

use construct_sdk::gts::{IDENTITY_TYPE, SKILL_TYPE};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::fact::fact_node;
use crate::domain::error::DomainError;
use crate::domain::plan::{Origin, Plan, Property};
use crate::test_support::{add_value, by_a_person, connector_plan};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-20T12:00:00Z", &Rfc3339).unwrap()
}

fn skills() -> Value {
    json!({ "name": "Rust", "level": "expert" })
}

fn node_of(plan: &Plan) -> graph_storage_sdk::models::NodeSpec {
    let step = &plan.steps[0];
    let property = &step.change.properties()[0];
    fact_node(plan, step, "construct:t:fact", property, now()).expect("a fact node")
}

#[test]
fn a_records_fact_names_the_connector_its_record_and_when_it_was_observed() {
    let plan = connector_plan(vec![add_value("skill", "skills", skills())]);

    let node = node_of(&plan);

    assert_eq!(node.type_id, SKILL_TYPE);
    assert_eq!(node.expected_version, Some(0), "claims its new key");
    let payload = node.payload.unwrap();
    assert_eq!(payload["skills"], skills());
    assert_eq!(
        payload["origin"]["produced_by"]["subject_id"],
        "connector-a"
    );
    assert_eq!(
        payload["origin"]["method"],
        construct_sdk::gts::CHAT_MESSAGE_TYPE
    );
    assert_eq!(payload["origin"]["produced_at"], "1970-01-01T00:00:00Z");
    assert_eq!(payload["origin"]["confidence"], json!(0.9_f32));
    assert_eq!(
        payload["record"],
        json!({ "connector": "connector-a", "provenance": "chat_engine/thread-42/msg-7", "version": "v1" })
    );
    assert_eq!(payload.as_object().unwrap().len(), 3);
}

#[test]
fn an_agents_fact_names_the_subject_and_the_agent_as_model() {
    let agent = Uuid::new_v4();
    let mut plan = connector_plan(vec![add_value("identity", "full_name", json!("Ada"))]);
    plan.origin = Origin::Agent { agent };

    let payload = node_of(&plan).payload.unwrap();

    assert_eq!(
        payload["origin"]["produced_by"]["subject_id"],
        plan.subject_id.to_string()
    );
    assert_eq!(payload["origin"]["method"], "mcp.manage_facts");
    assert_eq!(payload["origin"]["model"], agent.to_string());
    assert_eq!(payload["origin"]["produced_at"], "2026-10-20T12:00:00Z");
    assert!(payload.get("record").is_none());
}

#[test]
fn a_persons_fact_names_the_person_and_carries_no_confidence() {
    let reviewer = Uuid::new_v4();
    let administrator = Uuid::new_v4();
    for (origin, who, method) in [
        (Origin::Reviewer { reviewer }, reviewer, "review.correction"),
        (
            Origin::Administrator { administrator },
            administrator,
            "admin.edit",
        ),
    ] {
        let plan = by_a_person(
            connector_plan(vec![add_value("identity", "full_name", json!("Ada"))]),
            origin,
        );

        let node = node_of(&plan);

        assert_eq!(node.type_id, IDENTITY_TYPE);
        let payload = node.payload.unwrap();
        assert_eq!(
            payload["origin"]["produced_by"]["subject_id"],
            who.to_string()
        );
        assert_eq!(payload["origin"]["method"], method);
        assert!(payload["origin"].get("confidence").is_none());
        assert_eq!(payload.as_object().unwrap().len(), 2);
    }
}

#[test]
fn a_step_of_no_known_category_is_a_defect() {
    let plan = connector_plan(vec![add_value("hobby", "chess", json!("yes"))]);
    let step = &plan.steps[0];

    let result = fact_node(&plan, step, "k", &Property::new("chess", "yes"), now());

    assert!(
        matches!(result, Err(DomainError::Internal(_))),
        "{result:?}"
    );
}
