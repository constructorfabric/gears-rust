use serde_json::{Value, json};

use super::catalog::Catalog;
use super::draft::PlanDraft;
use super::test_data::{SUBJECT, TENANT, catalog, is_new_key, node_key, origin, profile};
use super::tools::{ADD, LINK, REMOVE, REPLACE, ToolRefusal};
use crate::domain::model_client::ToolCall;
use crate::domain::plan::{Change, NodeKey, Plan};

// The profile of `test_data::profile`: 1 me, 2 person Priya Shah, 3 project Atlas, 4 commitment owned by 3, and 5
// the link works_on from 2 to 3. The nodes this plan adds are numbered from 6.

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments,
    }
}

fn add(kind: &str, value: &Value) -> ToolCall {
    call(
        ADD,
        json!({ "kind": kind, "value": value, "confidence": 0.9 }),
    )
}

fn add_owned(kind: &str, value: &Value, owner: usize) -> ToolCall {
    call(
        ADD,
        json!({ "kind": kind, "value": value, "owner": owner, "confidence": 0.9 }),
    )
}

fn link(kind: &str, from: usize, to: usize) -> ToolCall {
    call(
        LINK,
        json!({ "kind": kind, "from": from, "to": to, "confidence": 0.9 }),
    )
}

fn replace(number: usize, value: &Value) -> ToolCall {
    call(
        REPLACE,
        json!({ "number": number, "value": value, "confidence": 0.9 }),
    )
}

fn remove(number: usize) -> ToolCall {
    call(REMOVE, json!({ "number": number, "confidence": 0.9 }))
}

fn refusal_after(catalog: &Catalog, before: &[ToolCall], refused: &ToolCall) -> ToolRefusal {
    let mut draft = PlanDraft::new(catalog, profile());
    for accepted in before {
        draft.apply(accepted).expect("accepted");
    }
    let steps_before = before.len();
    let refusal = draft.apply(refused).expect_err("refused");
    assert_eq!(
        draft.finish(TENANT, SUBJECT, origin()).steps.len(),
        steps_before,
        "a refused call added a step"
    );
    refusal
}

fn plan_of(calls: &[ToolCall]) -> Plan {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());
    for call in calls {
        draft.apply(call).expect("accepted");
    }
    draft.finish(TENANT, SUBJECT, origin())
}

#[test]
fn the_profile_is_shown_as_numbered_nodes_and_links() {
    let catalog = catalog();
    let draft = PlanDraft::new(&catalog, profile());

    assert_eq!(
        draft.numbered_profile(),
        [
            r#"1. me: {"name":"Jane Doe"}"#,
            r#"2. person: {"name":"Priya Shah","space":"work"}"#,
            r#"3. project: {"name":"Atlas"}"#,
            r#"4. commitment (owned by 3): {"kind":"action","text":"book the review"}"#,
            r#"5. link works_on from 2 to 3: {"role":"lead"}"#,
        ]
        .join("\n")
    );
}

