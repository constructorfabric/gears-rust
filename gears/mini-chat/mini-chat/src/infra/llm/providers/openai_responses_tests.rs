// Created: 2026-04-14 by Constructor Tech
#![allow(clippy::str_to_string)]
use super::*;
use crate::domain::llm::WebSearchContextSize;
use crate::infra::llm::request::{FeatureFlag, RequestMetadata, RequestType};
use crate::infra::llm::{LlmMessage, LlmProvider, LlmTool, llm_request};

use std::sync::Mutex;

use futures::StreamExt;
use oagw_sdk::models::*;
use toolkit_canonical_errors::{CanonicalError, resource_error};

// ── MockGateway ───────────────────────────────────────────────────────

/// What the mock should return from `proxy_request`.
enum MockResponse {
    /// Return an SSE stream from raw byte chunks.
    Sse(Vec<String>),
    /// Return a JSON body (non-SSE).
    Json(serde_json::Value),
    /// Return a `CanonicalError` (the SDK trait return type).
    Error(CanonicalError),
}

struct MockGateway {
    response: Mutex<Option<MockResponse>>,
    last_request: Mutex<Option<(String, String)>>, // (uri, body)
}

impl MockGateway {
    fn returning_sse(events: Vec<String>) -> Arc<Self> {
        Arc::new(MockGateway {
            response: Mutex::new(Some(MockResponse::Sse(events))),
            last_request: Mutex::new(None),
        })
    }

    fn returning_json(json: serde_json::Value) -> Arc<Self> {
        Arc::new(MockGateway {
            response: Mutex::new(Some(MockResponse::Json(json))),
            last_request: Mutex::new(None),
        })
    }

    fn returning_error(err: CanonicalError) -> Arc<Self> {
        Arc::new(MockGateway {
            response: Mutex::new(Some(MockResponse::Error(err))),
            last_request: Mutex::new(None),
        })
    }

    fn last_request_uri(&self) -> Option<String> {
        self.last_request
            .lock()
            .unwrap()
            .as_ref()
            .map(|(u, _)| u.clone())
    }

    fn last_request_body(&self) -> Option<String> {
        self.last_request
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, b)| b.clone())
    }
}

#[async_trait::async_trait]
impl ServiceGatewayClientV1 for MockGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: CreateUpstreamRequest,
    ) -> Result<Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &ListQuery,
    ) -> Result<Vec<Upstream>, CanonicalError> {
        unimplemented!()
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: UpdateUpstreamRequest,
    ) -> Result<Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn delete_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn create_route(
        &self,
        _: SecurityContext,
        _: CreateRouteRequest,
    ) -> Result<Route, CanonicalError> {
        unimplemented!()
    }
    async fn get_route(&self, _: SecurityContext, _: uuid::Uuid) -> Result<Route, CanonicalError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<uuid::Uuid>,
        _: &ListQuery,
    ) -> Result<Vec<Route>, CanonicalError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: UpdateRouteRequest,
    ) -> Result<Route, CanonicalError> {
        unimplemented!()
    }
    async fn delete_route(&self, _: SecurityContext, _: uuid::Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(Upstream, Route), CanonicalError> {
        unimplemented!()
    }
    async fn proxy_request(
        &self,
        _ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        let uri = req.uri().to_string();
        let (_parts, body) = req.into_parts();
        let body_bytes = body.into_bytes().await.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        *self.last_request.lock().unwrap() = Some((uri, body_str));

        let mock_resp = self
            .response
            .lock()
            .unwrap()
            .take()
            .expect("MockGateway response already consumed");

        match mock_resp {
            MockResponse::Sse(events) => {
                let mut sse_bytes = String::new();
                for event_str in &events {
                    sse_bytes.push_str(event_str);
                    sse_bytes.push_str("\n\n");
                }
                let body = Body::Stream(Box::pin(futures::stream::once(async move {
                    Ok(Bytes::from(sse_bytes))
                })));

                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(body)
                    .unwrap();
                Ok(response)
            }
            MockResponse::Json(json) => {
                let body = Body::Bytes(Bytes::from(serde_json::to_vec(&json).unwrap()));
                let response = http::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap();
                Ok(response)
            }
            MockResponse::Error(err) => Err(err),
        }
    }
}

fn test_security_context() -> SecurityContext {
    SecurityContext::anonymous()
}

fn sse_event(event_type: &str, data: &str) -> String {
    format!("event: {event_type}\ndata: {data}")
}

// ── Unit tests: request builder ────────────────────────────────────────

#[test]
fn builder_minimal_text_request() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hello"))
        .system_instructions("You are helpful")
        .max_output_tokens(4096)
        .user_identity("abc", "def")
        .metadata(RequestMetadata {
            tenant_id: "abc".into(),
            user_id: "def".into(),
            chat_id: "ghi".into(),
            request_type: RequestType::Chat,
            features: vec![],
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["user"], "abc:def");
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["instructions"], "You are helpful");
    assert!(body["previous_response_id"].is_null());
    assert_eq!(body["metadata"]["tenant_id"], "abc");
    assert_eq!(body["metadata"]["user_id"], "def");
    assert_eq!(body["metadata"]["chat_id"], "ghi");
    assert_eq!(body["metadata"]["request_type"], "chat");
    assert_eq!(body["metadata"]["feature"], "none");
}

#[test]
fn builder_file_search_tool() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::FileSearch {
            vector_store_ids: vec!["vs-123".into()],
            filters: None,
            max_num_results: None,
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"][0]["type"], "file_search");
    // Responses API: flat format — vector_store_ids at top level, not nested
    assert_eq!(body["tools"][0]["vector_store_ids"][0], "vs-123");
    assert!(body["tools"][0]["filters"].is_null());
}

#[test]
fn builder_file_search_tool_with_filter() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::FileSearch {
            vector_store_ids: vec!["vs-123".into()],
            filters: Some(FileSearchFilter::Eq {
                key: "attachment_id".into(),
                value: "abc-123".into(),
            }),
            max_num_results: None,
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"][0]["type"], "file_search");
    // Responses API: flat format
    let tool = &body["tools"][0];
    assert_eq!(tool["vector_store_ids"][0], "vs-123");
    assert_eq!(tool["filters"]["type"], "eq");
    assert_eq!(tool["filters"]["key"], "attachment_id");
    assert_eq!(tool["filters"]["value"], "abc-123");
}

#[test]
fn builder_web_search_tool() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::WebSearch {
            search_context_size: WebSearchContextSize::Low,
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"][0]["type"], "web_search");
    assert_eq!(body["tools"][0]["search_context_size"], "low");
}

#[test]
fn builder_code_interpreter_tool() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::CodeInterpreter {
            file_ids: vec!["file-abc".into(), "file-def".into()],
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"][0]["type"], "code_interpreter");
    assert_eq!(body["tools"][0]["container"]["type"], "auto");
    assert_eq!(body["tools"][0]["container"]["file_ids"][0], "file-abc");
    assert_eq!(body["tools"][0]["container"]["file_ids"][1], "file-def");
    assert_eq!(
        body["include"],
        serde_json::json!(["code_interpreter_call.outputs"])
    );
}

