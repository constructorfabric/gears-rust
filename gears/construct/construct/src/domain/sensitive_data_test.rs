//! The sensitive-data checks runner, with test checks: no real check exists.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Barrier;

use crate::domain::error::DomainError;
use crate::domain::plan::Origin;
use crate::domain::plan::{PlanValue, PropertyRef, ValueRef};
use crate::domain::record_intake::{DropReason, DropStage, IntakeEvents};
use crate::domain::sensitive_data::{
    Action, Checked, Found, SensitiveDataCheck, SensitiveDataChecks, SensitiveDataKind, Verdict,
};
use crate::test_support::{
    FailingCheck, FixedCheck, RecordingEvents, SubstringCheck, add_entity, add_value,
    connector_plan, remove_entity, replace_property, spans,
};

fn checks(list: Vec<Arc<dyn SensitiveDataCheck>>) -> (SensitiveDataChecks, Arc<RecordingEvents>) {
    let drops = Arc::new(RecordingEvents::default());
    let runner = SensitiveDataChecks::new(list, Arc::clone(&drops) as Arc<dyn IntakeEvents>);
    (runner, drops)
}

fn at(step: usize, property: usize) -> ValueRef {
    piece(step, property, 0)
}

fn piece(step: usize, property: usize, piece: usize) -> ValueRef {
    ValueRef {
        step,
        property,
        piece,
    }
}

#[test]
fn every_kind_has_the_action_the_design_gives_it() {
    use SensitiveDataKind as K;
    let redacted = [K::PersonalIds, K::Finance, K::Contacts, K::Location];

    for kind in SensitiveDataKind::ALL {
        let expected = if redacted.contains(&kind) {
            Action::Redact
        } else {
            Action::Block
        };
        assert_eq!(kind.action(), expected, "{kind:?}");
    }
    assert_eq!(K::Credentials.action(), Action::Block);
    assert_eq!(K::Health.action(), Action::Block);
}

#[test]
fn the_values_are_those_the_plan_would_store_and_removals_have_none() {
    let plan = connector_plan(vec![
        add_entity("employer", &[("name", "Acme"), ("since", "2021")]),
        remove_entity("hobby"),
        replace_property("employer", "role", "engineer"),
    ]);

    let values = plan.values();

    assert_eq!(
        values,
        vec![
            PlanValue {
                at: at(0, 0),
                text: "Acme".to_owned()
            },
            PlanValue {
                at: at(0, 1),
                text: "2021".to_owned()
            },
            PlanValue {
                at: at(2, 0),
                text: "engineer".to_owned()
            },
        ]
    );
}

#[tokio::test]
async fn without_checks_every_value_is_allowed() {
    let (runner, drops) = checks(Vec::new());
    let plan = connector_plan(vec![add_entity("diagnosis", &[("name", "asthma")])]);

    assert_eq!(runner.run(&plan).await, Checked::Verdicts(Vec::new()));
    assert!(drops.taken().is_empty());
}

#[tokio::test]
async fn each_check_answers_for_the_values_it_found_content_in() {
    let (runner, _) = checks(vec![
        Arc::new(SubstringCheck {
            kind: SensitiveDataKind::Health,
            needle: "asthma",
        }),
        Arc::new(SubstringCheck {
            kind: SensitiveDataKind::Contacts,
            needle: "+49 30 1234",
        }),
    ]);
    let plan = connector_plan(vec![add_entity(
        "note",
        &[("text", "has asthma"), ("phone", "call +49 30 1234 now")],
    )]);

    let Checked::Verdicts(verdicts) = runner.run(&plan).await else {
        panic!("the checks answered");
    };

    assert_eq!(
        verdicts,
        vec![
            Verdict {
                kind: SensitiveDataKind::Health,
                found: vec![Found {
                    at: at(0, 0),
                    spans: spans(&[(4, 10)])
                }],
            },
            Verdict {
                kind: SensitiveDataKind::Contacts,
                found: vec![Found {
                    at: at(0, 1),
                    spans: spans(&[(5, 16)])
                }],
            },
        ]
    );
}

/// Waits until every check of the test has started, so the checks finish only
/// when they run at the same time.
struct WaitsForTheOthers(Arc<Barrier>, SensitiveDataKind);

#[async_trait]
impl SensitiveDataCheck for WaitsForTheOthers {
    fn kind(&self) -> SensitiveDataKind {
        self.1
    }

    async fn check(&self, _values: &[PlanValue]) -> Result<Verdict, DomainError> {
        self.0.wait().await;
        Ok(Verdict::allow(self.1))
    }
}

