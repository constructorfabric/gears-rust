//! Admission over fixed subject settings and tenant rules, with verdicts made
//! by hand or by test checks.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use crate::domain::admission::{
    Admission, AdmissionEvents, AdmissionOutcome, AdmissionPorts, AdmittedPlan, Allowed, Enforced,
};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, Origin, Plan, Property, Step, ValueRef};
use crate::domain::record_intake::{DropReason, DropStage, IntakeEvents};
use crate::domain::sensitive_data::{
    Checked, Found, SensitiveDataCheck, SensitiveDataChecks, SensitiveDataKind, Verdict,
};
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    AdmissionFixture, FixedRules, FixedSubjectSettings, RecordingAdmissionEvents, RecordingEvents,
    SubstringCheck, add_entity, by_a_person, connector_plan, open_rules, processing_context,
    remove_entity, replace_property, spans,
};

const SECRET: &str = "a value that must not reach an event";

async fn admit(fixture: &AdmissionFixture, plan: Plan, verdicts: &[Verdict]) -> AdmissionOutcome {
    let (ctx, scope) = processing_context(plan.tenant_id);
    fixture
        .build()
        .admit(&ctx, &scope, plan, verdicts)
        .await
        .expect("admission decides")
}

fn admitted(outcome: AdmissionOutcome) -> AdmittedPlan {
    match outcome {
        AdmissionOutcome::Admitted(admitted) => *admitted,
        AdmissionOutcome::Dropped(cause) => panic!("dropped: {cause:?}"),
    }
}

fn at(step: usize, property: usize) -> ValueRef {
    ValueRef { step, property }
}

fn found(kind: SensitiveDataKind, at: ValueRef, spans: Vec<std::ops::Range<usize>>) -> Verdict {
    Verdict {
        kind,
        found: vec![Found { at, spans }],
    }
}

fn reviewer() -> Origin {
    Origin::Reviewer {
        reviewer: Uuid::new_v4(),
    }
}

fn administrator() -> Origin {
    Origin::Administrator {
        administrator: Uuid::new_v4(),
    }
}

fn only(members: &[&str]) -> Allowed<String> {
    Allowed::Only(
        members
            .iter()
            .map(|m| (*m).to_owned())
            .collect::<HashSet<_>>(),
    )
}

/// Assert a record's plan was dropped for `reason` with one drop event at the
/// admission stage, and nothing else.
fn assert_dropped(fixture: &AdmissionFixture, outcome: &AdmissionOutcome, reason: DropReason) {
    assert_eq!(outcome, &AdmissionOutcome::Dropped(reason));
    let drops = fixture.drops.taken();
    assert_eq!(drops.len(), 1, "one drop event");
    assert_eq!(drops[0].stage, DropStage::Admission);
    assert_eq!(drops[0].reason, reason);
    assert!(fixture.events.taken().is_empty(), "no block or redaction");
}

/// Assert a plan without a record was dropped for `reason`: the result says
/// so, and no record drop event is written; its caller audits the reason.
fn assert_dropped_without_a_record_event(
    fixture: &AdmissionFixture,
    outcome: &AdmissionOutcome,
    reason: DropReason,
) {
    assert_eq!(outcome, &AdmissionOutcome::Dropped(reason));
    assert!(fixture.drops.taken().is_empty(), "no record drop event");
    assert!(fixture.events.taken().is_empty(), "no block or redaction");
}

// --- The plan as a whole ---

#[tokio::test]
async fn a_plan_that_passes_every_rule_is_admitted_unchanged() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![
        add_entity("employer", &[("name", "Acme")]),
        remove_entity("hobby"),
    ]);

    let admitted = admitted(admit(&fixture, plan.clone(), &[]).await);

    assert_eq!(admitted.plan, plan);
    assert!(admitted.blocked.is_empty() && admitted.redacted.is_empty());
    assert!(admitted.dropped_steps.is_empty());
    assert!(fixture.drops.taken().is_empty());
}