#[test]
fn builder_no_include_without_code_interpreter() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::WebSearch {
            search_context_size: WebSearchContextSize::Low,
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert!(body.get("include").is_none(), "{body}");
}

#[test]
fn builder_max_tool_calls_and_max_num_results() {
    let request = llm_request("gpt-4o")
        .max_tool_calls(3)
        .tools(vec![
            LlmTool::FileSearch {
                vector_store_ids: vec!["vs-001".into()],
                filters: None,
                max_num_results: Some(10),
            },
            LlmTool::WebSearch {
                search_context_size: WebSearchContextSize::High,
            },
        ])
        .message(LlmMessage::user("test"))
        .build_streaming();

    let body = build_request_body(&request, true);

    // max_tool_calls at top level
    assert_eq!(body["max_tool_calls"], 3);

    // file_search tool has max_num_results
    assert_eq!(body["tools"][0]["type"], "file_search");
    assert_eq!(body["tools"][0]["max_num_results"], 10);

    // web_search tool has search_context_size
    assert_eq!(body["tools"][1]["type"], "web_search");
    assert_eq!(body["tools"][1]["search_context_size"], "high");
}

#[test]
fn builder_max_tool_calls_absent_when_not_set() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("test"))
        .build_streaming();

    let body = build_request_body(&request, true);
    assert!(body.get("max_tool_calls").is_none());
}

#[test]
fn builder_file_search_max_num_results_absent_when_none() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::FileSearch {
            vector_store_ids: vec!["vs-001".into()],
            filters: None,
            max_num_results: None,
        })
        .build_streaming();

    let body = build_request_body(&request, true);
    assert_eq!(body["tools"][0]["type"], "file_search");
    assert!(body["tools"][0].get("max_num_results").is_none());
}

#[test]
fn builder_both_tools_and_feature() {
    let request = llm_request("gpt-4o")
        .tools(vec![
            LlmTool::FileSearch {
                vector_store_ids: vec!["vs-123".into()],
                filters: None,
                max_num_results: None,
            },
            LlmTool::WebSearch {
                search_context_size: WebSearchContextSize::Low,
            },
        ])
        .metadata(RequestMetadata {
            tenant_id: "t1".into(),
            user_id: "u1".into(),
            chat_id: "c1".into(),
            request_type: RequestType::Chat,
            features: vec![FeatureFlag::FileSearch, FeatureFlag::WebSearch],
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(body["tools"][0]["type"], "file_search");
    assert_eq!(body["tools"][1]["type"], "web_search");
    assert_eq!(body["metadata"]["feature"], "file_search+web_search");
}

#[test]
fn builder_code_interpreter_feature() {
    let request = llm_request("gpt-4o")
        .tools(vec![LlmTool::CodeInterpreter {
            file_ids: vec!["file-1".into()],
        }])
        .metadata(RequestMetadata {
            tenant_id: "t1".into(),
            user_id: "u1".into(),
            chat_id: "c1".into(),
            request_type: RequestType::Chat,
            features: vec![FeatureFlag::CodeInterpreter],
        })
        .build_streaming();

    let body = build_request_body(&request, true);
    assert_eq!(body["metadata"]["feature"], "code_interpreter");
}

#[test]
fn builder_file_search_and_code_interpreter_feature() {
    let request = llm_request("gpt-4o")
        .tools(vec![
            LlmTool::FileSearch {
                vector_store_ids: vec!["vs-123".into()],
                filters: None,
                max_num_results: None,
            },
            LlmTool::CodeInterpreter {
                file_ids: vec!["file-1".into()],
            },
        ])
        .metadata(RequestMetadata {
            tenant_id: "t1".into(),
            user_id: "u1".into(),
            chat_id: "c1".into(),
            request_type: RequestType::Chat,
            features: vec![FeatureFlag::FileSearch, FeatureFlag::CodeInterpreter],
        })
        .build_streaming();

    let body = build_request_body(&request, true);
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(body["metadata"]["feature"], "file_search+code_interpreter");
}

#[test]
fn builder_multimodal_input() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user_with_image("Describe this", "file-abc"))
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "Describe this");
    assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(body["input"][0]["content"][1]["file_id"], "file-abc");
}

#[test]
fn builder_non_streaming_mode() {
    let request = llm_request("gpt-4o").build_non_streaming();

    let body = build_request_body(&request, false);

    assert_eq!(body["stream"], false);
}

#[test]
fn builder_streaming_mode() {
    let request = llm_request("gpt-4o").build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["stream"], true);
}

#[test]
fn builder_user_field_format() {
    let request = llm_request("gpt-4o")
        .user_identity("tenant-1", "user-2")
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["user"], "tenant-1:user-2");
}

#[test]
fn builder_function_tool_included() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::Function {
            name: "search_knowledge".into(),
            description: "Search the knowledge base".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    // Function tools are forwarded to the Responses API for agentic tool use
    let tools = body.get("tools").expect("tools should be present");
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "search_knowledge");
}

/// Test helper: build a `ModelApiParams` with sensible defaults that
/// individual tests override field-by-field.
fn test_api_params() -> mini_chat_sdk::ModelApiParams {
    mini_chat_sdk::ModelApiParams {
        temperature: Some(0.7),
        top_p: Some(1.0),
        frequency_penalty: Some(0.0),
        presence_penalty: Some(0.0),
        stop: vec![],
        extra_body: None,
        reasoning_effort: None,
    }
}

