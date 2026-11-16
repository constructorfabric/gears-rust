use std::sync::Arc;
use std::time::Duration;

use construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE;
use serde_json::json;
use tokio::time::Instant;

use super::processing::{Changes, ManageDeadlines, Managed, NothingStored, Written};
use super::test_data::{
    AGENT, Chain, Profiles, SUBJECT, TENANT, Writer, adding_rust, done, node_key, tool_calls,
};
use crate::domain::error::DomainError;
use crate::domain::model_client::ModelError;
use crate::domain::plan::{Change, Origin};
use crate::domain::record_intake::{DropEvent, DropReason, DropStage, ReceivedRecord};
use crate::domain::sensitive_data::SensitiveDataKind;
use crate::test_support::{FailingCheck, example_records, received_chat_record};

fn record() -> ReceivedRecord {
    received_chat_record(TENANT, SUBJECT)
}

fn candidate() -> ReceivedRecord {
    let mut candidate = record();
    candidate.envelope.type_id = ENRICHMENT_FACT_CANDIDATE_TYPE.to_owned();
    candidate.record = example_records().swap_remove(1);
    candidate.record["subject_id"] = json!(SUBJECT);
    candidate
}

fn dropped(stage: DropStage, reason: DropReason) -> Vec<DropEvent> {
    vec![DropEvent::for_record(&record(), stage, reason)]
}

#[tokio::test]
async fn an_admitted_plan_is_written_once() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    chain.process(&record()).await;

    let plans = chain.writer.plans();
    let [plan] = plans.as_slice() else {
        panic!("one write: {plans:?}");
    };
    assert_eq!(
        (plan.tenant_id, plan.subject_id, plan.steps.len()),
        (TENANT, SUBJECT, 1)
    );
    assert!(chain.events.taken().is_empty());
}

#[tokio::test]
async fn a_write_conflict_plans_again_on_a_new_read_of_the_profile() {
    let chain = Chain::new(
        adding_rust(2),
        &[Ok(Written::Conflict), Ok(Written::Stored)],
    );

    chain.process(&record()).await;

    assert_eq!((chain.reads(), chain.writer.plans().len()), (2, 2));
    assert_eq!(chain.model.requests().len(), 4, "two rounds per plan");
    assert!(chain.events.taken().is_empty());
}

#[tokio::test]
async fn a_conflict_on_every_rerun_drops_the_record_at_the_writer() {
    let chain = Chain::new(adding_rust(3), &[Ok(Written::Conflict)]);

    chain.process(&record()).await;

    assert_eq!(
        chain.writer.plans().len(),
        3,
        "the first write and two reruns"
    );
    assert_eq!(
        chain.events.taken(),
        dropped(DropStage::Writer, DropReason::RerunLimit)
    );
}

#[tokio::test]
async fn each_failure_drops_the_record_at_its_stage() {
    let no_context = Chain {
        contexts: false,
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };
    let no_profile = Chain {
        profiles: Arc::new(Profiles {
            failing: true,
            ..Profiles::default()
        }),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };
    let no_rules = Chain {
        rules: Err(|| DomainError::Unavailable("settings service is down".to_owned())),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };
    let no_write = Chain::new(adding_rust(1), &[Err(())]);

    for (chain, stage) in [
        (no_context, DropStage::Planner),
        (no_profile, DropStage::Planner),
        (no_rules, DropStage::Admission),
        (no_write, DropStage::Writer),
    ] {
        chain.process(&record()).await;

        assert_eq!(
            chain.events.taken(),
            dropped(stage, DropReason::ProcessingFailed)
        );
    }
}

