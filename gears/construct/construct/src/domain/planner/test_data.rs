use std::collections::VecDeque;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::person_types::PERSON_TYPES;
use serde_json::{Value, json};
use tokio::time::Instant;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::agent::{AgentMessage, Planner, PlannerCaps};
use super::catalog::PropertyCatalog;
use super::processing::{
    Managed, PlanWriter, Processing, ProcessingContexts, ProcessingPorts, ProfileSource, Written,
};
use crate::domain::admission::{Admission, AdmissionPorts, AdmissionRules};
use crate::domain::error::DomainError;
use crate::domain::model_client::{
    ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::plan::{NodeKey, Origin, Plan};
use crate::domain::profile::{Category, Fact, Profile};
use crate::domain::record_intake::{IntakeEvents, ReceivedRecord};
use crate::domain::sensitive_data::{SensitiveDataCheck, SensitiveDataChecks};
use crate::domain::subject_settings::SubjectSettings;
use crate::test_support::{
    FixedRules, FixedSubjectSettings, RecordingAdmissionEvents, RecordingEvents, open_rules,
    processing_context,
};

pub(super) const TENANT: Uuid = Uuid::from_u128(0x6b1f_0c2a_93d4_4e8f_a1b2_c3d4_e5f6_0718);
pub(super) const SUBJECT: Uuid = Uuid::from_u128(0x2c9e_7a41_5b3f_4d6e_8f90_1a2b_3c4d_5e6f);

pub(super) fn catalog() -> PropertyCatalog {
    PropertyCatalog::load().expect("the catalog")
}

/// The schema of `property` read straight from the JSON of its person type, following a reference into the type's
/// own definitions.
pub(super) fn schema_in_person_type(property: &str) -> Value {
    PERSON_TYPES
        .iter()
        .map(|person_type| {
            serde_json::from_str::<Value>(person_type.schema).expect("a JSON schema")
        })
        .find_map(|document| {
            let found = document
                .pointer(&format!(
                    "/allOf/1/properties/payload/properties/{property}"
                ))?
                .clone();
            Some(match found["$ref"].as_str() {
                Some(reference) => document
                    .pointer(reference.trim_start_matches('#'))
                    .expect("a definition")
                    .clone(),
                None => found,
            })
        })
        .expect("a property of a person type")
}

pub(super) fn origin() -> Origin {
    Origin::Connector {
        connector: "chat_engine".to_owned(),
        provenance: "chat_engine/thread-42/msg-7".to_owned(),
        version: "v1".to_owned(),
        type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

pub(super) fn profile() -> Profile {
    Profile {
        version: Some(7),
        facts: vec![
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:5d2c8a3e-0f41-4b7a-9c6d-2e8f1a3b4c5d"
                )),
                category: Category::Role,
                property: "role".to_owned(),
                value: json!("teacher"),
            },
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:a7e3f9b1-2c4d-4e6f-8a0b-1c3d5e7f9a2b"
                )),
                category: Category::Skill,
                property: "skills".to_owned(),
                value: json!({ "name": "Python" }),
            },
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:c4b6d8e0-1f3a-4c5e-9b7d-0e2f4a6c8e1b"
                )),
                category: Category::Identity,
                property: "full_name".to_owned(),
                value: json!("Jane Doe"),
            },
        ],
    }
}

pub(super) fn node_key(number: usize) -> NodeKey {
    profile().facts[number - 1].node_key.clone()
}

pub(super) fn is_new_key(key: &NodeKey) -> bool {
    key.0.starts_with(&format!("construct:{TENANT}:"))
        && profile().facts.iter().all(|fact| fact.node_key != *key)
}

pub(super) type Answer = Result<ModelResponse, ModelError>;

pub(super) fn caps() -> PlannerCaps {
    PlannerCaps {
        max_rounds: NonZeroU32::new(3).expect("rounds"),
        max_tokens: NonZeroU64::new(10_000).expect("tokens"),
    }
}