#[test]
fn builder_reasoning_effort_nested_under_reasoning() {
    let request = llm_request("o3")
        .message(LlmMessage::user("Think hard"))
        .api_params(mini_chat_sdk::ModelApiParams {
            temperature: Some(1.0),
            reasoning_effort: Some("high".into()),
            ..test_api_params()
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    // Top-level Chat-Completions-style key is never written
    assert!(
        body.get("reasoning_effort").is_none(),
        "reasoning_effort should not appear at top level"
    );
    // Responses API expects `reasoning: { effort }`
    assert_eq!(body["reasoning"]["effort"], "high");
    // Other typed params land at top level
    assert_eq!(body["temperature"], 1.0);
}

/// Unset sampling parameters are not sent: reasoning models (gpt-5-mini)
/// reject `temperature` with 400 "Unsupported parameter".
#[test]
fn builder_omits_unset_sampling_params() {
    let request = llm_request("gpt-5-mini")
        .message(LlmMessage::user("Hello"))
        .api_params(mini_chat_sdk::ModelApiParams {
            temperature: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            reasoning_effort: Some("low".into()),
            ..test_api_params()
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    for key in [
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
    ] {
        assert!(body.get(key).is_none(), "{key} must not be sent: {body}");
    }
    assert_eq!(body["reasoning"]["effort"], "low");
}

#[test]
fn builder_no_reasoning_key_when_effort_absent() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hello"))
        .api_params(test_api_params())
        .build_streaming();

    let body = build_request_body(&request, true);

    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("reasoning").is_none());
}

// ── Unit tests: FromServerEvent ────────────────────────────────────────

#[test]
fn parse_text_delta_event() {
    let event = ServerEvent {
        event: Some("response.output_text.delta".to_string()),
        data: r#"{"delta":"Hello"}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ProviderEvent::ResponseOutputTextDelta { delta } if delta == "Hello"));
}

#[test]
fn parse_event_without_event_line_uses_data_type() {
    for name in [None, Some("message".to_string())] {
        let event = ServerEvent {
            event: name,
            data: r#"{"type":"response.output_text.delta","delta":"Hi"}"#.to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        assert!(
            matches!(result, ProviderEvent::ResponseOutputTextDelta { delta } if delta == "Hi")
        );
    }
}

#[test]
fn parse_event_line_takes_precedence_over_data_type() {
    let event = ServerEvent {
        event: Some("response.output_text.done".to_string()),
        data: r#"{"type":"response.output_text.delta","text":"Done"}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ProviderEvent::ResponseOutputTextDone { text } if text == "Done"));
}

#[test]
fn parse_event_without_event_line_or_type_is_unknown() {
    // Unparseable data, valid JSON without `type`, and JSON with other keys:
    // none names the event, so it stays the SSE default `message`.
    for data in ["not json", "{}", r#"{"foo":1}"#] {
        let event = ServerEvent {
            event: None,
            data: data.to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        assert!(
            matches!(&result, ProviderEvent::Unknown { event_name } if event_name == "message"),
            "{data}: {result:?}"
        );
    }
}

#[test]
fn parse_text_done_event() {
    let event = ServerEvent {
        event: Some("response.output_text.done".to_string()),
        data: r#"{"text":"Hello world"}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(
        matches!(result, ProviderEvent::ResponseOutputTextDone { text } if text == "Hello world")
    );
}

#[test]
fn parse_file_search_searching_event() {
    let event = ServerEvent {
        event: Some("response.file_search_call.searching".to_string()),
        data: "{}".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(
        result,
        ProviderEvent::ResponseFileSearchCallSearching
    ));
}

#[test]
fn parse_file_search_completed_event() {
    let event = ServerEvent {
        event: Some("response.file_search_call.completed".to_string()),
        data:
            r#"{"results":[{"file_id":"f1","filename":"test.pdf","score":0.95,"text":"snippet"}]}"#
                .to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseFileSearchCallCompleted { results } => {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].file_id, "f1");
        }
        _ => panic!("expected ResponseFileSearchCallCompleted"),
    }
}

#[test]
fn parse_web_search_searching_event() {
    let event = ServerEvent {
        event: Some("response.web_search_call.searching".to_string()),
        data: "{}".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(
        result,
        ProviderEvent::ResponseWebSearchCallSearching
    ));
}

#[test]
fn parse_web_search_completed_event() {
    let event = ServerEvent {
        event: Some("response.web_search_call.completed".to_string()),
        data: "{}".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(
        result,
        ProviderEvent::ResponseWebSearchCallCompleted
    ));
}

#[test]
fn parse_code_interpreter_in_progress_event() {
    let event = ServerEvent {
        event: Some("response.code_interpreter_call.in_progress".to_string()),
        data: "{}".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(
        result,
        ProviderEvent::ResponseCodeInterpreterCallInProgress
    ));
}

#[test]
fn parse_code_interpreter_interpreting_event_is_ignored() {
    let event = ServerEvent {
        event: Some("response.code_interpreter_call.interpreting".to_string()),
        data: "{}".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ProviderEvent::Unknown { .. }));
}

#[test]
fn parse_code_interpreter_completed_event_is_ignored() {
    // Real shape: no outputs; they come in `response.output_item.done`.
    let event = ServerEvent {
        event: Some("response.code_interpreter_call.completed".to_string()),
        data: r#"{"type":"response.code_interpreter_call.completed","item_id":"ci_1","output_index":0,"sequence_number":7}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ProviderEvent::Unknown { .. }));
}

fn output_item_done(item: &str) -> ServerEvent {
    ServerEvent {
        event: Some("response.output_item.done".to_string()),
        data: format!(
            r#"{{"type":"response.output_item.done","output_index":0,"sequence_number":9,"item":{item}}}"#
        ),
        id: None,
        retry: None,
    }
}

#[test]
fn parse_output_item_done_code_interpreter_extracts_logs() {
    let event = output_item_done(
        r#"{"type":"code_interpreter_call","id":"ci_1","status":"completed","code":"print(42)","container_id":"cntr_1","outputs":[{"type":"logs","logs":"result text"}]}"#,
    );
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
            assert_eq!(output, "result text");
        }
        other => panic!("expected ResponseCodeInterpreterCallCompleted, got {other:?}"),
    }
}

#[test]
fn parse_output_item_done_code_interpreter_ignores_image_outputs() {
    let event = output_item_done(
        r#"{"type":"code_interpreter_call","id":"ci_1","status":"completed","outputs":[
            {"type":"image","url":"https://example.com/a.png"},
            {"type":"logs","logs":"only this"},
            {"type":"logs","logs":"and this"}
        ]}"#,
    );
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
            assert_eq!(output, "only this\nand this");
        }
        other => panic!("expected ResponseCodeInterpreterCallCompleted, got {other:?}"),
    }
}

#[test]
fn parse_output_item_done_code_interpreter_null_outputs() {
    // Without `include: ["code_interpreter_call.outputs"]` outputs is null.
    let event = output_item_done(
        r#"{"type":"code_interpreter_call","id":"ci_1","status":"completed","outputs":null}"#,
    );
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
            assert_eq!(output, "");
        }
        other => panic!("expected ResponseCodeInterpreterCallCompleted, got {other:?}"),
    }
}

#[test]
fn parse_output_item_done_code_interpreter_truncates_long_output() {
    let long = "x".repeat(MAX_CODE_INTERPRETER_OUTPUT_CHARS + 10);
    let event = output_item_done(&format!(
        r#"{{"type":"code_interpreter_call","outputs":[{{"type":"logs","logs":"{long}"}}]}}"#
    ));
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
            let expected = format!(
                "{}...[truncated]",
                "x".repeat(MAX_CODE_INTERPRETER_OUTPUT_CHARS)
            );
            assert_eq!(output, expected);
        }
        other => panic!("expected ResponseCodeInterpreterCallCompleted, got {other:?}"),
    }
}

#[test]
fn parse_output_item_done_other_items_are_ignored() {
    for item in [
        r#"{"type":"message","id":"msg_1","role":"assistant","content":[]}"#,
        r#"{"type":"file_search_call","id":"fs_1","status":"completed","queries":["q"],"results":null}"#,
        r#"{"type":"web_search_call","id":"ws_1","status":"completed"}"#,
    ] {
        let result = ProviderEvent::from_server_event(output_item_done(item)).unwrap();
        assert!(
            matches!(result, ProviderEvent::Unknown { .. }),
            "{item}: {result:?}"
        );
    }
}

