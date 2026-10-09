use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::StatusCode;
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::chat_completions::ChatCompletionsModel;
use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ToolCall, ToolSpec,
    Usage,
};
use crate::test_support::context_in;

const TOOL_CALLS: &str = include_str!("fixtures/chat_tool_calls.json");
const STRUCTURED: &str = include_str!("fixtures/chat_structured.json");
const CONTENT_FILTER: &str = include_str!("fixtures/chat_content_filter.json");
const PROMPT_MARKER: &str = "1. work_history: Acme, teacher";

#[resource_error("gts.cf.core.oagw.proxy.v1~")]
struct ProxyError;

#[derive(Default)]
struct Sent {
    uri: String,
    body: Value,
}

type Failure = fn() -> CanonicalError;

enum Answer {
    Http(StatusCode, String),
    Failure(Failure),
}

struct FakeGateway {
    answer: Answer,
    delay: Duration,
    sent: Mutex<Sent>,
}

impl FakeGateway {
    fn new(answer: Answer, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            answer,
            delay,
            sent: Mutex::default(),
        })
    }

    fn answering(status: StatusCode, body: &str) -> Arc<Self> {
        Self::new(Answer::Http(status, body.to_owned()), Duration::ZERO)
    }

    fn failing(failure: Failure) -> Arc<Self> {
        Self::new(Answer::Failure(failure), Duration::ZERO)
    }

    fn slow() -> Arc<Self> {
        Self::new(
            Answer::Http(StatusCode::OK, STRUCTURED.to_owned()),
            Duration::from_secs(5),
        )
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
        match &self.answer {
            Answer::Http(status, body) => Ok(http::Response::builder()
                .status(*status)
                .body(Body::Bytes(Bytes::from(body.clone())))
                .expect("response")),
            Answer::Failure(failure) => Err(failure()),
        }
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
            Message::User(PROMPT_MARKER.to_owned()),
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

fn answer(message: &Value, finish_reason: &str) -> String {
    json!({
        "choices": [{ "message": message, "finish_reason": finish_reason }],
        "usage": { "prompt_tokens": 40, "completion_tokens": 5 },
    })
    .to_string()
}

async fn complete(
    body: &str,
    request: &ModelRequest,
) -> Result<crate::domain::model_client::ModelResponse, ModelError> {
    model_behind(FakeGateway::answering(StatusCode::OK, body))
        .complete(&context(), request)
        .await
}

#[tokio::test]
async fn the_request_goes_to_the_upstream_in_chat_completions_form() {
    let gateway = FakeGateway::answering(StatusCode::OK, TOOL_CALLS);
    model_behind(gateway.clone())
        .complete(&context(), &planner_request())
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
                { "role": "user", "content": PROMPT_MARKER },
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
        .complete(&context(), &verdict_request())
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
    let answer = complete(TOOL_CALLS, &planner_request())
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
        Some(Usage {
            input_tokens: 1843,
            output_tokens: 96,
        })
    );
}

#[tokio::test]
async fn a_structured_answer_comes_back_as_json() {
    let answer = complete(STRUCTURED, &verdict_request())
        .await
        .expect("an answer");

    assert_eq!(
        answer.output,
        ModelOutput::Structured(json!({ "verdict": "allow", "kind": "health" }))
    );
}

#[tokio::test]
async fn a_text_answer_comes_back_as_text() {
    let body = answer(
        &json!({ "content": "Nothing changed.", "tool_calls": null }),
        "stop",
    );

    let answer = complete(&body, &planner_request())
        .await
        .expect("an answer");

    assert_eq!(
        answer.output,
        ModelOutput::Text("Nothing changed.".to_owned())
    );
}

#[tokio::test]
async fn an_answer_without_usage_reports_no_usage() {
    let body =
        json!({ "choices": [{ "message": { "content": "Done." }, "finish_reason": "stop" }] })
            .to_string();

    let answer = complete(&body, &planner_request())
        .await
        .expect("an answer");

    assert_eq!(answer.usage, None);
}