pub(super) struct ScriptedModel {
    answers: Mutex<VecDeque<Answer>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    pub(super) fn answering(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            requests: Mutex::default(),
        })
    }

    pub(super) fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(
        &self,
        _: &SecurityContext,
        request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        self.requests.lock().expect("lock").push(request.clone());
        let mut answers = self.answers.lock().expect("lock");
        if answers.len() > 1 {
            answers.pop_front().expect("an answer")
        } else {
            answers.front().cloned().expect("an answer")
        }
    }
}

pub(super) fn usage(input_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens: 20,
    }
}

pub(super) fn tool_calls(calls: &[(&str, Value)], input_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: ModelOutput::ToolCalls(
            calls
                .iter()
                .enumerate()
                .map(|(index, (name, arguments))| ToolCall {
                    id: format!("call_{index}"),
                    name: (*name).to_owned(),
                    arguments: arguments.clone(),
                })
                .collect(),
        ),
        usage: Some(usage(input_tokens)),
    }
}

pub(super) fn done(input_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: ModelOutput::Text("The plan is complete.".to_owned()),
        usage: Some(usage(input_tokens)),
    }
}

pub(super) struct Contexts(pub(super) bool);

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
pub(super) struct Profiles {
    pub(super) failing: bool,
    pub(super) hanging: bool,
    pub(super) reads: Mutex<usize>,
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
        if self.hanging {
            std::future::pending::<()>().await;
        }
        if self.failing {
            Err(DomainError::Unavailable("graph storage is down".to_owned()))
        } else {
            Ok(profile())
        }
    }
}

pub(super) struct Writer {
    pub(super) answers: Mutex<VecDeque<Result<Written, ()>>>,
    pub(super) plans: Mutex<Vec<Plan>>,
    pub(super) takes: Duration,
}

impl Writer {
    pub(super) fn answering(answers: &[Result<Written, ()>]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.iter().copied().collect()),
            plans: Mutex::default(),
            takes: Duration::ZERO,
        })
    }

    pub(super) fn plans(&self) -> Vec<Plan> {
        self.plans.lock().expect("lock").clone()
    }
}

#[async_trait]
impl PlanWriter for Writer {
    async fn write(&self, _: &SecurityContext, plan: &Plan) -> Result<Written, DomainError> {
        tokio::time::sleep(self.takes).await;
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

pub(super) struct Chain {
    pub(super) contexts: bool,
    pub(super) profiles: Arc<Profiles>,
    pub(super) model: Arc<ScriptedModel>,
    pub(super) checks: Vec<Arc<dyn SensitiveDataCheck>>,
    pub(super) settings: SubjectSettings,
    pub(super) rules: Result<AdmissionRules, fn() -> DomainError>,
    pub(super) writer: Arc<Writer>,
    pub(super) events: Arc<RecordingEvents>,
}

impl Chain {
    pub(super) fn new(answers: Vec<Answer>, written: &[Result<Written, ()>]) -> Self {
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

    pub(super) async fn process(&self, record: &ReceivedRecord) {
        self.processing().process(record).await;
    }

    pub(super) async fn manage(&self, deadline: Instant) -> Result<Managed, DomainError> {
        let (ctx, scope) = processing_context(TENANT);
        self.processing()
            .manage(&ctx, &scope, &agent_message(), deadline)
            .await
    }

    pub(super) fn processing(&self) -> Processing {
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
    }

    pub(super) fn reads(&self) -> usize {
        *self.profiles.reads.lock().expect("lock")
    }
}

pub(super) fn adding_rust(plans: usize) -> Vec<Answer> {
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

pub(super) const AGENT: Uuid = Uuid::from_u128(0xa6e7);

pub(super) fn agent_message() -> AgentMessage {
    AgentMessage {
        tenant_id: TENANT,
        subject_id: SUBJECT,
        agent: AGENT,
        message: "I also know Rust.".to_owned(),
        conversation: String::new(),
    }
}
