use std::mem::discriminant;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayClientV1, LlmGatewayError,
    ResponseEventStream, ResponseResource,
};
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;

use super::llm_gateway::LlmGatewayModel;
use super::model_client;
use super::test_requests::{context, planner_request, verdict_request};
use crate::config::ModelConfig;
use crate::domain::model_client::{ModelClient, ModelError, ModelOutput, ToolCall, Usage};

const TOOL_CALLS: &str = include_str!("fixtures/gateway_tool_calls.json");
const STRUCTURED: &str = include_str!("fixtures/gateway_structured.json");
const CONTENT_FILTER: &str = include_str!("fixtures/gateway_content_filter.json");

type Answer = Box<dyn Fn() -> Result<ResponseResource, LlmGatewayError> + Send + Sync>;
type MakeError = fn() -> LlmGatewayError;

struct FakeGateway {
    answer: Answer,
    delay: Duration,
    sent: Mutex<Option<CreateResponseBody>>,
}

impl FakeGateway {
    fn new(answer: Answer, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            answer,
            delay,
            sent: Mutex::default(),
        })
    }

    fn answering(response: Value) -> Arc<Self> {
        Self::new(
            Box::new(move || Ok(serde_json::from_value(response.clone()).expect("a response"))),
            Duration::ZERO,
        )
    }

    fn failing(error: MakeError) -> Arc<Self> {
        Self::new(Box::new(move || Err(error())), Duration::ZERO)
    }

    fn slow() -> Arc<Self> {
        Self::new(
            Box::new(|| Ok(serde_json::from_str(STRUCTURED).expect("a response"))),
            Duration::from_secs(5),
        )
    }

    fn sent(&self) -> Value {
        serde_json::to_value(self.sent.lock().expect("lock").as_ref().expect("a request"))
            .expect("a JSON request")
    }
}

#[async_trait]
impl LlmGatewayClientV1 for FakeGateway {
    async fn create_response(
        &self,
        _: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        *self.sent.lock().expect("lock") = Some(body);
        tokio::time::sleep(self.delay).await;
        (self.answer)()
    }

    async fn create_response_stream(
        &self,
        _: &SecurityContext,
        _: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        unimplemented!()
    }

    async fn create_embedding(
        &self,
        _: &SecurityContext,
        _: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        unimplemented!()
    }
}

fn fixture(json: &str) -> Value {
    serde_json::from_str(json).expect("a fixture")
}

fn edited(json: &str, pointer: &str, value: Value) -> Value {
    let mut response = fixture(json);
    *response.pointer_mut(pointer).expect("the field") = value;
    response
}

fn model_behind(gateway: Arc<FakeGateway>) -> LlmGatewayModel {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn LlmGatewayClientV1>(gateway);
    LlmGatewayModel::new(hub, "fact-planner".to_owned(), Duration::from_millis(200))
}