#[tokio::test]
async fn the_checks_run_in_parallel() {
    let barrier = Arc::new(Barrier::new(3));
    let (runner, _) = checks(vec![
        Arc::new(WaitsForTheOthers(
            Arc::clone(&barrier),
            SensitiveDataKind::Health,
        )),
        Arc::new(WaitsForTheOthers(
            Arc::clone(&barrier),
            SensitiveDataKind::Sex,
        )),
        Arc::new(WaitsForTheOthers(barrier, SensitiveDataKind::Politics)),
    ]);
    let plan = connector_plan(vec![add_entity("note", &[("text", "hello")])]);

    let checked = tokio::time::timeout(Duration::from_secs(5), runner.run(&plan))
        .await
        .expect("checks run one after another would wait forever");

    assert!(matches!(checked, Checked::Verdicts(v) if v.len() == 3));
}

#[tokio::test]
async fn a_failed_check_drops_the_record_with_a_drop_event() {
    let (runner, drops) = checks(vec![
        Arc::new(SubstringCheck {
            kind: SensitiveDataKind::Health,
            needle: "asthma",
        }),
        Arc::new(FailingCheck(SensitiveDataKind::Politics)),
    ]);
    let plan = connector_plan(vec![add_entity("note", &[("text", "has asthma")])]);

    assert_eq!(runner.run(&plan).await, Checked::Dropped);

    let events = drops.taken();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].stage, DropStage::Checks);
    assert_eq!(events[0].reason, DropReason::CheckFailed);
    assert_eq!(events[0].tenant_id, plan.tenant_id);
    assert_eq!(events[0].subject_id, Some(plan.subject_id));
    assert_eq!(events[0].connector, "connector-a");
    assert_eq!(
        events[0].record_key,
        crate::domain::record_intake::RecordKey::of(crate::domain::record_intake::IdentityParts {
            connector: "connector-a",
            provenance: "chat_engine/thread-42/msg-7",
            version: "v1",
        })
    );
}

#[tokio::test]
async fn a_failed_check_on_a_plan_without_a_record_drops_it_without_a_record_event() {
    let (runner, drops) = checks(vec![Arc::new(FailingCheck(SensitiveDataKind::Health))]);
    let mut plan = connector_plan(vec![add_entity("note", &[("text", "hello")])]);
    plan.origin = Origin::Administrator {
        administrator: uuid::Uuid::new_v4(),
    };

    assert_eq!(runner.run(&plan).await, Checked::Dropped);
    assert!(drops.taken().is_empty());
}

#[tokio::test]
async fn an_answer_for_another_kind_drops_the_record() {
    let (runner, drops) = checks(vec![Arc::new(FixedCheck(
        SensitiveDataKind::Health,
        Verdict::allow(SensitiveDataKind::Finance),
    ))]);
    let plan = connector_plan(vec![add_entity("note", &[("text", "hello")])]);

    assert_eq!(runner.run(&plan).await, Checked::Dropped);
    assert_eq!(drops.taken()[0].reason, DropReason::CheckFailed);
}

#[tokio::test]
async fn a_finding_in_a_value_the_plan_does_not_hold_drops_the_record() {
    let (runner, drops) = checks(vec![Arc::new(FixedCheck(
        SensitiveDataKind::Health,
        Verdict {
            kind: SensitiveDataKind::Health,
            found: vec![Found {
                at: at(0, 5),
                spans: Vec::new(),
            }],
        },
    ))]);
    let plan = connector_plan(vec![add_entity("note", &[("text", "hello")])]);

    assert_eq!(runner.run(&plan).await, Checked::Dropped);
    assert_eq!(drops.taken().len(), 1);
}

#[test]
fn an_object_value_gives_its_strings_and_numbers_but_not_its_keys() {
    let plan = connector_plan(vec![add_value(
        "skill",
        "skills",
        // Keys in alphabetical order, so the walk order is the same whether or
        // not serde_json keeps insertion order.
        serde_json::json!({ "current": true, "name": "Python", "tags": ["rust", "c"], "years": 5 }),
    )]);

    let values = plan.values();

    // The pieces in walk order, each with its text: the boolean has none, the
    // array's items come in their order, the number comes as its text.
    let pieces: Vec<(usize, &str)> = values
        .iter()
        .map(|v| (v.at.piece, v.text.as_str()))
        .collect();
    assert_eq!(pieces, [(0, "Python"), (1, "rust"), (2, "c"), (3, "5")]);
    assert!(values.iter().all(|v| v.at.property_ref()
        == PropertyRef {
            step: 0,
            property: 0
        }));
}

#[tokio::test]
async fn a_finding_in_a_piece_its_value_does_not_have_drops_the_record() {
    let (runner, drops) = checks(vec![Arc::new(FixedCheck(
        SensitiveDataKind::Health,
        Verdict {
            kind: SensitiveDataKind::Health,
            found: vec![Found {
                at: ValueRef {
                    step: 0,
                    property: 0,
                    piece: 1,
                },
                spans: Vec::new(),
            }],
        },
    ))]);
    // One string, so the value has piece 0 only.
    let plan = connector_plan(vec![add_entity("note", &[("text", "hello")])]);

    assert_eq!(runner.run(&plan).await, Checked::Dropped);
    let events = drops.taken();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason, DropReason::CheckFailed);
}
