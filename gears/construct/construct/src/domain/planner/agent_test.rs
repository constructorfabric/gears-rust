use std::collections::VecDeque;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_security::SecurityContext;

use super::agent::{Planner, PlannerCaps};
use super::test_data::{SUBJECT, TENANT, catalog, origin, profile};
use crate::domain::model_client::{
    Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::plan::{Change, Origin, Plan, Step};
use crate::domain::record_intake::{DropEvent, DropReason, DropStage, ReceivedRecord};
use crate::test_support::{RecordingEvents, context_in, received_chat_record};

type Answer = Result<ModelResponse, ModelError>;

fn caps() -> PlannerCaps {
    PlannerCaps {
        max_rounds: NonZeroU32::new(3).expect("rounds"),
        max_tokens: NonZeroU64::new(10_000).expect("tokens"),
    }
}

struct ScriptedModel {
    answers: Mutex<VecDeque<Answer>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn answering(answers: Vec<Answer>) -> Arc<Self> {
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

fn usage(input_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens: 20,
    }
}

fn tool_calls(calls: &[(&str, Value)], input_tokens: u64) -> ModelResponse {
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

fn done(input_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: ModelOutput::Text("The plan is complete.".to_owned()),
        usage: Some(usage(input_tokens)),
    }
}

fn add_go() -> (&'static str, Value) {
    (
        "add",
        json!({ "property": "skills", "value": { "name": "Go" }, "confidence": 0.5 }),
    )
}

fn record() -> ReceivedRecord {
    received_chat_record(TENANT, SUBJECT)
}

async fn plan_with(
    answers: Vec<Answer>,
    received: &ReceivedRecord,
) -> (Option<Plan>, Arc<ScriptedModel>, Arc<RecordingEvents>) {
    plan_under(caps(), answers, received).await
}

async fn plan_under(
    caps: PlannerCaps,
    answers: Vec<Answer>,
    received: &ReceivedRecord,
) -> (Option<Plan>, Arc<ScriptedModel>, Arc<RecordingEvents>) {
    let model = ScriptedModel::answering(answers);
    let events = Arc::new(RecordingEvents::default());
    let plan = Planner::new(model.clone(), Arc::new(catalog()), events.clone(), caps)
        .plan(&context_in(TENANT), received, profile())
        .await;
    (plan, model, events)
}

#[tokio::test]
async fn the_tool_calls_of_each_round_become_a_plan_for_the_record_subject() {
    let (plan, model, events) = plan_with(
        vec![
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
            Ok(done(1000)),
        ],
        &record(),
    )
    .await;

    let plan = plan.expect("a plan");
    assert_eq!(
        (
            plan.tenant_id,
            plan.subject_id,
            &plan.origin,
            plan.profile_version
        ),
        (TENANT, SUBJECT, &origin(), Some(7))
    );
    assert!(
        matches!(
            plan.steps.as_slice(),
            [
                Step {
                    change: Change::AddEntity { .. },
                    ..
                },
                Step {
                    change: Change::ReplaceEntity { .. },
                    ..
                }
            ]
        ),
        "{:?}",
        plan.steps
    );
    assert_eq!(model.requests().len(), 3);
    assert!(events.taken().is_empty());
}

#[tokio::test]
async fn each_round_sends_the_assistant_message_before_its_tool_results() {
    let (_, model, _) = plan_with(
        vec![
            Ok(tool_calls(
                &[(
                    "add",
                    json!({ "property": "role", "value": "dean", "confidence": 0.5 }),
                )],
                900,
            )),
            Ok(done(950)),
        ],
        &record(),
    )
    .await;

    let requests = model.requests();
    let tail: Vec<&Message> = requests[1].messages.iter().skip(2).collect();
    assert_eq!(
        tail,
        vec![
            &Message::Assistant {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_0".to_owned(),
                    name: "add".to_owned(),
                    arguments: json!({ "property": "role", "value": "dean", "confidence": 0.5 }),
                }],
            },
            &Message::ToolResult {
                call_id: "call_0".to_owned(),
                content: "`role` holds one value, and value 1 is it; use replace 1".to_owned(),
            },
        ]
    );
}

