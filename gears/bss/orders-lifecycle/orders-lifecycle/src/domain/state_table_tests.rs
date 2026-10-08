#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use serde_json::Value;

/// The reviewed S1-02 catalog: an independent expectation of the rows and expanded keys.
const CATALOG: &str = include_str!("../../../orders-lifecycle-sdk/contracts/catalog.json");

fn catalog() -> Value {
    serde_json::from_str(CATALOG).unwrap()
}
fn state(token: &str) -> OrderState {
    serde_json::from_value(Value::String(token.to_owned())).unwrap()
}
fn trigger(token: &str) -> Trigger {
    serde_json::from_value(Value::String(token.to_owned())).unwrap()
}

#[test]
fn registered_table_compiles_with_29_rows_and_46_expanded_keys() {
    let table = StateTable::registered().unwrap();
    assert_eq!(table.rows().len(), 29);
    assert_eq!(table.expanded().count(), 46);
    assert_eq!(table.create_row().id, RowId::of(1));
}

#[test]
fn every_row_matches_the_catalog_rows_versioning_and_event() {
    let table = StateTable::registered().unwrap();
    let catalog = catalog();
    let rows = catalog["source_transition_rows"].as_array().unwrap();
    assert_eq!(rows.len(), 29);
    for (row, expected) in table.rows().iter().zip(rows) {
        assert_eq!(
            u64::from(row.id.number()),
            expected["row"].as_u64().unwrap()
        );
        assert_eq!(row.trigger, trigger(expected["trigger"].as_str().unwrap()));
        let from: Vec<Option<OrderState>> = expected["from_states"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().map(state))
            .collect();
        assert_eq!(row.from, from, "{}", row.id);
        let versioning = match expected["versioning"].as_str().unwrap() {
            "append" => Versioning::Append,
            "none" => Versioning::StateOnly,
            other => panic!("unexpected versioning {other}"),
        };
        assert_eq!(row.versioning, versioning, "{}", row.id);
        let event = expected["event"]
            .as_str()
            .map(|e| serde_json::from_value::<EventKind>(Value::String(e.to_owned())).unwrap());
        assert_eq!(row.event, event, "{}", row.id);
    }
}

#[test]
fn expanded_keys_equal_the_catalog_keys_and_targets() {
    let table = StateTable::registered().unwrap();
    let catalog = catalog();
    let keys = catalog["expanded_transition_keys"].as_array().unwrap();
    assert_eq!(keys.len(), table.expanded().count());
    for key in keys {
        let from = key["from_state"].as_str().map(state);
        let t = trigger(key["trigger"].as_str().unwrap());
        let row = match from {
            None => table.create_row(),
            Some(from) => table.lookup(from, t).expect("catalog key is admissible"),
        };
        assert_eq!(u64::from(row.id.number()), key["row"].as_u64().unwrap());
        let to = key["to"].as_str().unwrap();
        match (to, from) {
            ("$pre_hold_state", Some(from)) => {
                assert_eq!(row.target, Target::PreHold);
                assert_eq!(row.effective_target(from, None), None);
                assert_eq!(
                    row.effective_target(from, Some(OrderState::Approved)),
                    Some(OrderState::Approved)
                );
            }
            (to, Some(from)) => assert_eq!(row.effective_target(from, None), Some(state(to))),
            (to, None) => assert_eq!(row.target, Target::State(state(to))),
        }
    }
}