#[test]
fn an_added_node_gets_a_new_key_and_the_number_after_the_profile() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    let first = draft
        .apply(&add("team", &json!({ "name": "Platform" })))
        .unwrap();
    let second = draft
        .apply(&add("team", &json!({ "name": "Design" })))
        .unwrap();

    assert_eq!(first, "added team as 6");
    assert_eq!(second, "added team as 7");
    let plan = draft.finish(TENANT, SUBJECT, origin());
    let keys: Vec<&NodeKey> = plan
        .steps
        .iter()
        .map(|step| match &step.change {
            Change::AddEntity { key, owner, .. } => {
                assert_eq!(owner, &None);
                key
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(keys.iter().all(|key| is_new_key(key)));
    assert_ne!(keys[0], keys[1]);
    assert_eq!(plan.profile_version, Some(7));
}

#[test]
fn a_node_with_the_name_of_a_held_one_is_refused_with_the_number_of_the_held_one() {
    let catalog = catalog();

    let same = refusal_after(
        &catalog,
        &[],
        &add("project", &json!({ "name": "  atlas " })),
    );

    assert_eq!(
        same,
        ToolRefusal::AlreadyExists {
            kind: "project".to_owned(),
            number: 3
        }
    );
    assert!(same.to_string().contains("as 3"), "{same}");
    let twice = refusal_after(
        &catalog,
        &[add("project", &json!({ "name": "Orion" }))],
        &add("project", &json!({ "name": "ORION" })),
    );
    assert_eq!(
        twice,
        ToolRefusal::AlreadyAdded {
            kind: "project".to_owned(),
            number: 6
        }
    );
}

#[test]
fn a_name_is_the_same_only_within_its_kind_and_its_owner() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    draft
        .apply(&add("team", &json!({ "name": "Atlas" })))
        .expect("a team may share a name with a project");
    draft
        .apply(&add_owned(
            "commitment",
            &json!({ "kind": "action", "text": "book the review" }),
            2,
        ))
        .expect("another owner, another commitment");
    let held = draft.apply(&add_owned(
        "commitment",
        &json!({ "kind": "action", "text": "Book the review" }),
        3,
    ));

    assert_eq!(
        held,
        Err(ToolRefusal::AlreadyExists {
            kind: "commitment".to_owned(),
            number: 4
        })
    );
}

#[test]
fn a_kind_the_schema_does_not_have_is_refused_with_the_kinds_it_has() {
    let catalog = catalog();

    let refusal = refusal_after(&catalog, &[], &add("pets", &json!({ "name": "Rex" })));

    let ToolRefusal::UnknownKind { kind, known } = refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(kind, "pets");
    assert!(
        known.contains("project") && known.contains("commitment"),
        "{known}"
    );
}

#[test]
fn a_value_that_does_not_fit_its_kind_is_refused_without_repeating_it() {
    let catalog = catalog();
    let value = json!({ "name": "Atlas 2", "status": "x".repeat(500) });

    let refusal = refusal_after(&catalog, &[], &add("project", &value));

    let ToolRefusal::ValueDoesNotFit { kind, reason } = &refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(kind, "project");
    assert!(
        reason.starts_with("`/status` breaks `maxLength`"),
        "{reason}"
    );
    assert!(!refusal.to_string().contains("xxxxx"), "{refusal}");
}

#[test]
fn an_entry_needs_an_owner_of_a_kind_that_may_own_it_and_a_thing_takes_none() {
    let catalog = catalog();
    let decision = json!({ "text": "ship in March" });

    assert!(matches!(
        refusal_after(&catalog, &[], &add("decision", &decision)),
        ToolRefusal::OwnerRequired { .. }
    ));
    assert!(matches!(
        refusal_after(&catalog, &[], &add_owned("decision", &decision, 4)),
        ToolRefusal::WrongOwner { number: 4, .. }
    ));
    assert!(matches!(
        refusal_after(&catalog, &[], &add_owned("decision", &decision, 5)),
        ToolRefusal::NotANode(5)
    ));
    assert!(matches!(
        refusal_after(&catalog, &[], &add_owned("decision", &decision, 99)),
        ToolRefusal::UnknownNumber(99)
    ));
    assert!(matches!(
        refusal_after(
            &catalog,
            &[],
            &add_owned("team", &json!({ "name": "Platform" }), 3)
        ),
        ToolRefusal::OwnerNotAllowed { .. }
    ));
}

#[test]
fn an_entry_can_be_owned_by_a_node_this_plan_adds() {
    let plan = plan_of(&[
        add("project", &json!({ "name": "Orion" })),
        add_owned("decision", &json!({ "text": "ship in March" }), 6),
    ]);

    let [project, decision] = &plan.steps[..] else {
        panic!("{:?}", plan.steps);
    };
    let (Change::AddEntity { key: project, .. }, Change::AddEntity { owner, .. }) =
        (&project.change, &decision.change)
    else {
        panic!("{plan:?}");
    };
    assert_eq!(owner.as_ref(), Some(project));
}

#[test]
fn the_profile_holds_one_me() {
    let catalog = catalog();

    let refusal = refusal_after(&catalog, &[], &add("me", &json!({ "name": "Someone" })));

    assert_eq!(
        refusal,
        ToolRefusal::SecondNode {
            kind: "me".to_owned(),
            number: 1
        }
    );
}

#[test]
fn a_link_joins_two_nodes_by_number_and_may_use_a_node_this_plan_adds() {
    let plan = plan_of(&[
        add("project", &json!({ "name": "Orion" })),
        link("works_on", 2, 6),
        call(
            LINK,
            json!({ "kind": "works_on", "from": 1, "to": 3, "fields": { "role": "advisor" }, "confidence": 0.8 }),
        ),
    ]);

    let [_, orion, advisor] = &plan.steps[..] else {
        panic!("{:?}", plan.steps);
    };
    assert_eq!(orion.entity_kind, "works_on");
    let Change::AddLink { kind, from, to, .. } = &orion.change else {
        panic!("{orion:?}");
    };
    assert_eq!(
        (kind.as_str(), from, to),
        (
            "works_on",
            &node_key(2),
            match &plan.steps[0].change {
                Change::AddEntity { key, .. } => key,
                other => panic!("{other:?}"),
            }
        )
    );
    let Change::AddLink { properties, .. } = &advisor.change else {
        panic!("{advisor:?}");
    };
    assert_eq!(properties[0].value, json!({ "role": "advisor" }));
    assert_eq!(advisor.confidence, Some(0.8));
}

#[test]
fn a_link_is_refused_for_an_unknown_kind_wrong_ends_a_held_link_or_bad_fields() {
    let catalog = catalog();

    assert!(matches!(
        refusal_after(&catalog, &[], &link("friend_of", 1, 2)),
        ToolRefusal::UnknownLinkKind { .. }
    ));
    // works_on goes from a person to a project, not the other way round.
    assert!(matches!(
        refusal_after(&catalog, &[], &link("works_on", 3, 2)),
        ToolRefusal::LinkEnd { number: 3, .. }
    ));
    assert_eq!(
        refusal_after(&catalog, &[], &link("works_on", 2, 3)),
        ToolRefusal::LinkExists {
            kind: "works_on".to_owned(),
            from: 2,
            to: 3
        }
    );
    assert!(matches!(
        refusal_after(&catalog, &[], &link("works_on", 2, 5)),
        ToolRefusal::NotANode(5)
    ));
    assert!(matches!(
        refusal_after(
            &catalog,
            &[],
            &call(
                LINK,
                json!({ "kind": "works_on", "from": 1, "to": 3, "fields": { "mood": "good" }, "confidence": 0.9 })
            )
        ),
        ToolRefusal::ValueDoesNotFit { .. }
    ));
    assert_eq!(
        refusal_after(&catalog, &[link("works_on", 1, 3)], &link("works_on", 1, 3)),
        ToolRefusal::LinkExists {
            kind: "works_on".to_owned(),
            from: 1,
            to: 3
        }
    );
}

#[test]
fn a_replace_changes_the_node_where_it_stands() {
    let plan = plan_of(&[replace(3, &json!({ "name": "Atlas", "status": "on hold" }))]);

    let Change::ReplaceEntity {
        old,
        key,
        owner,
        properties,
    } = &plan.steps[0].change
    else {
        panic!("{:?}", plan.steps);
    };
    assert_eq!(old, &node_key(3));
    assert_eq!(
        key, old,
        "the key stays, so the links and the owned nodes stay"
    );
    assert_eq!(owner, &None);
    assert_eq!(properties[0].name, "project");
    assert_eq!(
        properties[0].value,
        json!({ "name": "Atlas", "status": "on hold" })
    );
    assert_eq!(plan.steps[0].entity_kind, "project");
}

#[test]
fn a_replace_that_changes_nothing_or_takes_the_name_of_another_node_is_refused() {
    let catalog = catalog();

    assert_eq!(
        refusal_after(&catalog, &[], &replace(3, &json!({ "name": "Atlas" }))),
        ToolRefusal::AlreadyHeld { number: 3 }
    );
    let mut other = PlanDraft::new(&catalog, profile());
    other
        .apply(&add("project", &json!({ "name": "Orion" })))
        .unwrap();
    assert!(matches!(
        refusal_after(
            &catalog,
            &[],
            &replace(2, &json!({ "name": "Jane", "age": "old" }))
        ),
        ToolRefusal::ValueDoesNotFit { .. }
    ));
}

#[test]
fn a_number_names_one_replace_or_remove() {
    let catalog = catalog();

    assert_eq!(
        refusal_after(
            &catalog,
            &[replace(3, &json!({ "name": "Atlas", "status": "done" }))],
            &remove(3)
        ),
        ToolRefusal::NumberUsed(3)
    );
    assert_eq!(
        refusal_after(
            &catalog,
            &[remove(2)],
            &replace(2, &json!({ "name": "Priya" }))
        ),
        ToolRefusal::NumberUsed(2)
    );
    assert_eq!(
        refusal_after(&catalog, &[], &remove(42)),
        ToolRefusal::UnknownNumber(42)
    );
}

#[test]
fn a_remove_takes_the_owned_nodes_with_it_and_hides_what_goes_with_the_gone() {
    let catalog = catalog();
    let mut draft = PlanDraft::new(&catalog, profile());

    draft.apply(&remove(3)).expect("a project goes");

    assert_eq!(
        draft.numbered_profile(),
        [
            r#"1. me: {"name":"Jane Doe"}"#,
            r#"2. person: {"name":"Priya Shah","space":"work"}"#,
        ]
        .join("\n")
    );
    let plan = draft.finish(TENANT, SUBJECT, origin());
    // One step: the writer removes what the node owns, and graph storage its links.
    let removed: Vec<&NodeKey> = plan
        .steps
        .iter()
        .map(|step| match &step.change {
            Change::RemoveEntity { entity } => entity,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(removed, [&node_key(3)]);
    assert!(
        !plan
            .steps
            .iter()
            .any(|step| matches!(step.change, Change::RemoveLink { .. }))
    );
}

#[test]
fn a_link_is_removed_by_its_number_and_a_gone_node_cannot_be_linked() {
    let catalog = catalog();

    let plan = plan_of(&[remove(5)]);

    assert_eq!(
        plan.steps[0].change,
        Change::RemoveLink {
            kind: "works_on".to_owned(),
            from: node_key(2),
            to: node_key(3)
        }
    );
    assert_eq!(
        refusal_after(&catalog, &[remove(3)], &link("works_on", 1, 3)),
        ToolRefusal::NumberUsed(3)
    );
    assert_eq!(
        refusal_after(&catalog, &[remove(5)], &remove(5)),
        ToolRefusal::NumberUsed(5)
    );
}

#[test]
fn a_remove_stops_at_an_owned_node_this_plan_already_changed() {
    let catalog = catalog();

    let refusal = refusal_after(
        &catalog,
        &[replace(
            4,
            &json!({ "kind": "action", "text": "book the review early" }),
        )],
        &remove(3),
    );

    assert_eq!(refusal, ToolRefusal::NumberUsed(4));
}

#[test]
fn a_call_that_is_no_tool_or_has_bad_arguments_is_refused() {
    let catalog = catalog();

    assert!(matches!(
        refusal_after(&catalog, &[], &call("delete", json!({}))),
        ToolRefusal::UnknownTool(_)
    ));
    assert!(matches!(
        refusal_after(&catalog, &[], &call(ADD, json!({ "kind": "team" }))),
        ToolRefusal::BadArguments(_)
    ));
    assert_eq!(
        refusal_after(
            &catalog,
            &[],
            &call(
                ADD,
                json!({ "kind": "team", "value": { "name": "T" }, "confidence": 2 })
            )
        ),
        ToolRefusal::BadConfidence
    );
}

#[test]
fn the_tools_offer_the_kinds_and_the_link_kinds_of_the_schema() {
    let catalog = catalog();
    let draft = PlanDraft::new(&catalog, profile());

    let tools = draft.tools();

    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, [ADD, LINK, REPLACE, REMOVE]);
    let kinds = &tools[0].parameters["properties"]["kind"]["enum"];
    assert!(kinds.as_array().unwrap().contains(&json!("commitment")));
    let links = &tools[1].parameters["properties"]["kind"]["enum"];
    assert!(links.as_array().unwrap().contains(&json!("works_on")));
}

#[test]
fn a_replace_counts_as_the_one_me_and_a_rename_to_a_name_this_plan_adds_is_refused() {
    let catalog = catalog();

    assert_eq!(
        refusal_after(
            &catalog,
            &[replace(1, &json!({ "name": "Jane D." }))],
            &add("me", &json!({ "name": "Someone" }))
        ),
        ToolRefusal::SecondNode {
            kind: "me".to_owned(),
            number: 1
        }
    );
    assert_eq!(
        refusal_after(
            &catalog,
            &[add("project", &json!({ "name": "Orion" }))],
            &replace(3, &json!({ "name": "orion" }))
        ),
        ToolRefusal::AlreadyHeld { number: 6 }
    );
    assert_eq!(
        refusal_after(
            &catalog,
            &[replace(3, &json!({ "name": "Orion" }))],
            &add("project", &json!({ "name": "Orion" }))
        ),
        ToolRefusal::AlreadyAdded {
            kind: "project".to_owned(),
            number: 3
        }
    );
}

#[test]
fn a_node_with_a_new_node_under_it_is_not_removed_in_the_same_plan() {
    let catalog = catalog();

    let refusal = refusal_after(
        &catalog,
        &[add_owned(
            "decision",
            &json!({ "text": "ship in March" }),
            3,
        )],
        &remove(3),
    );

    assert_eq!(refusal, ToolRefusal::OwnsNewNodes(3));
}