#[tokio::test]
async fn a_record_the_planner_does_not_decide_on_is_dropped_before_the_profile_is_read() {
    let mut no_subject = record();
    no_subject.envelope.subject_id = None;
    let mut mastery = record();
    mastery.envelope.type_id = construct_sdk::gts::MASTERY_STUDENT_MASTERY_TYPE.to_owned();
    let mut catalog = record();
    catalog.envelope.type_id = construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE.to_owned();
    catalog.envelope.subject_id = None;
    let mut assistant = record();
    assistant.record["payload"]["role"] = json!("assistant");

    for received in [no_subject, mastery, catalog, assistant] {
        let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

        chain.process(&received).await;

        assert_eq!(chain.reads(), 0, "{}", received.envelope.type_id);
        assert!(chain.model.requests().is_empty());
        assert_eq!(
            chain.events.taken(),
            vec![DropEvent::for_record(
                &received,
                DropStage::Planner,
                DropReason::NotPlanned
            )]
        );
    }
}

#[tokio::test]
async fn a_plan_without_steps_writes_nothing() {
    let chain = Chain::new(vec![Ok(done(900))], &[Ok(Written::Stored)]);

    chain.process(&record()).await;

    assert!(chain.writer.plans().is_empty());
    assert!(chain.events.taken().is_empty());
}

#[tokio::test]
async fn a_plan_a_stage_drops_is_not_written_and_dropped_once() {
    let failing_check = Chain {
        checks: vec![Arc::new(FailingCheck(SensitiveDataKind::Health))],
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };
    let mut personalization_off = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    personalization_off.settings.personalization_enabled = false;
    let model_down = Chain::new(vec![Err(ModelError::Timeout)], &[Ok(Written::Stored)]);

    for (chain, stage, reason) in [
        (failing_check, DropStage::Checks, DropReason::CheckFailed),
        (
            personalization_off,
            DropStage::Admission,
            DropReason::PersonalizationOff,
        ),
        (model_down, DropStage::Planner, DropReason::ModelFailed),
    ] {
        chain.process(&record()).await;

        assert!(chain.writer.plans().is_empty());
        let events = chain.events.taken();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!((events[0].stage, events[0].reason), (stage, reason));
    }
}

#[tokio::test]
async fn a_fact_candidate_never_removes_or_replaces_a_value() {
    let remove = ("remove", json!({ "number": 2, "confidence": 0.8 }));
    let replace = (
        "replace",
        json!({ "number": 1, "value": "professor", "confidence": 0.8 }),
    );
    let add = (
        "add",
        json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.8 }),
    );
    let only_taking_away = Chain::new(
        vec![
            Ok(tool_calls(&[remove.clone(), replace.clone()], 900)),
            Ok(done(950)),
        ],
        &[Ok(Written::Stored)],
    );
    let also_adding = Chain::new(
        vec![Ok(tool_calls(&[remove, replace, add], 900)), Ok(done(950))],
        &[Ok(Written::Stored)],
    );

    only_taking_away.process(&candidate()).await;
    also_adding.process(&candidate()).await;

    assert!(only_taking_away.writer.plans().is_empty());
    let plans = also_adding.writer.plans();
    let [plan] = plans.as_slice() else {
        panic!("one write: {plans:?}");
    };
    assert!(
        matches!(
            plan.steps.as_slice(),
            [step] if matches!(step.change, Change::AddEntity { .. } | Change::AddProperty { .. })
        ),
        "only the add is written: {plan:?}"
    );
}

/// The skill, fact 2, under an open review request.
fn skill_under_review(chain: &Chain) {
    let mut skill = crate::test_support::stored_fact(
        crate::domain::profile::Category::Skill,
        "skills",
        json!({ "name": "Python" }),
        1,
    );
    skill.node_key = node_key(2);
    chain.reviews.open_on(
        crate::domain::subject_settings::SubjectKey {
            tenant_id: TENANT,
            subject_id: SUBJECT,
        },
        &skill,
    );
}