#[tokio::test]
async fn the_request_goes_to_the_gateway_in_open_responses_form() {
    let gateway = FakeGateway::answering(fixture(TOOL_CALLS));
    model_behind(gateway.clone())
        .complete(&context(), planner_request())
        .await
        .expect("an answer");

    assert_eq!(
        gateway.sent(),
        json!({
            "model": "fact-planner",
            "input": [
                { "type": "message", "role": "system", "content": "Decide how the record changes the profile." },
                { "type": "message", "role": "user", "content": "1. work_history: Acme, teacher" },
                { "type": "function_call", "call_id": "call_1", "name": "replace", "arguments": "{\"value_number\":1}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "replaced value 1" },
            ],
            "tools": [{
                "type": "function",
                "name": "add",
                "description": "Add a fact.",
                "parameters": { "type": "object", "properties": { "property": { "type": "string" } } },
            }],
            "max_output_tokens": 512,
        })
    );
}

#[tokio::test]
async fn a_structured_answer_asks_for_a_json_schema() {
    let gateway = FakeGateway::answering(fixture(STRUCTURED));
    model_behind(gateway.clone())
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    let sent = gateway.sent();
    assert_eq!(
        sent["text"],
        json!({
            "format": {
                "type": "json_schema",
                "name": "verdict",
                "schema": { "type": "object", "properties": { "verdict": { "type": "string" } } },
                "strict": true,
            },
        })
    );
    assert!(sent.get("tools").is_none());
}

#[tokio::test]
async fn tool_calls_come_back_with_parsed_arguments_and_usage() {
    let answer = model_behind(FakeGateway::answering(fixture(TOOL_CALLS)))
        .complete(&context(), planner_request())
        .await
        .expect("an answer");

    assert_eq!(
        answer.output,
        ModelOutput::ToolCalls(vec![
            ToolCall {
                id: "call_Qm2xY7nR4tVb8kLs1pZc0dEf".to_owned(),
                name: "replace".to_owned(),
                arguments: json!({
                    "value_number": 1,
                    "value": { "work_history": { "organization": "Acme", "title": "School principal" } },
                }),
            },
            ToolCall {
                id: "call_Hj5wK2mN9pRt3vXy6aBc4dFg".to_owned(),
                name: "add".to_owned(),
                arguments: json!({ "property": "skills", "value": { "name": "Curriculum design" } }),
            },
        ])
    );
    assert_eq!(
        answer.usage,
        Usage {
            input_tokens: 1843,
            output_tokens: 96,
        }
    );
}

#[tokio::test]
async fn a_structured_answer_comes_back_as_json() {
    let answer = model_behind(FakeGateway::answering(fixture(STRUCTURED)))
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    assert_eq!(
        answer.output,
        ModelOutput::Structured(json!({ "verdict": "allow", "kind": "health" }))
    );
}

#[tokio::test]
async fn a_failed_answer_says_why() {
    let refused = ModelError::Refused(String::new());
    let unavailable = ModelError::Unavailable(String::new());
    let bad_answer = ModelError::BadAnswer(String::new());
    let cases = [
        (fixture(CONTENT_FILTER), &refused, "content filter"),
        (
            edited(
                STRUCTURED,
                "/output/0/content/0",
                json!({ "type": "refusal", "refusal": "I can't help with that." }),
            ),
            &refused,
            "refusal",
        ),
        (
            edited(
                CONTENT_FILTER,
                "/incomplete_details",
                json!({ "reason": "max_output_tokens" }),
            ),
            &bad_answer,
            "cut off",
        ),
        (
            edited(STRUCTURED, "/status", json!("failed")),
            &unavailable,
            "response failed",
        ),
        (
            edited(STRUCTURED, "/output", json!([])),
            &bad_answer,
            "no content",
        ),
        (
            edited(TOOL_CALLS, "/output/0/arguments", json!("{oops")),
            &bad_answer,
            "arguments not JSON",
        ),
    ];
    for (response, expected, case) in cases {
        let error = model_behind(FakeGateway::answering(response))
            .complete(&context(), verdict_request())
            .await
            .expect_err(case);
        assert_eq!(
            discriminant(&error),
            discriminant(expected),
            "{case}: {error:?}"
        );
    }
}

#[tokio::test]
async fn a_gateway_error_says_why() {
    let cases: [(MakeError, ModelError); 7] = [
        (
            || LlmGatewayError::RateLimited,
            ModelError::Unavailable("rate_limited".to_owned()),
        ),
        (
            || LlmGatewayError::ProviderError {
                message: "upstream 502".to_owned(),
            },
            ModelError::Unavailable("provider_error".to_owned()),
        ),
        (|| LlmGatewayError::ProviderTimeout, ModelError::Timeout),
        (
            || LlmGatewayError::model_not_approved("fact-planner"),
            ModelError::Refused("model_not_approved".to_owned()),
        ),
        (
            || LlmGatewayError::BudgetExceeded,
            ModelError::Refused("budget_exceeded".to_owned()),
        ),
        (
            || LlmGatewayError::request_blocked("pii"),
            ModelError::Refused("request_blocked".to_owned()),
        ),
        (
            || LlmGatewayError::OutputValidationError {
                message: "missing verdict".to_owned(),
            },
            ModelError::BadAnswer("output_validation_error".to_owned()),
        ),
    ];
    for (error, expected) in cases {
        let got = model_behind(FakeGateway::failing(error))
            .complete(&context(), verdict_request())
            .await
            .expect_err("a gateway error");
        assert_eq!(got, expected);
    }
}

#[tokio::test]
async fn a_slow_model_times_out() {
    let error = model_behind(FakeGateway::slow())
        .complete(&context(), verdict_request())
        .await
        .expect_err("too slow");

    assert_eq!(error, ModelError::Timeout);
}

#[tokio::test]
async fn without_a_gateway_client_the_model_is_unavailable() {
    let model = LlmGatewayModel::new(
        Arc::new(ClientHub::new()),
        "fact-planner".to_owned(),
        Duration::from_secs(1),
    );

    let error = model
        .complete(&context(), verdict_request())
        .await
        .expect_err("no gateway");

    assert!(matches!(error, ModelError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn an_error_never_repeats_the_prompt_or_the_answer() {
    let gateways = [
        FakeGateway::answering(fixture(CONTENT_FILTER)),
        FakeGateway::failing(|| LlmGatewayError::ProviderError {
            message: "the provider rejected: 1. work_history: Acme, teacher".to_owned(),
        }),
    ];
    for gateway in gateways {
        let text = model_behind(gateway)
            .complete(&context(), planner_request())
            .await
            .expect_err("failed")
            .to_string();
        assert!(!text.contains("Acme"), "{text}");
        assert!(!text.contains("work_history"), "{text}");
    }
}

#[tokio::test]
async fn the_configured_adapter_calls_the_gateway_with_the_configured_model() {
    let gateway = FakeGateway::answering(fixture(STRUCTURED));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn LlmGatewayClientV1>(gateway.clone());
    let config = ModelConfig::LlmGateway {
        model: "planner-large".to_owned(),
        timeout_ms: 1_000,
    };

    model_client(&config, hub)
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    assert_eq!(gateway.sent()["model"], "planner-large");
}