#[test]
fn parse_response_completed_event() {
    let event = ServerEvent {
        event: Some("response.completed".to_string()),
        data: r#"{"response":{"id":"resp-abc","output":[{"type":"message","content":[{"type":"output_text","text":"Hello","annotations":[]}]}],"usage":{"input_tokens":100,"output_tokens":50}}}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCompleted { response } => {
            assert_eq!(response.id, "resp-abc");
            assert_eq!(response.usage.input_tokens, 100);
            assert_eq!(response.usage.output_tokens, 50);
        }
        _ => panic!("expected ResponseCompleted"),
    }
}

#[test]
fn parse_response_completed_with_token_details() {
    let event = ServerEvent {
        event: Some("response.completed".to_string()),
        data: r#"{"response":{"id":"resp-abc","output":[],"usage":{"input_tokens":800,"output_tokens":200,"input_tokens_details":{"cached_tokens":300},"output_tokens_details":{"reasoning_tokens":60}}}}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseCompleted { response } => {
            assert_eq!(
                response
                    .usage
                    .input_tokens_details
                    .as_ref()
                    .unwrap()
                    .cached_tokens,
                300
            );
            assert_eq!(
                response
                    .usage
                    .output_tokens_details
                    .as_ref()
                    .unwrap()
                    .reasoning_tokens,
                60
            );
        }
        _ => panic!("expected ResponseCompleted"),
    }
}

fn parse_error(event_name: &str, data: &str) -> ProviderErrorPayload {
    let event = ServerEvent {
        event: Some(event_name.to_string()),
        data: data.to_string(),
        id: None,
        retry: None,
    };
    match ProviderEvent::from_server_event(event).unwrap() {
        ProviderEvent::ResponseFailed { error, .. } => error,
        other => panic!("expected ResponseFailed, got {other:?}"),
    }
}

#[test]
fn parse_response_failed_event() {
    // Real shape: the error is inside `response`.
    let error = parse_error(
        "response.failed",
        r#"{"type":"response.failed","sequence_number":5,"response":{"id":"resp_1","object":"response","status":"failed","error":{"code":"server_error","message":"internal failure"},"incomplete_details":null,"output":[],"usage":null}}"#,
    );
    assert_eq!(error.code, "server_error");
    assert_eq!(error.message, "internal failure");
}

#[test]
fn parse_response_failed_top_level_error_fallback() {
    let error = parse_error(
        "response.failed",
        r#"{"error":{"code":"server_error","message":"internal failure"}}"#,
    );
    assert_eq!(error.code, "server_error");
    assert_eq!(error.message, "internal failure");
}

/// No known error shape: the raw payload becomes the message, as for the
/// `error` event, so the cause is not lost.
#[test]
fn parse_response_failed_without_error_keeps_payload() {
    let data = r#"{"response":{"id":"resp_1","status":"failed","error":null}}"#;
    let error = parse_error("response.failed", data);
    assert_eq!(error.code, "");
    assert_eq!(error.message, data);
}

#[test]
fn parse_error_event_flat() {
    let error = parse_error(
        "error",
        r#"{"type":"error","code":"rate_limit_exceeded","message":"Slow down","param":null,"sequence_number":1}"#,
    );
    assert_eq!(error.code, "rate_limit_exceeded");
    assert_eq!(error.message, "Slow down");
}

#[test]
fn parse_error_event_flat_null_code() {
    let error = parse_error(
        "error",
        r#"{"type":"error","code":null,"message":"Something broke","param":null}"#,
    );
    assert_eq!(error.code, "");
    assert_eq!(error.message, "Something broke");
}

#[test]
fn parse_error_event_nested() {
    let error = parse_error(
        "error",
        r#"{"type":"error","error":{"type":"invalid_request_error","code":null,"message":"Bad input","param":"input"}}"#,
    );
    assert_eq!(error.code, "");
    assert_eq!(error.message, "Bad input");
}

#[test]
fn parse_error_event_unparseable_uses_raw_data() {
    let error = parse_error("error", "upstream exploded");
    assert_eq!(error.code, "");
    assert_eq!(error.message, "upstream exploded");
}

#[test]
fn parse_error_response_envelope_with_null_code() {
    let err = parse_error_response(
        br#"{"error":{"message":"Invalid model","type":"invalid_request_error","param":"model","code":null}}"#,
    );
    match err {
        LlmProviderError::ProviderError { code, message, .. } => {
            assert_eq!(code, "");
            assert_eq!(message, "Invalid model");
        }
        other => panic!("expected ProviderError, got {other:?}"),
    }
}

#[test]
fn parse_response_incomplete_event() {
    let event = ServerEvent {
        event: Some("response.incomplete".to_string()),
        data: r#"{"response":{"id":"resp-inc","output":[],"usage":{"input_tokens":200,"output_tokens":4096},"incomplete_details":{"reason":"max_output_tokens"}}}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    match result {
        ProviderEvent::ResponseIncomplete { response } => {
            assert_eq!(response.id, "resp-inc");
            assert_eq!(response.usage.input_tokens, 200);
            assert_eq!(response.usage.output_tokens, 4096);
            assert_eq!(
                response.incomplete_details.as_ref().unwrap().reason,
                "max_output_tokens"
            );
        }
        _ => panic!("expected ResponseIncomplete"),
    }
}

#[test]
fn parse_unknown_event_returns_unknown() {
    let event = ServerEvent {
        event: Some("response.new_feature.delta".to_string()),
        data: r#"{"something":"new"}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event).unwrap();
    assert!(
        matches!(result, ProviderEvent::Unknown { event_name } if event_name == "response.new_feature.delta")
    );
}

#[test]
fn parse_malformed_json_in_known_event_returns_error() {
    let event = ServerEvent {
        event: Some("response.output_text.delta".to_string()),
        data: "not valid json".to_string(),
        id: None,
        retry: None,
    };
    let result = ProviderEvent::from_server_event(event);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        StreamingError::ServerEventsParse { .. }
    ));
}

// ── Unit tests: translate_provider_event ───────────────────────────────

#[test]
fn translate_text_delta() {
    let event = ProviderEvent::ResponseOutputTextDelta { delta: "Hi".into() };
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Delta { r#type, content }) => {
            assert_eq!(r#type, "text");
            assert_eq!(content, "Hi");
        }
        _ => panic!("expected Sse(Delta)"),
    }
}

#[test]
fn translate_text_done_is_skip() {
    let event = ProviderEvent::ResponseOutputTextDone {
        text: "done".into(),
    };
    let translated = translate_provider_event(&event, "");
    assert!(matches!(translated, TranslatedEvent::Skip));
}