#[tokio::test]
async fn a_step_on_a_fact_under_review_is_dropped_and_the_rest_written() {
    let remove_skill = ("remove", json!({ "number": 2, "confidence": 0.9 }));
    let replace_role = (
        "replace",
        json!({ "number": 1, "value": "professor", "confidence": 0.9 }),
    );
    let chain = Chain::new(
        vec![
            Ok(tool_calls(&[remove_skill, replace_role], 900)),
            Ok(done(950)),
        ],
        &[Ok(Written::Stored)],
    );
    skill_under_review(&chain);

    chain.process(&record()).await;

    let plans = chain.writer.plans();
    let [plan] = plans.as_slice() else {
        panic!("one write: {plans:?}");
    };
    assert!(
        matches!(
            plan.steps.as_slice(),
            [step] if matches!(&step.change, Change::ReplaceEntity { old, .. } if *old == node_key(1))
        ),
        "only the role's replace is written: {plan:?}"
    );
    assert!(chain.events.taken().is_empty(), "the record is not dropped");
}

#[tokio::test]
async fn a_call_whose_only_step_is_on_a_fact_under_review_stores_nothing() {
    let chain = Chain::new(
        vec![
            Ok(tool_calls(
                &[("remove", json!({ "number": 2, "confidence": 0.9 }))],
                900,
            )),
            Ok(done(950)),
        ],
        &[Ok(Written::Stored)],
    );
    skill_under_review(&chain);

    let managed = chain.manage(in_a_while()).await.expect("an answer");

    assert_eq!(managed, Managed::NothingStored(NothingStored::UnderReview));
    assert_eq!(NothingStored::UnderReview.name(), "under_review");
    assert!(chain.writer.plans().is_empty());
}

#[tokio::test]
async fn an_unreadable_review_store_drops_the_record_at_the_planner() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    chain
        .reviews
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);

    chain.process(&record()).await;

    assert!(chain.writer.plans().is_empty());
    assert_eq!(
        chain.events.taken(),
        dropped(DropStage::Planner, DropReason::ProcessingFailed)
    );
}

fn in_a_while() -> ManageDeadlines {
    let now = Instant::now();
    ManageDeadlines {
        before_write: now + Duration::from_secs(20),
        answer: now + Duration::from_secs(27),
    }
}

/// Deadlines whose steps before the write end in 50 ms.
fn soon_before_the_write() -> ManageDeadlines {
    ManageDeadlines {
        before_write: Instant::now() + Duration::from_millis(50),
        ..in_a_while()
    }
}

#[tokio::test]
async fn a_manage_call_reads_the_facts_under_review_as_construct_not_as_the_agent() {
    let chain = Chain::new(
        vec![
            Ok(tool_calls(
                &[("remove", json!({ "number": 2, "confidence": 0.9 }))],
                900,
            )),
            Ok(done(950)),
        ],
        &[Ok(Written::Stored)],
    );
    skill_under_review(&chain);

    let managed = chain
        .manage_as(toolkit_security::AccessScope::deny_all(), in_a_while())
        .await
        .expect("an answer");

    assert_eq!(managed, Managed::NothingStored(NothingStored::UnderReview));
    assert_eq!(
        *chain.reviews.under_review_scopes.lock().unwrap(),
        [crate::test_support::processing_context(TENANT).1],
        "an agent scope that reaches no request does not unfreeze its fact"
    );
}

#[tokio::test]
async fn a_manage_call_drops_only_the_steps_on_a_fact_under_review() {
    let remove_skill = ("remove", json!({ "number": 2, "confidence": 0.9 }));
    let replace_role = (
        "replace",
        json!({ "number": 1, "value": "professor", "confidence": 0.9 }),
    );
    let chain = Chain::new(
        vec![
            Ok(tool_calls(&[remove_skill, replace_role], 900)),
            Ok(done(950)),
        ],
        &[Ok(Written::Stored)],
    );
    skill_under_review(&chain);

    let managed = chain.manage(in_a_while()).await.expect("an answer");

    assert_eq!(
        managed,
        Managed::Stored(Changes {
            added: 0,
            replaced: 1,
            removed: 0
        })
    );
}

#[tokio::test]
async fn a_manage_call_with_the_review_store_down_writes_nothing() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    chain
        .reviews
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let managed = chain.manage(in_a_while()).await;

    assert!(
        matches!(managed, Err(DomainError::Unavailable(_))),
        "{managed:?}"
    );
    assert!(chain.writer.plans().is_empty());
}