#[tokio::test]
async fn the_model_gets_the_record_and_the_profile_as_data_but_no_id() {
    let (_, model, _) = plan_with(vec![Ok(done(900))], &record()).await;

    let first = model.requests().remove(0);
    let Message::User(input) = &first.messages[1] else {
        panic!("the record input: {:?}", first.messages);
    };
    assert!(input.starts_with("<record>\n"), "{input}");
    assert!(
        input.contains("Which of these two papers contradict each other?"),
        "{input}"
    );
    assert!(
        input.ends_with(
            "<profile>\n1. role (role): \"teacher\"\n2. skills (skill): {\"name\":\"Python\"}\n3. full_name (identity): \"Jane Doe\"\n</profile>"
        ),
        "{input}"
    );
    for message in &first.messages {
        let (Message::System(text) | Message::User(text)) = message else {
            panic!("only instructions and input in the first request: {message:?}");
        };
        for id in profile()
            .facts
            .iter()
            .map(|fact| fact.node_key.0.clone())
            .chain([TENANT.to_string(), SUBJECT.to_string()])
        {
            assert!(!text.contains(&id), "{text} shows {id}");
        }
        assert!(!text.contains("gts."), "{text} shows a type id");
        for source_id in ["msg-7", "thread-42"] {
            assert!(!text.contains(source_id), "{text} shows {source_id}");
        }
    }
}

#[tokio::test]
async fn each_round_may_answer_with_the_tokens_still_left() {
    let (_, model, _) = plan_with(
        vec![Ok(tool_calls(&[add_go()], 900)), Ok(done(950))],
        &record(),
    )
    .await;

    let limits: Vec<Option<u32>> = model
        .requests()
        .iter()
        .map(|request| request.max_output_tokens)
        .collect();
    assert_eq!(limits, vec![Some(10_000), Some(10_000 - 920)]);
}

#[tokio::test]
async fn the_tokens_of_all_rounds_add_up_to_the_cap() {
    let (at_the_cap, _, _) = plan_with(
        vec![Ok(tool_calls(&[add_go()], 4_980)), Ok(done(4_980))],
        &record(),
    )
    .await;
    let (over_the_cap, _, events) = plan_with(
        vec![Ok(tool_calls(&[add_go()], 4_980)), Ok(done(4_981))],
        &record(),
    )
    .await;

    assert!(
        at_the_cap.is_some(),
        "10 000 tokens is at the cap, not over it"
    );
    assert!(over_the_cap.is_none());
    assert_eq!(
        events.taken(),
        vec![DropEvent::for_record(
            &record(),
            DropStage::Planner,
            DropReason::TokenCap
        )]
    );
}

#[tokio::test]
async fn a_cap_past_the_largest_answer_limit_asks_for_the_largest() {
    let caps = PlannerCaps {
        max_tokens: NonZeroU64::new(u64::from(u32::MAX) + 1_000).expect("tokens"),
        ..caps()
    };

    let (_, model, _) = plan_under(caps, vec![Ok(done(900))], &record()).await;

    assert_eq!(model.requests()[0].max_output_tokens, Some(u32::MAX));
}

#[tokio::test]
async fn a_round_that_ends_at_the_cap_drops_the_record_before_another_call() {
    let (plan, model, events) = plan_with(
        vec![Ok(tool_calls(&[add_go()], 9_980)), Ok(done(0))],
        &record(),
    )
    .await;

    assert!(plan.is_none());
    assert_eq!(model.requests().len(), 1);
    assert_eq!(
        events.taken(),
        vec![DropEvent::for_record(
            &record(),
            DropStage::Planner,
            DropReason::TokenCap
        )]
    );
}

#[tokio::test]
async fn a_record_too_long_for_the_token_cap_is_dropped_before_any_model_call() {
    let mut long = record();
    long.record["payload"] = json!({ "text": "word ".repeat(8_000) });
    let mut short = record();
    short.record["payload"] = json!({ "text": "word ".repeat(7_000) });

    let (plan, model, events) = plan_with(vec![Ok(done(900))], &long).await;
    let (fits, _, _) = plan_with(vec![Ok(done(900))], &short).await;

    assert!(plan.is_none());
    assert!(model.requests().is_empty());
    assert_eq!(
        events.taken(),
        vec![DropEvent::for_record(
            &long,
            DropStage::Planner,
            DropReason::TokenCap
        )]
    );
    assert!(fits.is_some());
}

