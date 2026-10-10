use std::mem::discriminant;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::StatusCode;
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::chat_completions::ChatCompletionsModel;
use super::model_client;
use crate::config::ModelConfig;
use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ToolCall, ToolSpec,
    Usage,
};
use crate::test_support::context_in;

const TOOL_CALLS: &str = include_str!("fixtures/chat_tool_calls.json");
const STRUCTURED: &str = include_str!("fixtures/chat_structured.json");
const CONTENT_FILTER: &str = include_str!("fixtures/chat_content_filter.json");

#[derive(Default)]
struct Sent {
    uri: String,
    body: Value,
}

struct FakeGateway {
    status: StatusCode,
    body: String,
    delay: Duration,
    sent: Mutex<Sent>,
}

impl FakeGateway {
    fn answering(status: StatusCode, body: &str) -> Arc<Self> {
        Arc::new(Self {
            status,
            body: body.to_owned(),
            delay: Duration::ZERO,
            sent: Mutex::default(),
        })
    }

    fn slow() -> Arc<Self> {
        Arc::new(Self {
            status: StatusCode::OK,
            body: STRUCTURED.to_owned(),
            delay: Duration::from_secs(5),
            sent: Mutex::default(),
        })
    }
}

#[async_trait]
impl ServiceGatewayClientV1 for FakeGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        unimplemented!()
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn delete_upstream(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn create_route(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn get_route(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn delete_route(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        unimplemented!()
    }
    async fn proxy_request(
        &self,
        _: SecurityContext,
        request: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        let uri = request.uri().to_string();
        let bytes = request
            .into_body()
            .into_bytes()
            .await
            .expect("request body");
        *self.sent.lock().expect("lock") = Sent {
            uri,
            body: serde_json::from_slice(&bytes).expect("a JSON request"),
        };
        tokio::time::sleep(self.delay).await;
        Ok(http::Response::builder()
            .status(self.status)
            .body(Body::Bytes(Bytes::from(self.body.clone())))
            .expect("response"))
    }
}

fn model_behind(gateway: Arc<FakeGateway>) -> ChatCompletionsModel {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn ServiceGatewayClientV1>(gateway);
    ChatCompletionsModel::new(
        hub,
        "llm.example".to_owned(),
        "fact-planner".to_owned(),
        Duration::from_millis(200),
    )
}

fn context() -> SecurityContext {
    context_in(Uuid::new_v4())
}

fn planner_request() -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::System("Decide how the record changes the profile.".to_owned()),
            Message::User("1. work_history: Acme, teacher".to_owned()),
            Message::Assistant {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_owned(),
                    name: "replace".to_owned(),
                    arguments: json!({ "value_number": 1 }),
                }],
            },
            Message::ToolResult {
                call_id: "call_1".to_owned(),
                content: "replaced value 1".to_owned(),
            },
        ],
        tools: vec![ToolSpec {
            name: "add".to_owned(),
            description: "Add a fact.".to_owned(),
            parameters: json!({ "type": "object", "properties": { "property": { "type": "string" } } }),
        }],
        answer: AnswerKind::Text,
        max_output_tokens: Some(512),
    }
}

fn verdict_request() -> ModelRequest {
    ModelRequest {
        messages: vec![Message::User("Check the values.".to_owned())],
        tools: vec![],
        answer: AnswerKind::Structured {
            name: "verdict".to_owned(),
            schema: json!({ "type": "object", "properties": { "verdict": { "type": "string" } } }),
        },
        max_output_tokens: None,
    }
}