#[tokio::test]
async fn what_a_subject_told_an_agent_is_written_and_counted() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);

    let managed = chain.manage(in_a_while()).await.expect("an answer");

    assert_eq!(
        managed,
        Managed::Stored(Changes {
            added: 1,
            replaced: 0,
            removed: 0
        })
    );
    let plans = chain.writer.plans();
    let [plan] = plans.as_slice() else {
        panic!("one write: {plans:?}");
    };
    assert_eq!(plan.origin, Origin::Agent { agent: AGENT });
}

#[tokio::test]
async fn a_conflict_plans_the_message_again_on_a_new_read() {
    let chain = Chain::new(
        adding_rust(2),
        &[Ok(Written::Conflict), Ok(Written::Stored)],
    );

    let managed = chain.manage(in_a_while()).await.expect("an answer");

    assert!(matches!(managed, Managed::Stored(_)), "{managed:?}");
    assert_eq!((chain.reads(), chain.writer.plans().len()), (2, 2));
}

#[tokio::test]
async fn a_call_that_stores_nothing_answers_why_and_sends_no_record_drop_event() {
    let failing_check = Chain {
        checks: vec![Arc::new(FailingCheck(SensitiveDataKind::Health))],
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };
    let mut personalization_off = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    personalization_off.settings.personalization_enabled = false;
    let cases = [
        (
            Chain::new(vec![Ok(done(900))], &[Ok(Written::Stored)]),
            NothingStored::NoChange,
        ),
        (
            Chain::new(vec![Err(ModelError::Timeout)], &[Ok(Written::Stored)]),
            NothingStored::Dropped(DropReason::ModelFailed),
        ),
        (
            failing_check,
            NothingStored::Dropped(DropReason::CheckFailed),
        ),
        (
            personalization_off,
            NothingStored::Dropped(DropReason::PersonalizationOff),
        ),
        (
            Chain::new(adding_rust(3), &[Ok(Written::Conflict)]),
            NothingStored::Dropped(DropReason::RerunLimit),
        ),
    ];

    for (chain, nothing) in cases {
        let managed = chain.manage(in_a_while()).await.expect("an answer");

        assert_eq!(managed, Managed::NothingStored(nothing));
        assert!(chain.events.taken().is_empty(), "{nothing:?}");
    }
}

#[tokio::test]
async fn the_deadline_cuts_off_the_steps_before_the_write() {
    let chain = Chain {
        profiles: Arc::new(Profiles {
            hanging: true,
            ..Profiles::default()
        }),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };

    let managed = chain
        .manage(soon_before_the_write())
        .await
        .expect("an answer");

    assert_eq!(managed, Managed::NothingStored(NothingStored::Timeout));
    assert!(chain.writer.plans().is_empty());
}

#[tokio::test]
async fn a_write_that_started_is_awaited_past_the_deadline_before_the_write() {
    let chain = Chain {
        writer: Arc::new(Writer {
            takes: Duration::from_millis(200),
            ..Arc::into_inner(Writer::answering(&[Ok(Written::Stored)])).expect("one owner")
        }),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };

    let managed = chain
        .manage(soon_before_the_write())
        .await
        .expect("an answer");

    assert!(matches!(managed, Managed::Stored(_)), "{managed:?}");
}

#[tokio::test]
async fn the_write_runs_by_the_answer_deadline() {
    let chain = Chain::new(adding_rust(1), &[Ok(Written::Stored)]);
    let deadlines = in_a_while();

    chain.manage(deadlines).await.expect("an answer");

    assert_eq!(chain.writer.deadlines(), vec![deadlines.answer]);
}

#[tokio::test]
async fn an_unreachable_profile_is_an_error() {
    let chain = Chain {
        profiles: Arc::new(Profiles {
            failing: true,
            ..Profiles::default()
        }),
        ..Chain::new(adding_rust(1), &[Ok(Written::Stored)])
    };

    assert!(chain.manage(in_a_while()).await.is_err());
}