#[tokio::test]
async fn personalization_off_drops_the_plan() {
    let fixture = AdmissionFixture {
        settings: SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: false,
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")])]);

    let outcome = admit(&fixture, plan.clone(), &[]).await;

    assert_dropped(&fixture, &outcome, DropReason::PersonalizationOff);
    let event = &fixture.drops.taken()[0];
    assert_eq!(event.tenant_id, plan.tenant_id);
    assert_eq!(event.subject_id, Some(plan.subject_id));
    assert_eq!(event.connector, "connector-a");
    assert_eq!(event.provenance, "chat_engine/thread-42/msg-7");
    assert_eq!(event.version, "v1");
    assert_eq!(event.type_id, construct_sdk::gts::CHAT_MESSAGE_TYPE);
}

#[tokio::test]
async fn an_erasure_under_way_drops_the_plan_even_with_personalization_on() {
    let fixture = AdmissionFixture {
        settings: SubjectSettings {
            personalization_enabled: true,
            erasure_in_progress: true,
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")])]);

    let outcome = admit(&fixture, plan, &[]).await;

    assert_dropped(&fixture, &outcome, DropReason::ErasureInProgress);
}

#[tokio::test]
async fn a_connector_the_tenant_does_not_trust_drops_the_plan() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            trusted_connectors: only(&["connector-b"]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")])]);

    let outcome = admit(&fixture, plan, &[]).await;

    assert_dropped(&fixture, &outcome, DropReason::UntrustedSource);
}

#[tokio::test]
async fn a_trusted_connector_passes() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            trusted_connectors: only(&["connector-a"]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")])]);

    admitted(admit(&fixture, plan, &[]).await);
}

#[tokio::test]
async fn a_step_below_the_confidence_floor_drops_the_whole_plan() {
    let fixture = AdmissionFixture::default();
    let mut low = add_entity("hobby", &[("name", "chess")]);
    low.confidence = Some(0.5);
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")]), low]);

    let outcome = admit(&fixture, plan, &[]).await;

    assert_dropped(&fixture, &outcome, DropReason::BelowConfidenceFloor);
}

#[tokio::test]
async fn a_step_at_the_floor_passes() {
    let fixture = AdmissionFixture::default();
    let mut step = add_entity("hobby", &[("name", "chess")]);
    step.confidence = Some(0.6);

    admitted(admit(&fixture, connector_plan(vec![step]), &[]).await);
}

#[tokio::test]
async fn a_model_step_without_a_confidence_or_with_one_that_is_not_a_number_drops_the_plan() {
    for confidence in [None, Some(f32::NAN)] {
        let fixture = AdmissionFixture::default();
        let mut step = add_entity("hobby", &[("name", "chess")]);
        step.confidence = confidence;

        let outcome = admit(&fixture, connector_plan(vec![step]), &[]).await;

        assert_dropped(&fixture, &outcome, DropReason::BelowConfidenceFloor);
    }
}

#[tokio::test]
async fn an_agents_plan_meets_the_floor_but_has_no_connector_to_trust() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            trusted_connectors: only(&[]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let agent = Origin::Agent {
        agent: Uuid::new_v4(),
    };
    let mut plan = connector_plan(vec![add_entity("hobby", &[("name", "chess")])]);
    plan.origin = agent.clone();

    admitted(admit(&fixture, plan.clone(), &[]).await);

    plan.steps[0].confidence = Some(0.1);
    let outcome = admit(&fixture, plan, &[]).await;
    assert_dropped_without_a_record_event(&fixture, &outcome, DropReason::BelowConfidenceFloor);
}

#[tokio::test]
async fn a_plan_written_by_a_person_skips_trust_and_the_confidence_floor() {
    for origin in [reviewer(), administrator()] {
        let fixture = AdmissionFixture {
            rules: crate::domain::admission::AdmissionRules {
                trusted_connectors: only(&[]),
                confidence_floor: 1.0,
                ..open_rules()
            },
            ..AdmissionFixture::default()
        };
        let plan = by_a_person(
            connector_plan(vec![replace_property("employer", "name", "Acme")]),
            origin,
        );

        admitted(admit(&fixture, plan, &[]).await);
    }
}

#[tokio::test]
async fn a_plan_written_by_a_person_goes_through_every_other_rule() {
    let off = SubjectSettings {
        personalization_enabled: false,
        erasure_in_progress: false,
    };
    let fixture = AdmissionFixture {
        settings: off,
        ..AdmissionFixture::default()
    };
    let plan = by_a_person(
        connector_plan(vec![add_entity("employer", &[("name", "Acme")])]),
        administrator(),
    );
    let outcome = admit(&fixture, plan.clone(), &[]).await;
    assert_dropped_without_a_record_event(&fixture, &outcome, DropReason::PersonalizationOff);

    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            entity_kinds: only(&["hobby"]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let outcome = admit(&fixture, plan.clone(), &[]).await;
    assert_dropped_without_a_record_event(&fixture, &outcome, DropReason::EntityKindNotAllowed);

    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            max_steps: 0,
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let outcome = admit(&fixture, plan, &[]).await;
    assert_dropped_without_a_record_event(&fixture, &outcome, DropReason::WriteCapExceeded);
}

#[tokio::test]
async fn an_entity_kind_the_tenant_does_not_allow_drops_the_plan() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            entity_kinds: only(&["employer"]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![
        add_entity("employer", &[("name", "Acme")]),
        add_entity("religion", &[("name", "x")]),
    ]);

    let outcome = admit(&fixture, plan, &[]).await;

    assert_dropped(&fixture, &outcome, DropReason::EntityKindNotAllowed);
}

#[tokio::test]
async fn removing_an_entity_of_a_kind_no_longer_allowed_passes() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            entity_kinds: only(&["employer"]),
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![remove_entity("religion")]);

    admitted(admit(&fixture, plan, &[]).await);
}

#[tokio::test]
async fn a_plan_over_the_write_caps_is_dropped() {
    let too_many = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            max_steps: 1,
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![
        add_entity("employer", &[("name", "Acme")]),
        remove_entity("hobby"),
    ]);
    let outcome = admit(&too_many, plan, &[]).await;
    assert_dropped(&too_many, &outcome, DropReason::WriteCapExceeded);

    let too_long = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            max_value_chars: 4,
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme Inc")])]);
    let outcome = admit(&too_long, plan, &[]).await;
    assert_dropped(&too_long, &outcome, DropReason::WriteCapExceeded);
}

