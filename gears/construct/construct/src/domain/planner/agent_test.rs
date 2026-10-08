use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use toolkit_security::SecurityContext;

use super::agent::{Planner, PlannerCaps};
use super::test_data::{SUBJECT, TENANT, catalog, profile};
use crate::domain::model_client::{
    Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::plan::Step;
use crate::domain::record_intake::{DropCause, DropEvent, Envelope, IntakeEvents, ReceivedRecord};
use crate::test_support::{chat_record, context_in};

const CAPS: PlannerCaps = PlannerCaps {
    max_rounds: 3,
    max_tokens: 10_000,
};

struct ScriptedModel {
    answers: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn answering(answers: Vec<Result<ModelResponse, ModelError>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            requests: Mutex::default(),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(
        &self,
        _: &SecurityContext,
        request: ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        self.requests.lock().expect("lock").push(request);
        let mut answers = self.answers.lock().expect("lock");
        if answers.len() > 1 {
            answers.pop_front().expect("an answer")
        } else {
            answers.front().cloned().expect("an answer")
        }
    }
}

#[derive(Default)]
struct RecordedEvents(Mutex<Vec<DropEvent>>);

impl IntakeEvents for RecordedEvents {
    fn dropped(&self, event: DropEvent) {
        self.0.lock().expect("lock").push(event);
    }
}

fn tool_calls(calls: &[(&str, serde_json::Value)], tokens: u64) -> ModelResponse {
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
        usage: Usage {
            input_tokens: tokens,
            output_tokens: 20,
        },
    }
}

fn done() -> ModelResponse {
    ModelResponse {
        output: ModelOutput::Text("The plan is complete.".to_owned()),
        usage: Usage {
            input_tokens: 900,
            output_tokens: 10,
        },
    }
}

fn record() -> ReceivedRecord {
    ReceivedRecord {
        tenant_id: TENANT,
        connector: "chat_engine".to_owned(),
        envelope: Envelope {
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            provenance: "chat_engine/thread-42/msg-7".to_owned(),
            version: "v1".to_owned(),
            subject_id: Some(SUBJECT),
        },
        record: chat_record(SUBJECT, "chat_engine/thread-42/msg-7", "v1"),
    }
}

fn planner(model: Arc<ScriptedModel>, events: Arc<RecordedEvents>) -> Planner {
    Planner::new(model, Arc::new(catalog()), events, CAPS)
}

#[tokio::test]
async fn the_tool_calls_of_each_round_become_the_plan() {
    let model = ScriptedModel::answering(vec![
        Ok(tool_calls(
            &[(
                "add",
                json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.8 }),
            )],
            900,
        )),
        Ok(tool_calls(
            &[(
                "replace",
                json!({ "number": 1, "value": "school principal", "confidence": 0.9 }),
            )],
            950,
        )),
        Ok(done()),
    ]);
    let events = Arc::new(RecordedEvents::default());

    let plan = planner(model.clone(), events.clone())
        .plan(&context_in(TENANT), &record(), profile())
        .await
        .expect("a plan");

    assert!(
        matches!(
            plan.steps.as_slice(),
            [Step::Add { .. }, Step::Replace { .. }]
        ),
        "{:?}",
        plan.steps
    );
    assert_eq!(model.requests().len(), 3);
    assert!(events.0.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_refused_tool_call_goes_back_to_the_model_as_the_tool_result() {
    let model = ScriptedModel::answering(vec![
        Ok(tool_calls(
            &[(
                "add",
                json!({ "property": "role", "value": "dean", "confidence": 0.5 }),
            )],
            900,
        )),
        Ok(done()),
    ]);

    let plan = planner(model.clone(), Arc::new(RecordedEvents::default()))
        .plan(&context_in(TENANT), &record(), profile())
        .await
        .expect("a plan");

    assert!(plan.steps.is_empty());
    let second = &model.requests()[1];
    assert!(
        second.messages.iter().any(|message| matches!(
            message,
            Message::ToolResult { call_id, content }
                if call_id == "call_0" && content.contains("use replace 1")
        )),
        "{:?}",
        second.messages
    );
}

#[tokio::test]
async fn the_model_gets_the_record_and_the_numbered_profile_but_no_id() {
    let model = ScriptedModel::answering(vec![Ok(done())]);

    planner(model.clone(), Arc::new(RecordedEvents::default()))
        .plan(&context_in(TENANT), &record(), profile())
        .await
        .expect("a plan");

    let first = model.requests()[0].clone();
    let text = first
        .messages
        .iter()
        .map(|message| format!("{message:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("Which of these two papers contradict each other?"),
        "{text}"
    );
    assert!(text.contains("1. role (roles): teacher"), "{text}");
    for id in profile()
        .facts
        .iter()
        .map(|fact| fact.node_key.clone())
        .chain([TENANT.to_string(), SUBJECT.to_string()])
    {
        assert!(!text.contains(&id), "{text} shows {id}");
    }
    assert!(!text.contains("gts."), "{text} shows a type id");
}

#[tokio::test]
async fn each_failure_drops_the_record_with_its_cause_and_no_plan() {
    let add = (
        "add",
        json!({ "property": "skills", "value": { "name": "Go" }, "confidence": 0.5 }),
    );
    let mut without_subject = record();
    without_subject.envelope.subject_id = None;
    let cases = [
        (
            vec![Ok(tool_calls(std::slice::from_ref(&add), 900))],
            record(),
            DropCause::RoundCap,
        ),
        (
            vec![Ok(tool_calls(std::slice::from_ref(&add), 20_000))],
            record(),
            DropCause::TokenCap,
        ),
        (
            vec![Err(ModelError::Timeout)],
            record(),
            DropCause::ModelFailed,
        ),
        (
            vec![Ok(done())],
            without_subject,
            DropCause::ProcessingFailed,
        ),
    ];

    for (answers, received, cause) in cases {
        let events = Arc::new(RecordedEvents::default());

        let plan = planner(ScriptedModel::answering(answers), events.clone())
            .plan(&context_in(TENANT), &received, profile())
            .await;

        assert!(plan.is_none(), "{cause:?}");
        assert_eq!(
            *events.0.lock().expect("lock"),
            vec![DropEvent::for_record(&received, cause)]
        );
    }
}

#[tokio::test]
async fn the_round_cap_stops_the_loop_after_its_last_round() {
    let model = ScriptedModel::answering(vec![Ok(tool_calls(
        &[(
            "add",
            json!({ "property": "skills", "value": { "name": "Go" }, "confidence": 0.5 }),
        )],
        900,
    ))]);

    planner(model.clone(), Arc::new(RecordedEvents::default()))
        .plan(&context_in(TENANT), &record(), profile())
        .await;

    assert_eq!(model.requests().len(), 3);
}