#[tokio::test]
async fn the_request_goes_to_the_upstream_in_chat_completions_form() {
    let gateway = FakeGateway::answering(StatusCode::OK, TOOL_CALLS);
    model_behind(gateway.clone())
        .complete(&context(), planner_request())
        .await
        .expect("an answer");

    let sent = gateway.sent.lock().expect("lock");
    assert_eq!(sent.uri, "/llm.example");
    assert_eq!(
        sent.body,
        json!({
            "model": "fact-planner",
            "messages": [
                { "role": "system", "content": "Decide how the record changes the profile." },
                { "role": "user", "content": "1. work_history: Acme, teacher" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "replace", "arguments": "{\"value_number\":1}" },
                    }],
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "replaced value 1" },
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "add",
                    "description": "Add a fact.",
                    "parameters": { "type": "object", "properties": { "property": { "type": "string" } } },
                },
            }],
            "max_completion_tokens": 512,
        })
    );
}

#[tokio::test]
async fn a_structured_answer_asks_for_a_json_schema() {
    let gateway = FakeGateway::answering(StatusCode::OK, STRUCTURED);
    model_behind(gateway.clone())
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    let sent = gateway.sent.lock().expect("lock");
    assert_eq!(
        sent.body["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "verdict",
                "schema": { "type": "object", "properties": { "verdict": { "type": "string" } } },
                "strict": true,
            },
        })
    );
    assert!(sent.body.get("tools").is_none());
}

#[tokio::test]
async fn tool_calls_come_back_with_parsed_arguments_and_usage() {
    let answer = model_behind(FakeGateway::answering(StatusCode::OK, TOOL_CALLS))
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
    let answer = model_behind(FakeGateway::answering(StatusCode::OK, STRUCTURED))
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    assert_eq!(
        answer.output,
        ModelOutput::Structured(json!({ "verdict": "allow", "kind": "health" }))
    );
}

#[tokio::test]
async fn a_failed_call_says_why() {
    let refused = ModelError::Refused(String::new());
    let unavailable = ModelError::Unavailable(String::new());
    let bad_answer = ModelError::BadAnswer(String::new());
    let cases = [
        (StatusCode::OK, CONTENT_FILTER, &refused, "content filter"),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            &unavailable,
            "rate limited",
        ),
        (StatusCode::BAD_GATEWAY, "{}", &unavailable, "upstream down"),
        (
            StatusCode::BAD_REQUEST,
            "{}",
            &refused,
            "request not accepted",
        ),
        (
            StatusCode::OK,
            "not json",
            &bad_answer,
            "not a chat completion",
        ),
        (
            StatusCode::OK,
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c","function":{"name":"add","arguments":"{oops"}}]}}]}"#,
            &bad_answer,
            "arguments not JSON",
        ),
    ];
    for (status, body, expected, case) in cases {
        let error = model_behind(FakeGateway::answering(status, body))
            .complete(&context(), planner_request())
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
async fn a_slow_model_times_out() {
    let error = model_behind(FakeGateway::slow())
        .complete(&context(), verdict_request())
        .await
        .expect_err("too slow");

    assert_eq!(error, ModelError::Timeout);
}

#[tokio::test]
async fn without_an_oagw_client_the_model_is_unavailable() {
    let model = ChatCompletionsModel::new(
        Arc::new(ClientHub::new()),
        "llm.example".to_owned(),
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
    let error = model_behind(FakeGateway::answering(StatusCode::OK, CONTENT_FILTER))
        .complete(&context(), planner_request())
        .await
        .expect_err("refused");

    let text = error.to_string();
    assert!(!text.contains("Acme"), "{text}");
    assert!(!text.contains("Decide how"), "{text}");
}

#[tokio::test]
async fn the_configured_adapter_calls_the_configured_upstream_and_model() {
    let gateway = FakeGateway::answering(StatusCode::OK, STRUCTURED);
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn ServiceGatewayClientV1>(gateway.clone());
    let config = ModelConfig::ChatCompletions {
        upstream_alias: "models.internal".to_owned(),
        model: "planner-large".to_owned(),
        timeout_ms: 1_000,
    };

    model_client(&config, hub)
        .complete(&context(), verdict_request())
        .await
        .expect("an answer");

    let sent = gateway.sent.lock().expect("lock");
    assert_eq!(sent.uri, "/models.internal");
    assert_eq!(sent.body["model"], "planner-large");
}