#[tokio::test]
async fn an_unusable_answer_is_rejected_with_its_reason() {
    let refused = || ModelError::Refused("the model declined to answer".to_owned());
    let bad = |reason: &str| ModelError::BadAnswer(reason.to_owned());
    let cases = [
        (CONTENT_FILTER.to_owned(), planner_request(), refused()),
        (
            answer(
                &json!({ "content": null, "refusal": "I can't help with that." }),
                "stop",
            ),
            planner_request(),
            refused(),
        ),
        (
            answer(&json!({ "content": "Half an ans" }), "length"),
            planner_request(),
            bad("the answer was cut off at its token limit"),
        ),
        (
            json!({ "choices": [] }).to_string(),
            planner_request(),
            bad("no choice in the answer"),
        ),
        (
            answer(&json!({ "content": null }), "stop"),
            planner_request(),
            bad("the answer has no content"),
        ),
        (
            answer(&json!({ "content": "  " }), "stop"),
            planner_request(),
            bad("the answer has no content"),
        ),
        (
            "not json".to_owned(),
            planner_request(),
            bad("not a chat completion"),
        ),
        (
            answer(
                &json!({ "content": null, "tool_calls": [{ "id": "c", "function": { "name": "add", "arguments": "{oops" } }] }),
                "tool_calls",
            ),
            planner_request(),
            bad("the arguments of tool call 0 are not JSON"),
        ),
        (
            answer(&json!({ "content": "verdict: allow" }), "stop"),
            verdict_request(),
            bad("the structured answer is not JSON"),
        ),
    ];

    for (body, request, expected) in cases {
        assert_eq!(complete(&body, &request).await, Err(expected), "{body}");
    }
}

#[tokio::test]
async fn an_http_failure_maps_to_unavailable_or_refused() {
    let cases = [
        (
            StatusCode::TOO_MANY_REQUESTS,
            ModelError::Unavailable("HTTP 429 Too Many Requests".to_owned()),
        ),
        (
            StatusCode::BAD_GATEWAY,
            ModelError::Unavailable("HTTP 502 Bad Gateway".to_owned()),
        ),
        (
            StatusCode::BAD_REQUEST,
            ModelError::Refused("HTTP 400 Bad Request".to_owned()),
        ),
    ];

    for (status, expected) in cases {
        let got = model_behind(FakeGateway::answering(status, "{}"))
            .complete(&context(), &planner_request())
            .await;
        assert_eq!(got, Err(expected), "{status}");
    }
}

#[tokio::test]
async fn an_oagw_failure_maps_by_its_kind_and_names_only_the_kind() {
    let cases: [(Failure, ModelError); 4] = [
        (
            || ProxyError::deadline_exceeded("too slow").create(),
            ModelError::Timeout,
        ),
        (
            || CanonicalError::service_unavailable().create(),
            ModelError::Unavailable("OAGW Unavailable".to_owned()),
        ),
        (
            || {
                ProxyError::resource_exhausted("too many requests")
                    .with_quota_violation("proxy", "rate limit")
                    .create()
            },
            ModelError::Unavailable("OAGW RateLimited".to_owned()),
        ),
        (
            || {
                ProxyError::permission_denied()
                    .with_reason("NOT_ENTITLED")
                    .create()
            },
            ModelError::Refused("OAGW PermissionDenied".to_owned()),
        ),
    ];

    for (failure, expected) in cases {
        let got = model_behind(FakeGateway::failing(failure))
            .complete(&context(), &planner_request())
            .await;
        assert_eq!(got, Err(expected));
    }
}

#[tokio::test]
async fn a_slow_model_times_out() {
    let error = model_behind(FakeGateway::slow())
        .complete(&context(), &verdict_request())
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
        .complete(&context(), &verdict_request())
        .await
        .expect_err("no gateway");

    assert_eq!(
        error,
        ModelError::Unavailable("no OAGW client in ClientHub".to_owned())
    );
}

#[tokio::test]
async fn an_error_never_repeats_the_prompt_the_answer_or_the_gateway_detail() {
    let gateway_detail = model_behind(FakeGateway::failing(|| {
        CanonicalError::internal(format!("upstream rejected: {PROMPT_MARKER}")).create()
    }))
    .complete(&context(), &planner_request())
    .await;
    let wrong_type = complete(
        &json!({ "choices": [], "usage": { "prompt_tokens": PROMPT_MARKER, "completion_tokens": 1 } }).to_string(),
        &planner_request(),
    )
    .await;
    let bad_arguments = complete(
        &answer(
            &json!({ "content": null, "tool_calls": [{ "id": "c", "function": { "name": PROMPT_MARKER, "arguments": PROMPT_MARKER } }] }),
            "tool_calls",
        ),
        &planner_request(),
    )
    .await;

    for got in [gateway_detail, wrong_type, bad_arguments] {
        let text = got.expect_err("a failure").to_string();
        assert!(!text.contains(PROMPT_MARKER), "{text}");
    }
}
