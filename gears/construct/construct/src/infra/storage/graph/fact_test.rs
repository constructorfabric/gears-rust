//! A fact's node: its type, payload, origin and record; and a stored node read
//! back as a fact.

use construct_sdk::gts::{IDENTITY_TYPE, PROFILE_ROOT_TYPE, SKILL_TYPE};
use graph_storage_sdk::models::{ElementEnvelope, GraphRevision, NodeSpec, NodeView, Subject};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::fact::{fact_node, fact_of};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin, Plan, Property, Step};
use crate::domain::profile::Category;
use crate::test_support::{
    add_entity, add_value, by_a_person, connector_plan, remove_entity, replace_property,
};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-20T12:00:00Z", &Rfc3339).unwrap()
}

fn skills() -> Value {
    json!({ "name": "Rust", "level": "expert" })
}

fn node_of(plan: &Plan) -> NodeSpec {
    fact_node(plan, &plan.steps[0], now())
        .expect("a fact node")
        .expect("a step that stores a fact")
}

fn internal(result: Result<Option<NodeSpec>, DomainError>) -> String {
    match result {
        Err(DomainError::Internal(message)) => message,
        other => panic!("an internal error, got {other:?}"),
    }
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
    assert_eq!(
        payload["record"],
        json!({ "connector": "connector-a", "provenance": "chat_engine/thread-42/msg-7", "version": "v1" })
    );
    assert_eq!(payload.as_object().unwrap().len(), 3);
}

#[test]
fn a_confidence_is_stored_as_the_decimal_the_model_gave() {
    for (given, stored) in [
        (0.9_f32, json!(0.9)),
        (0.1, json!(0.1)),
        (0.3, json!(0.3)),
        (0.7, json!(0.7)),
        (0.0, json!(0.0)),
        (1.0, json!(1.0)),
    ] {
        let mut plan = connector_plan(vec![add_value("skill", "skills", skills())]);
        plan.steps[0].confidence = Some(given);

        let payload = node_of(&plan).payload.unwrap();

        assert_eq!(payload["origin"]["confidence"], stored, "{given}");
    }
}

#[test]
fn a_confidence_that_is_no_number_is_a_defect() {
    let mut plan = connector_plan(vec![add_value("skill", "skills", skills())]);
    plan.steps[0].confidence = Some(f32::NAN);

    let message = internal(fact_node(&plan, &plan.steps[0], now()));

    assert!(message.contains("confidence"), "{message}");
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
fn a_persons_fact_names_the_person_and_a_step_without_confidence_stores_none() {
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
fn a_removal_stores_no_node() {
    let plan = connector_plan(vec![remove_entity("skill")]);

    assert!(fact_node(&plan, &plan.steps[0], now()).unwrap().is_none());
}

#[test]
fn a_step_of_no_known_category_is_a_defect() {
    let plan = connector_plan(vec![add_value("hobby", "chess", json!("yes"))]);

    let message = internal(fact_node(&plan, &plan.steps[0], now()));

    assert!(message.contains("hobby"), "{message}");
}

#[test]
fn a_fact_must_hold_exactly_one_property() {
    let two = connector_plan(vec![add_entity("skill", &[("a", "1"), ("b", "2")])]);
    let none = connector_plan(vec![Step {
        entity_kind: "skill".to_owned(),
        change: Change::AddEntity {
            key: NodeKey("k".to_owned()),
            properties: Vec::new(),
        },
        confidence: Some(0.9),
    }]);

    for plan in [two, none] {
        internal(fact_node(&plan, &plan.steps[0], now()));
    }
}

#[test]
fn a_property_change_is_refused_without_its_value_in_the_message() {
    let entity = NodeKey("construct:t:fact".to_owned());
    let secret = Property::new("skills", json!("a secret value"));
    for (change, kind) in [
        (
            Change::AddProperty {
                entity: entity.clone(),
                property: secret.clone(),
            },
            "add-property",
        ),
        (
            Change::ReplaceProperty {
                entity: entity.clone(),
                property: secret,
            },
            "replace-property",
        ),
        (
            Change::RemoveProperty {
                entity,
                name: "skills".to_owned(),
            },
            "remove-property",
        ),
    ] {
        let mut plan = connector_plan(vec![replace_property("skill", "skills", "a secret value")]);
        plan.steps[0].change = change;

        let message = internal(fact_node(&plan, &plan.steps[0], now()));

        assert!(message.contains(kind), "{message}");
        assert!(
            !message.contains("a secret value"),
            "no fact value: {message}"
        );
    }
}

/// A stored node of `type_id` holding `payload`.
pub(super) fn view(type_id: &str, payload: Option<Value>) -> NodeView {
    let subject = Subject {
        subject_id: Uuid::new_v4(),
        subject_type: None,
    };
    NodeView {
        node_key: "construct:t:fact".to_owned(),
        type_id: type_id.to_owned(),
        name: None,
        payload,
        has_embedding: false,
        labels: Vec::new(),
        adjacency: Vec::new(),
        adjacency_truncated: false,
        envelope: ElementEnvelope {
            tenant_id: Uuid::new_v4(),
            key: "construct:t:fact".to_owned(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            created_by: subject.clone(),
            updated_at: OffsetDateTime::UNIX_EPOCH,
            updated_by: subject,
            deleted_at: None,
            deleted_by: None,
            graph_revision: GraphRevision {
                source_epoch: 1,
                revision: 1,
            },
        },
    }
}

#[test]
fn a_stored_fact_reads_back_as_its_property_and_value() {
    let fact = fact_of(&view(
        SKILL_TYPE,
        Some(json!({ "skills": skills(), "origin": {}, "record": {} })),
    ))
    .expect("a fact");

    assert_eq!(fact.category, Category::Skill);
    assert_eq!(fact.property, "skills");
    assert_eq!(fact.value, skills());
    assert_eq!(fact.node_key.0, "construct:t:fact");
}

#[test]
fn a_node_that_holds_no_fact_is_no_fact() {
    assert!(fact_of(&view(SKILL_TYPE, None)).is_none(), "no payload");
    assert!(
        fact_of(&view(
            SKILL_TYPE,
            Some(json!({ "origin": {}, "record": {} }))
        ))
        .is_none(),
        "no fact property"
    );
    assert!(
        fact_of(&view(PROFILE_ROOT_TYPE, Some(json!({ "removals": [] })))).is_none(),
        "not a person type"
    );
}