#[test]
fn translate_file_search_start() {
    let event = ProviderEvent::ResponseFileSearchCallSearching;
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool { phase, name, .. }) => {
            assert!(matches!(phase, ToolPhase::Start));
            assert_eq!(name, "file_search");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_file_search_done_with_count() {
    let event = ProviderEvent::ResponseFileSearchCallCompleted {
        results: vec![
            FileSearchResult {
                file_id: "f1".into(),
                filename: "a.pdf".into(),
                score: 0.9,
                text: String::new(),
            },
            FileSearchResult {
                file_id: "f2".into(),
                filename: "b.pdf".into(),
                score: 0.8,
                text: String::new(),
            },
        ],
    };
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool {
            phase,
            name,
            details,
        }) => {
            assert!(matches!(phase, ToolPhase::Done));
            assert_eq!(name, "file_search");
            assert_eq!(details["files_searched"], 2);
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_web_search_start() {
    let event = ProviderEvent::ResponseWebSearchCallSearching;
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool { phase, name, .. }) => {
            assert!(matches!(phase, ToolPhase::Start));
            assert_eq!(name, "web_search");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_web_search_done() {
    let event = ProviderEvent::ResponseWebSearchCallCompleted;
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool { phase, name, .. }) => {
            assert!(matches!(phase, ToolPhase::Done));
            assert_eq!(name, "web_search");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_code_interpreter_start() {
    let event = ProviderEvent::ResponseCodeInterpreterCallInProgress;
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool { phase, name, .. }) => {
            assert!(matches!(phase, ToolPhase::Start));
            assert_eq!(name, "code_interpreter");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_code_interpreter_done_with_output() {
    let event = ProviderEvent::ResponseCodeInterpreterCallCompleted {
        output: "42".into(),
    };
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool {
            phase,
            name,
            details,
        }) => {
            assert!(matches!(phase, ToolPhase::Done));
            assert_eq!(name, "code_interpreter");
            assert_eq!(details["output"], "42");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_completed_returns_terminal() {
    let event = ProviderEvent::ResponseCompleted {
        response: ResponseObject {
            id: "resp-abc".into(),
            output: vec![],
            usage: RawUsage {
                input_tokens: 500,
                output_tokens: 120,
                input_tokens_details: None,
                output_tokens_details: None,
            },
            incomplete_details: None,
        },
    };
    let translated = translate_provider_event(&event, "Hello");
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed {
            usage,
            response_id,
            content,
            ..
        }) => {
            assert_eq!(usage.input_tokens, 500);
            assert_eq!(usage.output_tokens, 120);
            assert_eq!(response_id, "resp-abc");
            assert_eq!(content, "Hello");
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

#[test]
fn translate_completed_propagates_token_details() {
    let event = ProviderEvent::ResponseCompleted {
        response: ResponseObject {
            id: "resp-xyz".into(),
            output: vec![],
            usage: RawUsage {
                input_tokens: 800,
                output_tokens: 200,
                input_tokens_details: Some(InputTokensDetails { cached_tokens: 300 }),
                output_tokens_details: Some(OutputTokensDetails {
                    reasoning_tokens: 60,
                }),
            },
            incomplete_details: None,
        },
    };
    let translated = translate_provider_event(&event, "");
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed { usage, .. }) => {
            assert_eq!(usage.cache_read_input_tokens, 300);
            assert_eq!(usage.reasoning_tokens, 60);
            assert_eq!(usage.cache_write_input_tokens, 0);
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

/// `response.failed` carries `response.usage` when the provider billed the
/// request; it reaches the terminal outcome, so settlement can use it.
#[test]
fn response_failed_usage_reaches_terminal_outcome() {
    let event = ServerEvent {
        event: Some("response.failed".to_string()),
        data: r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"boom"},"usage":{"input_tokens":12,"output_tokens":3}}}"#.to_string(),
        id: None,
        retry: None,
    };
    let parsed = ProviderEvent::from_server_event(event).unwrap();
    match translate_provider_event(&parsed, "") {
        TranslatedEvent::Terminal(TerminalOutcome::Failed { usage: Some(u), .. }) => {
            assert_eq!(u.input_tokens, 12);
            assert_eq!(u.output_tokens, 3);
        }
        _ => panic!("expected Terminal(Failed) with usage"),
    }

    let without = ServerEvent {
        event: Some("response.failed".to_string()),
        data: r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"},"usage":null}}"#.to_string(),
        id: None,
        retry: None,
    };
    let parsed = ProviderEvent::from_server_event(without).unwrap();
    assert!(matches!(
        translate_provider_event(&parsed, ""),
        TranslatedEvent::Terminal(TerminalOutcome::Failed { usage: None, .. })
    ));
}

#[test]
fn translate_failed_returns_terminal() {
    let event = ProviderEvent::ResponseFailed {
        error: ProviderErrorPayload {
            code: "err".into(),
            message: "failed".into(),
        },
        usage: None,
    };
    let translated = translate_provider_event(&event, "partial");
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Failed {
            partial_content, ..
        }) => {
            assert_eq!(partial_content, "partial");
        }
        _ => panic!("expected Terminal(Failed)"),
    }
}

#[test]
fn translate_incomplete_returns_terminal() {
    let event = ProviderEvent::ResponseIncomplete {
        response: ResponseObject {
            id: "resp-inc".into(),
            output: vec![],
            usage: RawUsage {
                input_tokens: 200,
                output_tokens: 4096,
                input_tokens_details: None,
                output_tokens_details: None,
            },
            incomplete_details: Some(IncompleteDetails {
                reason: "max_output_tokens".into(),
            }),
        },
    };
    let translated = translate_provider_event(&event, "partial");
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
            reason,
            usage,
            partial_content,
        }) => {
            assert_eq!(reason, "max_output_tokens");
            assert_eq!(partial_content, "partial");
            assert_eq!(usage.input_tokens, 200);
            assert_eq!(usage.output_tokens, 4096);
        }
        _ => panic!("expected Terminal(Incomplete)"),
    }
}

#[test]
fn translate_unknown_is_skip() {
    let event = ProviderEvent::Unknown {
        event_name: "response.new".into(),
    };
    let translated = translate_provider_event(&event, "");
    assert!(matches!(translated, TranslatedEvent::Skip));
}

#[test]
fn char_slice_counts_characters_and_rejects_bad_ranges() {
    assert_eq!(char_slice("h\u{e9}llo", 1, 3).as_deref(), Some("\u{e9}l"));
    assert_eq!(
        char_slice("h\u{e9}llo", 0, 5).as_deref(),
        Some("h\u{e9}llo")
    );
    // Empty or reversed range.
    assert_eq!(char_slice("h\u{e9}llo", 2, 2), None);
    assert_eq!(char_slice("h\u{e9}llo", 3, 1), None);
    // End past the text.
    assert_eq!(char_slice("h\u{e9}llo", 3, 6), None);
    assert_eq!(char_slice("h\u{e9}llo", 5, 6), None);
}