#[tokio::test]
async fn the_plan_origin_carries_when_the_record_was_observed() {
    let mut received = record();
    received.envelope.observed_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);

    let (plan, _, _) = plan_with(vec![Ok(done(900))], &received).await;

    let Origin::Connector { observed_at, .. } = plan.expect("a plan").origin else {
        panic!("a connector origin");
    };
    assert_eq!(observed_at, received.envelope.observed_at);
}

#[tokio::test]
async fn a_step_carries_no_more_confidence_than_its_record() {
    let mut candidate = record();
    candidate.record["payload"]["confidence"] = json!(0.6);
    let adds = |confidence: f64| {
        Ok(tool_calls(
            &[(
                "add",
                json!({ "property": "skills", "value": { "name": "Go" }, "confidence": confidence }),
            )],
            900,
        ))
    };

    let (above, _, _) = plan_with(vec![adds(0.9), Ok(done(950))], &candidate).await;
    let (below, _, _) = plan_with(vec![adds(0.5), Ok(done(950))], &candidate).await;
    let (no_cap, _, _) = plan_with(vec![adds(0.9), Ok(done(950))], &record()).await;

    let confidence = |plan: Option<Plan>| plan.expect("a plan").steps[0].confidence;
    assert_eq!(confidence(above), Some(0.6));
    assert_eq!(confidence(below), Some(0.5));
    assert_eq!(confidence(no_cap), Some(0.9));
}

#[tokio::test]
async fn each_failure_drops_the_record_with_its_cause_and_no_plan() {
    let no_usage = Ok(ModelResponse {
        output: ModelOutput::Text("Done.".to_owned()),
        usage: None,
    });
    let structured = Ok(ModelResponse {
        output: ModelOutput::Structured(json!({ "verdict": "allow" })),
        usage: Some(usage(900)),
    });
    let cases = [
        (vec![Ok(tool_calls(&[add_go()], 900))], DropReason::RoundCap),
        (
            vec![Ok(tool_calls(&[add_go()], 20_000))],
            DropReason::TokenCap,
        ),
        (
            vec![Ok(tool_calls(&[add_go()], u64::MAX))],
            DropReason::TokenCap,
        ),
        (vec![Err(ModelError::Timeout)], DropReason::ModelFailed),
        (
            vec![
                Ok(tool_calls(&[add_go()], 900)),
                Err(ModelError::Unavailable("HTTP 502".to_owned())),
            ],
            DropReason::ModelFailed,
        ),
        (vec![no_usage], DropReason::ModelFailed),
        (vec![structured], DropReason::ModelFailed),
    ];

    for (answers, cause) in cases {
        let (plan, _, events) = plan_with(answers, &record()).await;

        assert!(plan.is_none(), "{cause:?}");
        assert_eq!(
            events.taken(),
            vec![DropEvent::for_record(&record(), DropStage::Planner, cause)]
        );
    }
}

#[tokio::test]
async fn a_record_without_a_subject_is_dropped_before_any_model_call() {
    let mut received = record();
    received.envelope.subject_id = None;

    let (plan, model, events) = plan_with(vec![Ok(done(900))], &received).await;

    assert!(plan.is_none());
    assert!(model.requests().is_empty());
    assert_eq!(
        events.taken(),
        vec![DropEvent::for_record(
            &received,
            DropStage::Planner,
            DropReason::ProcessingFailed
        )]
    );
}

#[tokio::test]
async fn the_round_cap_stops_the_loop_after_its_last_round() {
    let (_, model, _) = plan_with(vec![Ok(tool_calls(&[add_go()], 900))], &record()).await;

    assert_eq!(model.requests().len(), 3);
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_drop_is_logged_by_its_kind_without_the_prompt_or_the_answer() {
    plan_with(
        vec![Err(ModelError::BadAnswer(
            "Which of these two papers contradict each other?".to_owned(),
        ))],
        &record(),
    )
    .await;

    assert!(logs_contain("event=\"planner_dropped_record\""));
    assert!(logs_contain("kind=\"model_bad_answer\""));
    assert!(logs_contain(&format!("tenant_id={TENANT}")));
    assert!(!logs_contain("contradict"));
}