#[tokio::test]
async fn the_value_cap_counts_characters_not_bytes() {
    let fixture = AdmissionFixture {
        rules: crate::domain::admission::AdmissionRules {
            max_value_chars: 4,
            ..open_rules()
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("city", &[("name", "K\u{f6}ln")])]);

    admitted(admit(&fixture, plan, &[]).await);
}

#[tokio::test]
async fn a_dropped_plan_writes_no_block_event_even_with_block_verdicts() {
    let fixture = AdmissionFixture {
        settings: SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: false,
        },
        ..AdmissionFixture::default()
    };
    let plan = connector_plan(vec![add_entity("note", &[("text", SECRET)])]);

    let outcome = admit(
        &fixture,
        plan,
        &[found(SensitiveDataKind::Health, at(0, 0), Vec::new())],
    )
    .await;

    assert_dropped(&fixture, &outcome, DropReason::PersonalizationOff);
}

#[tokio::test]
async fn unreadable_settings_or_rules_decide_nothing() {
    let plan = connector_plan(vec![add_entity("employer", &[("name", "Acme")])]);
    let (ctx, scope) = processing_context(plan.tenant_id);
    let unavailable = || DomainError::Unavailable("settings service down".to_owned());

    for (settings, rules) in [
        (
            FixedSubjectSettings(Err(unavailable)),
            FixedRules(Ok(open_rules())),
        ),
        (
            FixedSubjectSettings(Ok(AdmissionFixture::default().settings)),
            FixedRules(Err(unavailable)),
        ),
    ] {
        let events = Arc::new(RecordingAdmissionEvents::default());
        let drops = Arc::new(RecordingEvents::default());
        let admission = Admission::new(AdmissionPorts {
            settings: Arc::new(settings),
            rules: Arc::new(rules),
            events: Arc::clone(&events) as Arc<dyn AdmissionEvents>,
            drops: Arc::clone(&drops) as Arc<dyn IntakeEvents>,
        });

        let result = admission.admit(&ctx, &scope, plan.clone(), &[]).await;

        assert!(
            matches!(result, Err(DomainError::Unavailable(_))),
            "got {result:?}"
        );
        assert!(events.taken().is_empty() && drops.taken().is_empty());
    }
}

// --- The verdicts ---