#[test]
fn extract_citations_file_citation() {
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: "Hello".into(),
                annotations: vec![Annotation {
                    r#type: "file_citation".into(),
                    title: String::new(),
                    url: None,
                    file_id: Some("file-xyz".into()),
                    filename: Some("Report.pdf".into()),
                    start_index: None,
                    end_index: None,
                    text: None,
                }],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, "Hello");
    assert_eq!(citations.len(), 1);
    assert!(matches!(citations[0].source, CitationSource::File));
    assert_eq!(citations[0].title, "Report.pdf");
    assert_eq!(citations[0].attachment_id.as_deref(), Some("file-xyz"));
    assert_eq!(citations[0].snippet, "");
    assert!(citations[0].span.is_none());
    assert!(citations[0].url.is_none());
}

#[test]
fn extract_citations_file_citation_from_real_json() {
    let response: ResponseObject = serde_json::from_value(serde_json::json!({
        "id": "resp-1",
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "Revenue grew.",
                "annotations": [
                    {"type": "file_citation", "file_id": "file-1", "filename": "q3.pdf", "index": 13},
                    {"type": "url_citation", "url": "https://example.com", "title": "Example",
                     "start_index": 0, "end_index": 7}
                ]
            }]
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    }))
    .unwrap();
    let citations = extract_citations(&response, "Revenue grew.");
    assert_eq!(citations.len(), 2);
    assert!(matches!(citations[0].source, CitationSource::File));
    assert_eq!(citations[0].title, "q3.pdf");
    assert_eq!(citations[0].attachment_id.as_deref(), Some("file-1"));
    assert_eq!(citations[0].snippet, "");
    assert!(citations[0].span.is_none());
    assert!(matches!(citations[1].source, CitationSource::Web));
    assert_eq!(citations[1].snippet, "Revenue");
    let span = citations[1].span.unwrap();
    assert_eq!((span.start, span.end), (0, 7));
}

#[test]
fn extract_citations_file_citation_keeps_text_and_range_when_sent() {
    let response: ResponseObject = serde_json::from_value(serde_json::json!({
        "id": "resp-1",
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "Based on docs",
                "annotations": [{"type": "file_citation", "file_id": "file-1", "title": "doc.pdf",
                                 "start_index": 0, "end_index": 5, "text": "Based"}]
            }]
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    }))
    .unwrap();
    let citations = extract_citations(&response, "Based on docs");
    assert_eq!(citations[0].title, "doc.pdf");
    assert_eq!(citations[0].snippet, "Based");
    let span = citations[0].span.unwrap();
    assert_eq!((span.start, span.end), (0, 5));
}

#[test]
fn extract_citations_url_citation() {
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: "Hello".into(),
                annotations: vec![Annotation {
                    r#type: "url_citation".into(),
                    title: "Example".into(),
                    url: Some("https://example.com".into()),
                    file_id: None,
                    filename: None,
                    start_index: None,
                    end_index: None,
                    text: None,
                }],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, "");
    assert_eq!(citations.len(), 1);
    assert!(matches!(citations[0].source, CitationSource::Web));
    assert_eq!(citations[0].url.as_deref(), Some("https://example.com"));
    assert_eq!(citations[0].title, "Example");
}

#[test]
fn extract_citations_empty_annotations() {
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: "Hello".into(),
                annotations: vec![],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, "");
    assert!(citations.is_empty());
}

// Indices are character offsets into the annotated part, not byte offsets
// into the whole answer: a second message part with non-ASCII text.
#[test]
fn extract_citations_range_uses_characters_of_its_own_part() {
    let first = "Intro \u{e9}t\u{e9}. ";
    let second = "Caf\u{e9} cr\u{e8}me is sold here.";
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![
            OutputItem {
                r#type: "message".into(),
                content: vec![ResponseContentPart {
                    r#type: "output_text".into(),
                    text: first.into(),
                    annotations: vec![],
                }],
                ..Default::default()
            },
            OutputItem {
                r#type: "message".into(),
                content: vec![ResponseContentPart {
                    r#type: "output_text".into(),
                    text: second.into(),
                    annotations: vec![Annotation {
                        r#type: "url_citation".into(),
                        title: "Wiki".into(),
                        url: Some("https://example.org".into()),
                        file_id: None,
                        filename: None,
                        start_index: Some(0),
                        end_index: Some(10),
                        text: None,
                    }],
                }],
                ..Default::default()
            },
        ],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, &format!("{first}{second}"));
    assert_eq!(citations[0].snippet, "Caf\u{e9} cr\u{e8}me");
}

#[test]
fn extract_citations_url_citation_snippet_from_text_range() {
    let accumulated = "0123456789The capital of France is Paris.";
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: accumulated.into(),
                annotations: vec![Annotation {
                    r#type: "url_citation".into(),
                    title: "Wikipedia".into(),
                    url: Some("https://en.wikipedia.org/wiki/France".into()),
                    file_id: None,
                    filename: None,
                    start_index: Some(10),
                    end_index: Some(31),
                    text: None,
                }],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, accumulated);
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].snippet, "The capital of France");
}

#[test]
fn extract_citations_url_citation_snippet_from_annotation_text() {
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: "Hello world".into(),
                annotations: vec![Annotation {
                    r#type: "url_citation".into(),
                    title: "Example".into(),
                    url: Some("https://example.com".into()),
                    file_id: None,
                    filename: None,
                    start_index: Some(0),
                    end_index: Some(5),
                    text: Some("explicit snippet".into()),
                }],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, "Hello world");
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].snippet, "explicit snippet");
}

#[test]
fn extract_citations_url_citation_no_text_no_indices() {
    let response = ResponseObject {
        id: "resp-1".into(),
        output: vec![OutputItem {
            r#type: "message".into(),
            content: vec![ResponseContentPart {
                r#type: "output_text".into(),
                text: "Hello".into(),
                annotations: vec![Annotation {
                    r#type: "url_citation".into(),
                    title: "Example".into(),
                    url: Some("https://example.com".into()),
                    file_id: None,
                    filename: None,
                    start_index: None,
                    end_index: None,
                    text: None,
                }],
            }],
            ..Default::default()
        }],
        usage: RawUsage {
            input_tokens: 0,
            output_tokens: 0,
            input_tokens_details: None,
            output_tokens_details: None,
        },
        incomplete_details: None,
    };
    let citations = extract_citations(&response, "Hello");
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].snippet, "");
}

// ── Integration tests: streaming ───────────────────────────────────────