#[test]
fn every_unlisted_state_trigger_pair_is_not_admissible_and_terminals_have_no_exit() {
    let table = StateTable::registered().unwrap();
    let catalog = catalog();
    let listed: std::collections::HashSet<(String, String)> = catalog["expanded_transition_keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|k| {
            Some((
                k["from_state"].as_str()?.to_owned(),
                k["trigger"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut forbidden = 0;
    for s in OrderState::ALL {
        for t in Trigger::ALL {
            let key = (
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                serde_json::to_value(t)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            let admissible = table.lookup(*s, *t).is_some();
            assert_eq!(admissible, listed.contains(&key), "{key:?}");
            if is_terminal(*s) {
                assert!(!admissible, "terminal exit {key:?}");
            }
            if !admissible {
                forbidden += 1;
            }
        }
    }
    // 11 states x 21 triggers = 231 pairs; 45 existing-order keys are admissible.
    assert_eq!(forbidden, 231 - 45);
    // Create is never an existing-order trigger.
    assert!(
        OrderState::ALL
            .iter()
            .all(|s| table.lookup(*s, Trigger::Create).is_none())
    );
    // Normative exclusions (§4.3, D-109).
    for (s, t) in [
        (OrderState::InFulfillment, Trigger::Expire),
        (OrderState::InFulfillment, Trigger::Amendment),
        (OrderState::OnHold, Trigger::AcknowledgeCompleted),
        (OrderState::OnHold, Trigger::Amendment),
    ] {
        assert!(table.lookup(s, t).is_none(), "{s:?} {t:?}");
    }
}

#[test]
fn eventful_rows_are_exactly_the_declared_event_source_rows() {
    let table = StateTable::registered().unwrap();
    let eventful: Vec<u8> = table
        .rows()
        .iter()
        .filter(|r| r.event.is_some())
        .map(|r| r.id.number())
        .collect();
    let mut expected: Vec<u8> = vec![4, 5, 6, 8, 9, 10];
    expected.extend(13..=29);
    expected.retain(|r| *r != 0);
    assert_eq!(eventful, expected);
    let eventless: Vec<u8> = table
        .rows()
        .iter()
        .filter(|r| r.event.is_none())
        .map(|r| r.id.number())
        .collect();
    assert_eq!(eventless, vec![1, 2, 3, 7, 11, 12]);
    let versioning: Vec<u8> = table
        .rows()
        .iter()
        .filter(|r| r.is_versioning())
        .map(|r| r.id.number())
        .collect();
    assert_eq!(versioning, vec![1, 4, 18, 19, 20]);
    let actors: Vec<(u8, ActorRule)> = table
        .rows()
        .iter()
        .filter(|r| r.actor != ActorRule::Authorized)
        .map(|r| (r.id.number(), r.actor))
        .collect();
    assert_eq!(
        actors,
        vec![
            (6, ActorRule::System),
            (24, ActorRule::System),
            (28, ActorRule::User),
            (29, ActorRule::User)
        ]
    );
}

#[test]
fn workflow_class_is_exactly_the_five_seam_operations_triggers() {
    let class: Vec<Trigger> = Trigger::ALL
        .iter()
        .copied()
        .filter(|t| is_workflow_class(*t))
        .collect();
    let catalog = catalog();
    let mut expected: Vec<Trigger> = catalog["workflow_triggers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| trigger(t.as_str().unwrap()))
        .collect();
    expected.sort_by_key(|t| Trigger::ALL.iter().position(|x| x == t));
    assert_eq!(class, expected);
    assert!(!is_workflow_class(Trigger::ForceFailUnreconciled));
}

fn mutated(f: impl FnOnce(&mut Vec<RowDecl>)) -> Result<StateTable, TableError> {
    let mut rows = ROWS.to_vec();
    f(&mut rows);
    StateTable::compile(&rows, &EVENT_ROWS)
}

#[test]
fn invalid_declarations_fail_startup() {
    // Duplicate expanded key: row 17 also declared from draft (row 5's key).
    let err = mutated(|r| r[16].from = &["submitted", "draft"]).unwrap_err();
    assert!(matches!(err, TableError::DuplicateKey { .. }), "{err}");
    // Unknown token.
    let err = mutated(|r| r[4].to = "canceled").unwrap_err();
    assert_eq!(err, TableError::UnknownToken(RowId::of(5), "canceled"));
    let err = mutated(|r| r[4].trigger = "abort").unwrap_err();
    assert_eq!(err, TableError::UnknownToken(RowId::of(5), "abort"));
    // Terminal exit.
    let err = mutated(|r| r[4].from = &["completed"]).unwrap_err();
    assert_eq!(err, TableError::TerminalExit(RowId::of(5)));
    // Normative exclusions.
    let err = mutated(|r| r[23].from = &["in_fulfillment"]).unwrap_err();
    assert_eq!(
        err,
        TableError::ForbiddenEdge(RowId::of(24), OrderState::InFulfillment)
    );
    // Event drift between the row and the event's declared rows.
    let err = mutated(|r| r[6].event = Some("OrderApproved")).unwrap_err();
    assert_eq!(err, TableError::EventMismatch(EventKind::OrderApproved));
    let err = mutated(|r| r[3].event = None).unwrap_err();
    assert_eq!(err, TableError::EventMismatch(EventKind::OrderSubmitted));
    // Pre-hold target only on resume.
    let err = mutated(|r| r[22].to = PRE_HOLD).unwrap_err();
    assert_eq!(err, TableError::PreHoldShape(RowId::of(23)));
    // Versioning only on create/submit/amendment.
    let err = mutated(|r| r[20].versioning = Versioning::Append).unwrap_err();
    assert_eq!(err, TableError::VersioningShape(RowId::of(21)));
    // Create only from nothing.
    let err = mutated(|r| r[0].from = &["draft"]).unwrap_err();
    assert_eq!(err, TableError::CreateShape(RowId::of(1)));
    // Row numbering and completeness.
    let err = mutated(|r| {
        r.pop();
    })
    .unwrap_err();
    assert!(matches!(err, TableError::RowNumbering(_)), "{err}");
    let err = mutated(|r| r.swap(1, 2)).unwrap_err();
    assert!(matches!(err, TableError::RowNumbering(_)), "{err}");
    // An unused trigger: replace record-acceptance by hold from another state.
    let err = mutated(|r| {
        r[24].trigger = "hold";
        r[24].from = &["draft"];
        r[24].event = Some("OrderAcceptanceRecorded");
    })
    .unwrap_err();
    assert_eq!(err, TableError::UnusedTrigger(Trigger::RecordAcceptance));
    // Unknown event in the event-row declarations.
    let mut events = EVENT_ROWS.to_vec();
    events[0].0 = "OrderSubmittedV2";
    assert!(matches!(
        StateTable::compile(&ROWS, &events).unwrap_err(),
        TableError::UnknownToken(_, "OrderSubmittedV2")
    ));
}