#[tokio::test]
async fn a_blocked_value_is_not_stored_the_rest_is_and_the_result_says_so() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![add_entity(
        "person",
        &[("diagnosis", SECRET), ("employer", "Acme")],
    )]);

    let admitted = admitted(
        admit(
            &fixture,
            plan.clone(),
            &[found(SensitiveDataKind::Health, at(0, 0), Vec::new())],
        )
        .await,
    );

    assert_eq!(
        admitted.plan.steps[0].change.properties(),
        &[Property::new("employer", "Acme")]
    );
    assert_eq!(
        admitted.blocked,
        vec![Enforced {
            at: at(0, 0),
            kind: SensitiveDataKind::Health
        }]
    );
    assert!(admitted.dropped_steps.is_empty());
    let events = fixture.events.taken();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "blocked");
    assert_eq!(events[0].1.kind, SensitiveDataKind::Health);
    assert_eq!(events[0].1.subject_id, plan.subject_id);
    assert!(!format!("{events:?}").contains(SECRET), "no content");
}

#[tokio::test]
async fn a_step_that_keeps_no_value_is_dropped() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![
        add_entity("diagnosis", &[("name", "asthma")]),
        Step {
            entity_kind: "employer".to_owned(),
            change: Change::AddProperty {
                entity: crate::domain::plan::NodeKey("e".to_owned()),
                property: Property::new("password", "hunter2"),
            },
            confidence: Some(0.9),
        },
        add_entity("employer", &[("name", "Acme")]),
    ]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[
                found(SensitiveDataKind::Health, at(0, 0), Vec::new()),
                found(SensitiveDataKind::Credentials, at(1, 0), Vec::new()),
            ],
        )
        .await,
    );

    assert_eq!(admitted.dropped_steps, vec![0, 1]);
    assert_eq!(admitted.plan.steps.len(), 1);
    assert_eq!(admitted.plan.steps[0].entity_kind, "employer");
}

#[tokio::test]
async fn a_replace_whose_new_value_is_blocked_is_dropped_whole_so_the_old_value_stays() {
    let fixture = AdmissionFixture::default();
    let replace_entity = Step {
        entity_kind: "employer".to_owned(),
        change: Change::ReplaceEntity {
            old: crate::domain::plan::NodeKey("old".to_owned()),
            key: crate::domain::plan::NodeKey("new".to_owned()),
            properties: vec![
                Property::new("name", "Acme"),
                Property::new("salary", "90k"),
            ],
        },
        confidence: Some(0.9),
    };
    let plan = connector_plan(vec![
        replace_property("person", "diagnosis", "asthma"),
        replace_entity,
    ]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[
                found(SensitiveDataKind::Health, at(0, 0), Vec::new()),
                found(SensitiveDataKind::OtherProtected, at(1, 1), Vec::new()),
            ],
        )
        .await,
    );

    assert!(
        admitted.plan.steps.is_empty(),
        "no step removes an old value"
    );
    assert_eq!(admitted.dropped_steps, vec![0, 1]);
    assert_eq!(admitted.blocked.len(), 2);
}

#[tokio::test]
async fn every_value_blocked_leaves_an_admitted_plan_with_nothing_to_store() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![add_entity("diagnosis", &[("name", "asthma")])]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[found(SensitiveDataKind::Health, at(0, 0), Vec::new())],
        )
        .await,
    );

    assert!(admitted.plan.steps.is_empty());
}

#[tokio::test]
async fn a_redacted_value_keeps_its_step_without_the_found_content() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![replace_property(
        "person",
        "contact",
        "call +49 30 1234 now",
    )]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[found(
                SensitiveDataKind::Contacts,
                at(0, 0),
                spans(&[(5, 16)]),
            )],
        )
        .await,
    );

    assert_eq!(
        admitted.plan.steps[0].change.properties(),
        &[Property::new("contact", "call  now")]
    );
    assert_eq!(
        admitted.redacted,
        vec![Enforced {
            at: at(0, 0),
            kind: SensitiveDataKind::Contacts
        }]
    );
    assert!(admitted.blocked.is_empty() && admitted.dropped_steps.is_empty());
    let events = fixture.events.taken();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "redacted");
    assert_eq!(events[0].1.kind, SensitiveDataKind::Contacts);
}