#[tokio::test]
async fn stream_yields_events_and_outcome() {
    let events = vec![
        sse_event("response.output_text.delta", r#"{"delta":"Hel"}"#),
        sse_event("response.output_text.delta", r#"{"delta":"lo "}"#),
        sse_event("response.output_text.delta", r#"{"delta":"world"}"#),
        sse_event(
            "response.completed",
            r#"{"response":{"id":"resp-1","output":[],"usage":{"input_tokens":100,"output_tokens":30}}}"#,
        ),
    ];

    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw.clone());

    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hello"))
        .build_streaming();

    let cancel = CancellationToken::new();
    let stream = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await
        .unwrap();
    let outcome = stream.into_outcome().await;

    match outcome {
        TerminalOutcome::Completed {
            content,
            usage,
            response_id,
            ..
        } => {
            assert_eq!(content, "Hello world");
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 30);
            assert_eq!(response_id, "resp-1");
        }
        _ => panic!("expected Completed, got {outcome:?}"),
    }

    assert_eq!(gw.last_request_uri().unwrap(), "/openai");
}

#[tokio::test]
async fn stream_interleaved_tool_events() {
    let events = vec![
        sse_event("response.output_text.delta", r#"{"delta":"A"}"#),
        sse_event("response.file_search_call.searching", "{}"),
        sse_event("response.output_text.delta", r#"{"delta":"B"}"#),
        sse_event("response.file_search_call.completed", r#"{"results":[]}"#),
        sse_event("response.output_text.delta", r#"{"delta":"C"}"#),
        sse_event(
            "response.completed",
            r#"{"response":{"id":"resp-2","output":[],"usage":{"input_tokens":50,"output_tokens":10}}}"#,
        ),
    ];

    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("gpt-4o").build_streaming();
    let cancel = CancellationToken::new();
    let stream = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await
        .unwrap();
    let outcome = stream.into_outcome().await;

    match outcome {
        TerminalOutcome::Completed { content, .. } => {
            assert_eq!(content, "ABC");
        }
        _ => panic!("expected Completed"),
    }
}

#[tokio::test]
async fn stream_code_interpreter_start_then_done_with_output() {
    let events = vec![
        sse_event(
            "response.output_item.added",
            r#"{"output_index":0,"item":{"type":"code_interpreter_call","id":"ci_1","status":"in_progress","outputs":null}}"#,
        ),
        sse_event(
            "response.code_interpreter_call.in_progress",
            r#"{"item_id":"ci_1","output_index":0,"sequence_number":1}"#,
        ),
        sse_event(
            "response.code_interpreter_call.interpreting",
            r#"{"item_id":"ci_1","output_index":0,"sequence_number":2}"#,
        ),
        sse_event(
            "response.code_interpreter_call.completed",
            r#"{"item_id":"ci_1","output_index":0,"sequence_number":3}"#,
        ),
        sse_event(
            "response.output_item.done",
            r#"{"output_index":0,"sequence_number":4,"item":{"type":"code_interpreter_call","id":"ci_1","status":"completed","outputs":[{"type":"logs","logs":"Total: 42"}]}}"#,
        ),
        sse_event("response.output_text.delta", r#"{"delta":"42"}"#),
        sse_event(
            "response.completed",
            r#"{"response":{"id":"resp-ci","output":[],"usage":{"input_tokens":5,"output_tokens":1}}}"#,
        ),
    ];
    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw);
    let request = llm_request("gpt-4o").build_streaming();
    let stream = provider
        .stream(
            test_security_context(),
            request,
            "openai",
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let sse: Vec<ClientSseEvent> = stream.map(Result::unwrap).collect().await;
    let tools: Vec<(ToolPhase, serde_json::Value)> = sse
        .iter()
        .filter_map(|e| match e {
            ClientSseEvent::Tool {
                phase,
                name: "code_interpreter",
                details,
            } => Some((*phase, details.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            (ToolPhase::Start, serde_json::json!({})),
            (ToolPhase::Done, serde_json::json!({"output": "Total: 42"})),
        ]
    );
}

#[tokio::test]
async fn stream_response_failed_real_shape_keeps_message() {
    let events = vec![
        sse_event("response.output_text.delta", r#"{"delta":"Par"}"#),
        sse_event(
            "response.failed",
            r#"{"type":"response.failed","response":{"id":"resp-f","status":"failed","error":{"code":"server_error","message":"The model failed"},"output":[],"usage":null}}"#,
        ),
    ];
    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw);
    let request = llm_request("gpt-4o").build_streaming();
    let stream = provider
        .stream(
            test_security_context(),
            request,
            "openai",
            CancellationToken::new(),
        )
        .await
        .unwrap();

    match stream.into_outcome().await {
        TerminalOutcome::Failed {
            error: LlmProviderError::ProviderError { code, message, .. },
            partial_content,
            ..
        } => {
            assert_eq!(code, "server_error");
            assert_eq!(message, "The model failed");
            assert_eq!(partial_content, "Par");
        }
        other => panic!("expected Failed(ProviderError), got {other:?}"),
    }
}

// ── Integration test: cancellation ─────────────────────────────────────

#[tokio::test]
async fn cancellation_terminates_stream() {
    let events = vec![
        sse_event("response.output_text.delta", r#"{"delta":"Hello"}"#),
        sse_event("response.output_text.delta", r#"{"delta":" world"}"#),
        sse_event(
            "response.completed",
            r#"{"response":{"id":"resp-3","output":[],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ),
    ];

    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("gpt-4o").build_streaming();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(test_security_context(), request, "openai", cancel.clone())
        .await
        .unwrap();

    // Read first event
    let first = stream.next().await;
    assert!(first.is_some());

    // Cancel
    cancel.cancel();
    assert!(stream.is_cancelled());

    // Stream should terminate
    let _remaining: Vec<_> = stream.collect().await;
}

// ── Integration test: OAGW error paths ─────────────────────────────────

#[resource_error(gts_id!("cf.core.oagw.proxy.v1~"))]
struct TestProxyScope;

#[tokio::test]
async fn oagw_rate_limit_error() {
    // End-to-end: canonical `ResourceExhausted` with a per-violation retry
    // hint → `ServiceGatewayError::RateLimited` → `LlmProviderError::RateLimited`.
    // The retry value must survive every hop unchanged.
    let gw = MockGateway::returning_error(
        TestProxyScope::resource_exhausted("rate limit exceeded")
            .with_quota_violation(oagw_sdk::quota::RATE_LIMIT, "rate limit exceeded")
            .with_quota_violation_retry_after_seconds(15)
            .create(),
    );
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("gpt-4o").build_streaming();
    let cancel = CancellationToken::new();
    let result = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await;

    match result.unwrap_err() {
        LlmProviderError::RateLimited { retry_after_secs } => {
            assert_eq!(retry_after_secs, Some(15));
        }
        other => panic!("expected RateLimited with retry=15, got {other:?}"),
    }
}

#[tokio::test]
async fn oagw_deadline_exceeded_error() {
    let gw = MockGateway::returning_error(TestProxyScope::deadline_exceeded("timed out").create());
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("gpt-4o").build_streaming();
    let cancel = CancellationToken::new();
    let result = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await;

    assert!(matches!(result.unwrap_err(), LlmProviderError::Timeout));
}

#[tokio::test]
async fn oagw_unavailable_error() {
    let gw = MockGateway::returning_error(
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(30)
            .create(),
    );
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("gpt-4o").build_streaming();
    let cancel = CancellationToken::new();
    let result = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await;

    assert!(matches!(
        result.unwrap_err(),
        LlmProviderError::ProviderUnavailable
    ));
}

// ── Integration test: non-SSE response ─────────────────────────────────

#[tokio::test]
async fn non_sse_json_error_response() {
    let gw = MockGateway::returning_json(serde_json::json!({
        "code": "invalid_request",
        "message": "Error in resp_xyz123: invalid model at https://api.openai.com/v1"
    }));
    let provider = OpenAiResponsesProvider::new(gw);

    let request = llm_request("bad-model").build_streaming();
    let cancel = CancellationToken::new();
    let result = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await;

    match result.unwrap_err() {
        LlmProviderError::ProviderError {
            code,
            message,
            raw_detail,
        } => {
            assert_eq!(code, "invalid_request");
            assert!(!message.contains("resp_xyz123"));
            assert!(!message.contains("https://api.openai.com"));
            assert!(raw_detail.is_some());
        }
        other => panic!("expected ProviderError, got {other:?}"),
    }
}

// ── Integration test: complete ────────────────────────────────────

#[tokio::test]
async fn complete_response_success() {
    let gw = MockGateway::returning_json(serde_json::json!({
        "id": "resp-complete-1",
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "Summary of the conversation.",
                "annotations": [{
                    "type": "file_citation",
                    "title": "Doc.pdf",
                    "file_id": "file-1",
                    "start_index": 0,
                    "end_index": 10,
                    "text": "snippet"
                }]
            }]
        }],
        "usage": {
            "input_tokens": 500,
            "output_tokens": 50
        }
    }));
    let provider = OpenAiResponsesProvider::new(gw.clone());

    let request = llm_request("gpt-4o")
        .system_instructions("Summarize.")
        .message(LlmMessage::user("conversation"))
        .max_output_tokens(1024)
        .build_non_streaming();

    let result = provider
        .complete(test_security_context(), request, "azure-openai")
        .await
        .unwrap();

    assert_eq!(result.content, "Summary of the conversation.");
    assert_eq!(result.usage.input_tokens, 500);
    assert_eq!(result.usage.output_tokens, 50);
    assert_eq!(result.response_id, "resp-complete-1");
    assert_eq!(result.citations.len(), 1);
    assert!(matches!(result.citations[0].source, CitationSource::File));

    assert_eq!(gw.last_request_uri().unwrap(), "/azure-openai");
}

// ── Integration test: fluent builder ───────────────────────────────────

#[tokio::test]
async fn fluent_builder_produces_valid_json() {
    let events = vec![sse_event(
        "response.completed",
        r#"{"response":{"id":"resp-fb","output":[],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    )];

    let gw = MockGateway::returning_sse(events);
    let provider = OpenAiResponsesProvider::new(gw.clone());

    let request = llm_request("gpt-4o")
        .system_instructions("You are helpful")
        .message(LlmMessage::user("Hello"))
        .max_output_tokens(4096)
        .tools(vec![
            LlmTool::FileSearch {
                vector_store_ids: vec!["vs-123".into()],
                filters: None,
                max_num_results: None,
            },
            LlmTool::WebSearch {
                search_context_size: WebSearchContextSize::Low,
            },
        ])
        .user_identity("t1", "u1")
        .metadata(RequestMetadata {
            tenant_id: "t1".into(),
            user_id: "u1".into(),
            chat_id: "c1".into(),
            request_type: RequestType::Chat,
            features: vec![FeatureFlag::FileSearch, FeatureFlag::WebSearch],
        })
        .build_streaming();

    let cancel = CancellationToken::new();
    let _stream = provider
        .stream(test_security_context(), request, "openai", cancel)
        .await
        .unwrap();

    let body_str = gw.last_request_body().unwrap();
    let body: serde_json::Value = serde_json::from_str(&body_str).unwrap();

    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["instructions"], "You are helpful");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["user"], "t1:u1");
    assert!(body["previous_response_id"].is_null());
    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "Hello");
    assert_eq!(body["tools"][0]["type"], "file_search");
    assert_eq!(body["tools"][0]["vector_store_ids"][0], "vs-123");
    assert_eq!(body["tools"][1]["type"], "web_search");
    assert_eq!(body["metadata"]["tenant_id"], "t1");
    assert_eq!(body["metadata"]["feature"], "file_search+web_search");
}

// ── P5-K4: file_search wire format (Responses API = flat) ──

#[test]
fn file_search_wire_format_flat() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("test"))
        .tools(vec![LlmTool::FileSearch {
            vector_store_ids: vec!["vs-001".into(), "vs-002".into()],
            filters: None,
            max_num_results: None,
        }])
        .build_streaming();

    let body = build_request_body(&request, true);

    // Responses API: flat format — vector_store_ids at top level of tool object
    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "file_search");
    assert_eq!(tool["vector_store_ids"][0], "vs-001");
    assert_eq!(tool["vector_store_ids"][1], "vs-002");
}

// ── P5-K5: file_search wire format with filters ──

#[test]
fn file_search_wire_format_with_filters() {
    let filter = FileSearchFilter::In {
        key: "attachment_id".to_owned(),
        values: vec!["uuid-a".to_owned(), "uuid-b".to_owned()],
    };

    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("test"))
        .tools(vec![LlmTool::FileSearch {
            vector_store_ids: vec!["vs-001".into()],
            filters: Some(filter),
            max_num_results: None,
        }])
        .build_streaming();

    let body = build_request_body(&request, true);
    let tool = &body["tools"][0];

    assert_eq!(tool["type"], "file_search");
    // Responses API: flat format — everything at top level
    assert_eq!(tool["vector_store_ids"][0], "vs-001");
    assert_eq!(tool["filters"]["type"], "in");
    assert_eq!(tool["filters"]["key"], "attachment_id");
    assert_eq!(tool["filters"]["values"][0], "uuid-a");
    assert_eq!(tool["filters"]["values"][1], "uuid-b");
}

/// `extra_body` cannot lift the quota-derived caps or change the model; other
/// keys are merged.
#[test]
fn extra_body_does_not_override_reserved_keys() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hello"))
        .max_output_tokens(256)
        .max_tool_calls(2)
        .api_params(mini_chat_sdk::ModelApiParams {
            extra_body: Some(serde_json::json!({
                "max_output_tokens": 100_000,
                "max_tool_calls": 50,
                "model": "other-model",
                "user": "someone-else",
                "store": true,
                "include": [],
                "seed": 7
            })),
            ..test_api_params()
        })
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["max_output_tokens"], 256);
    assert_eq!(body["max_tool_calls"], 2);
    assert_eq!(body["model"], "gpt-4o");
    assert!(body.get("user").is_none(), "{body}");
    assert_eq!(body["store"], false);
    assert!(body.get("include").is_none(), "{body}");
    assert_eq!(body["seed"], 7);
}
