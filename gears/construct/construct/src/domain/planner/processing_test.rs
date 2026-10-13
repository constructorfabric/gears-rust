use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::agent::Planner;
use super::processing::{
    PlanWriter, Processing, ProcessingContexts, ProcessingPorts, ProfileSource, Written,
};
use super::test_data::{
    Answer, SUBJECT, ScriptedModel, TENANT, caps, catalog, done, profile, tool_calls,
};
use crate::domain::admission::{Admission, AdmissionPorts, AdmissionRules};
use crate::domain::error::DomainError;
use crate::domain::model_client::ModelError;
use crate::domain::plan::Plan;
use crate::domain::profile::Profile;
use crate::domain::record_intake::{
    DropEvent, DropReason, DropStage, IntakeEvents, ReceivedRecord,
};
use crate::domain::sensitive_data::{SensitiveDataCheck, SensitiveDataChecks, SensitiveDataKind};
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    FailingCheck, FixedRules, FixedSubjectSettings, RecordingAdmissionEvents, RecordingEvents,
    open_rules, processing_context, received_chat_record,
};

struct Contexts(bool);

#[async_trait]
impl ProcessingContexts for Contexts {
    async fn for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        if self.0 {
            Ok(processing_context(tenant_id))
        } else {
            Err(DomainError::Unavailable("no token".to_owned()))
        }
    }
}

#[derive(Default)]
struct Profiles {
    failing: bool,
    reads: Mutex<usize>,
}

#[async_trait]
impl ProfileSource for Profiles {
    async fn read(
        &self,
        _: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Profile, DomainError> {
        assert_eq!((tenant_id, subject_id), (TENANT, SUBJECT));
        *self.reads.lock().expect("lock") += 1;
        if self.failing {
            Err(DomainError::Unavailable("graph storage is down".to_owned()))
        } else {
            Ok(profile())
        }
    }
}

struct Writer {
    answers: Mutex<VecDeque<Result<Written, ()>>>,
    plans: Mutex<Vec<Plan>>,
}

impl Writer {
    fn answering(answers: &[Result<Written, ()>]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().copied().collect()),
            plans: Mutex::default(),
        })
    }

    fn plans(&self) -> Vec<Plan> {
        self.plans.lock().expect("lock").clone()
    }
}

#[async_trait]
impl PlanWriter for Writer {
    async fn write(&self, _: &SecurityContext, plan: &Plan) -> Result<Written, DomainError> {
        self.plans.lock().expect("lock").push(plan.clone());
        let mut answers = self.answers.lock().expect("lock");
        let answer = if answers.len() > 1 {
            answers.pop_front()
        } else {
            answers.front().copied()
        };
        answer
            .expect("an answer")
            .map_err(|()| DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

struct Chain {
    contexts: bool,
    profiles: Arc<Profiles>,
    model: Arc<ScriptedModel>,
    checks: Vec<Arc<dyn SensitiveDataCheck>>,
    settings: SubjectSettings,
    rules: Result<AdmissionRules, fn() -> DomainError>,
    writer: Arc<Writer>,
    events: Arc<RecordingEvents>,
}

impl Chain {
    fn new(answers: Vec<Answer>, written: &[Result<Written, ()>]) -> Self {
        let events = Arc::new(RecordingEvents::default());
        Self {
            contexts: true,
            profiles: Arc::default(),
            model: ScriptedModel::answering(answers),
            checks: Vec::new(),
            settings: SubjectSettings {
                personalization_enabled: true,
                erasure_in_progress: false,
            },
            rules: Ok(open_rules()),
            writer: Writer::answering(written),
            events,
        }
    }

    async fn process(&self, record: &ReceivedRecord) {
        let events = Arc::clone(&self.events) as Arc<dyn IntakeEvents>;
        Processing::new(
            Planner::new(
                self.model.clone(),
                Arc::new(catalog()),
                Arc::clone(&events),
                caps(),
            ),
            SensitiveDataChecks::new(self.checks.clone(), Arc::clone(&events)),
            Admission::new(AdmissionPorts {
                settings: Arc::new(FixedSubjectSettings(Ok(self.settings))),
                rules: Arc::new(FixedRules(self.rules.clone())),
                events: Arc::new(RecordingAdmissionEvents::default()),
                drops: Arc::clone(&events),
            }),
            ProcessingPorts {
                contexts: Arc::new(Contexts(self.contexts)),
                profiles: Arc::clone(&self.profiles) as Arc<dyn ProfileSource>,
                writer: Arc::clone(&self.writer) as Arc<dyn PlanWriter>,
                events,
            },
            2,
        )
        .process(record)
        .await;
    }

    fn reads(&self) -> usize {
        *self.profiles.reads.lock().expect("lock")
    }
}

fn adding_rust(plans: usize) -> Vec<Answer> {
    (0..plans)
        .flat_map(|_| {
            [
                Ok(tool_calls(
                    &[(
                        "add",
                        json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.8 }),
                    )],
                    900,
                )),
                Ok(done(950)),
            ]
        })
        .collect()
}

fn record() -> ReceivedRecord {
    received_chat_record(TENANT, SUBJECT)
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

    for received in [no_subject, mastery, catalog] {
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