#[tokio::test]
async fn overlapping_redactions_of_two_kinds_both_apply() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![add_entity("note", &[("text", "abcdefghij")])]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[
                found(SensitiveDataKind::Contacts, at(0, 0), spans(&[(1, 4)])),
                found(
                    SensitiveDataKind::Location,
                    at(0, 0),
                    spans(&[(3, 6), (8, 9)]),
                ),
            ],
        )
        .await,
    );

    assert_eq!(admitted.plan.steps[0].change.properties()[0].value, "aghj");
    assert_eq!(admitted.redacted.len(), 2);
}

#[tokio::test]
async fn block_wins_over_redact_on_the_same_value() {
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![add_entity(
        "note",
        &[("text", "x"), ("other", "kept")],
    )]);

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[
                found(SensitiveDataKind::Contacts, at(0, 0), spans(&[(0, 1)])),
                found(SensitiveDataKind::Health, at(0, 0), Vec::new()),
            ],
        )
        .await,
    );

    assert_eq!(
        admitted.blocked,
        vec![Enforced {
            at: at(0, 0),
            kind: SensitiveDataKind::Health
        }]
    );
    assert!(admitted.redacted.is_empty());
    assert_eq!(admitted.plan.steps[0].change.properties().len(), 1);
}

#[tokio::test]
async fn a_value_that_cannot_be_redacted_as_asked_is_blocked() {
    let cases: [(&str, Vec<std::ops::Range<usize>>); 5] = [
        ("whole", spans(&[(0, 5)])),       // nothing left
        ("  pad  ", spans(&[(2, 5)])),     // only spaces left
        ("short", spans(&[(2, 40)])),      // out of the value
        ("K\u{f6}ln x", spans(&[(0, 2)])), // inside a character
        ("no spans", Vec::new()),          // nothing to remove
    ];
    for (value, spans) in cases {
        let fixture = AdmissionFixture::default();
        let plan = connector_plan(vec![add_entity(
            "note",
            &[("text", value), ("other", "kept")],
        )]);

        let admitted = admitted(
            admit(
                &fixture,
                plan,
                &[found(SensitiveDataKind::Location, at(0, 0), spans)],
            )
            .await,
        );

        assert_eq!(
            admitted.blocked,
            vec![Enforced {
                at: at(0, 0),
                kind: SensitiveDataKind::Location
            }],
            "{value}"
        );
        assert!(admitted.redacted.is_empty());
        assert_eq!(fixture.events.taken()[0].0, "blocked");
    }
}

#[tokio::test]
async fn a_plan_written_by_a_person_goes_through_the_verdicts() {
    let fixture = AdmissionFixture::default();
    let plan = by_a_person(
        connector_plan(vec![replace_property("person", "diagnosis", "asthma")]),
        reviewer(),
    );

    let admitted = admitted(
        admit(
            &fixture,
            plan,
            &[found(SensitiveDataKind::Health, at(0, 0), Vec::new())],
        )
        .await,
    );

    assert!(admitted.plan.steps.is_empty());
    assert_eq!(fixture.events.taken()[0].1.origin.kind(), "reviewer");
}

#[tokio::test]
async fn checks_then_admission_store_nothing_the_checks_block() {
    let checks = SensitiveDataChecks::new(
        vec![
            Arc::new(SubstringCheck {
                kind: SensitiveDataKind::Credentials,
                needle: "hunter2",
            }) as Arc<dyn SensitiveDataCheck>,
            Arc::new(SubstringCheck {
                kind: SensitiveDataKind::Contacts,
                needle: "+49 30 1234",
            }),
        ],
        Arc::new(RecordingEvents::default()),
    );
    let fixture = AdmissionFixture::default();
    let plan = connector_plan(vec![add_entity(
        "account",
        &[
            ("password", "my password is hunter2"),
            ("phone", "call +49 30 1234"),
            ("name", "Ada"),
        ],
    )]);

    let Checked::Verdicts(verdicts) = checks.run(&plan).await else {
        panic!("the checks answered");
    };
    let admitted = admitted(admit(&fixture, plan, &verdicts).await);

    let stored = admitted.plan.values();
    assert!(stored.iter().all(|v| !v.text.contains("hunter2")));
    assert!(stored.iter().all(|v| !v.text.contains("+49 30 1234")));
    assert_eq!(
        admitted.plan.steps[0].change.properties(),
        &[
            Property::new("phone", "call "),
            Property::new("name", "Ada")
        ]
    );
}
