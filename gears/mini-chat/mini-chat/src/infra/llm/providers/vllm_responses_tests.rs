// Created: 2026-04-07 by Constructor Tech
#![allow(clippy::non_ascii_literal, clippy::str_to_string)]
use super::*;
use crate::infra::llm::{LlmMessage, llm_request};

// ── ThinkState unit tests ────────────────────────────────────────────

#[test]
fn think_tags_in_single_delta() {
    let mut state = ThinkState::new();
    let chunks = state.feed("<think>reasoning here</think>actual text");
    let types: Vec<_> = chunks
        .iter()
        .map(|c| (c.delta_type, c.text.as_str()))
        .collect();
    assert_eq!(
        types,
        vec![("reasoning", "reasoning here"), ("text", "actual text")]
    );
}

#[test]
fn think_tags_split_across_deltas() {
    let mut state = ThinkState::new();

    let c1 = state.feed("<think>start of thought");
    assert_eq!(c1.len(), 1);
    assert_eq!(c1[0].delta_type, "reasoning");
    assert_eq!(c1[0].text, "start of thought");

    let c2 = state.feed(" continued</think>visible");
    let types: Vec<_> = c2.iter().map(|c| (c.delta_type, c.text.as_str())).collect();
    assert_eq!(
        types,
        vec![("reasoning", " continued"), ("text", "visible")]
    );
}

#[test]
fn partial_tag_across_deltas() {
    let mut state = ThinkState::new();

    // Delta ends mid-tag: "<thi"
    let c1 = state.feed("<thi");
    assert!(c1.is_empty(), "partial tag should be buffered");

    // Next delta completes the tag
    let c2 = state.feed("nk>inside");
    assert_eq!(c2.len(), 1);
    assert_eq!(c2[0].delta_type, "reasoning");
    assert_eq!(c2[0].text, "inside");
}

#[test]
fn no_think_tags_passes_through() {
    let mut state = ThinkState::new();
    let chunks = state.feed("just normal text");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].delta_type, "text");
    assert_eq!(chunks[0].text, "just normal text");
}

#[test]
fn angle_bracket_not_a_tag() {
    let mut state = ThinkState::new();
    let chunks = state.feed("5 < 10 and 10 > 5");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].delta_type, "text");
    assert_eq!(chunks[0].text, "5 < 10 and 10 > 5");
}

#[test]
fn empty_think_block() {
    let mut state = ThinkState::new();
    let chunks = state.feed("<think></think>answer");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].delta_type, "text");
    assert_eq!(chunks[0].text, "answer");
}

#[test]
fn flush_emits_pending() {
    let mut state = ThinkState::new();
    let c1 = state.feed("<thi");
    assert!(c1.is_empty());

    let flushed = state.flush();
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].text, "<thi");
    assert_eq!(flushed[0].delta_type, "text");
}

#[test]
fn newlines_after_think_tag_stripped() {
    let mut state = ThinkState::new();
    let chunks = state.feed("<think>\nreasoning\n</think>\ntext");
    let types: Vec<_> = chunks
        .iter()
        .map(|c| (c.delta_type, c.text.as_str()))
        .collect();
    assert_eq!(
        types,
        vec![("reasoning", "\nreasoning\n"), ("text", "\ntext")]
    );
}

#[test]
fn cyrillic_text_preserved() {
    let mut state = ThinkState::new();
    let chunks = state.feed("<think>Нека помислим</think>Здравей свят!");
    let types: Vec<_> = chunks
        .iter()
        .map(|c| (c.delta_type, c.text.as_str()))
        .collect();
    assert_eq!(
        types,
        vec![("reasoning", "Нека помислим"), ("text", "Здравей свят!"),]
    );
}

#[test]
fn multibyte_chars_not_corrupted() {
    let mut state = ThinkState::new();
    // Emoji, CJK, Bulgarian in a single delta
    let chunks = state.feed("🦀 Rust は素晴らしい и прекрасен");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "🦀 Rust は素晴らしい и прекрасен");
}

// ── strip_think_tags ─────────────────────────────────────────────────

#[test]
fn strip_think_basic() {
    assert_eq!(strip_think_tags("<think>reasoning</think>answer"), "answer");
}

#[test]
fn strip_think_no_tags() {
    assert_eq!(strip_think_tags("plain text"), "plain text");
}

#[test]
fn strip_think_unclosed() {
    assert_eq!(strip_think_tags("before<think>reasoning"), "before");
}

#[test]
fn strip_think_multiple() {
    assert_eq!(strip_think_tags("<think>a</think>b<think>c</think>d"), "bd");
}

// ── build_request_body ───────────────────────────────────────────────

#[test]
fn assistant_content_is_plain_string() {
    let request = llm_request("test-model")
        .message(LlmMessage::user("Hello"))
        .message(LlmMessage::assistant("Hi there!"))
        .message(LlmMessage::user("How are you?"))
        .build_streaming();

    let body = build_request_body(&request, true);
    let input = body["input"].as_array().unwrap();

    assert_eq!(input[0]["role"], "user");
    assert!(input[0]["content"].is_array());

    assert_eq!(input[1]["role"], "assistant");
    assert!(input[1]["content"].is_string());
    assert_eq!(input[1]["content"], "Hi there!");

    assert_eq!(input[2]["role"], "user");
    assert!(input[2]["content"].is_array());
}

#[test]
fn provider_hosted_tools_are_omitted() {
    use crate::domain::llm::{LlmTool, WebSearchContextSize};

    // vLLM doesn't host file_search/web_search/code_interpreter — these must
    // be dropped so the upstream doesn't reject unknown tool types.
    let request = llm_request("test-model")
        .message(LlmMessage::user("Search"))
        .tool(LlmTool::WebSearch {
            search_context_size: WebSearchContextSize::Medium,
        })
        .tool(LlmTool::FileSearch {
            vector_store_ids: vec!["vs-1".into()],
            filters: None,
            max_num_results: None,
        })
        .build_streaming();

    let body = build_request_body(&request, true);
    // No function tools present → `tools` omitted entirely.
    assert!(body.get("tools").is_none());
}

#[test]
fn function_tools_are_serialized() {
    use crate::domain::llm::LlmTool;

    // Regression guard: function tools (e.g. `exa_search`) MUST reach the
    // model, otherwise it can never call them.
    let request = llm_request("test-model")
        .message(LlmMessage::user("weather in Istanbul"))
        .tool(LlmTool::Function {
            name: "exa_search".into(),
            description: "Search the web.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        })
        // Provider-hosted tool alongside — must be filtered out.
        .tool(LlmTool::WebSearch {
            search_context_size: crate::domain::llm::WebSearchContextSize::Low,
        })
        .build_streaming();

    let body = build_request_body(&request, true);
    let tools = body["tools"].as_array().expect("tools array present");
    assert_eq!(tools.len(), 1, "only the function tool should be serialized");
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "exa_search");
    assert_eq!(tools[0]["description"], "Search the web.");
    assert_eq!(tools[0]["parameters"]["required"][0], "query");
}

#[test]
fn raw_input_items_are_appended_to_input() {
    // Agentic-loop replay: function_call / function_call_output items must be
    // forwarded so the model sees its prior tool call and the result.
    let request = llm_request("test-model")
        .message(LlmMessage::user("weather in Istanbul"))
        .raw_input_items(vec![
            serde_json::json!({
                "type": "function_call",
                "name": "exa_search",
                "arguments": "{\"query\":\"weather Istanbul\"}",
                "call_id": "call_1"
            }),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "Sunny, 28°C"
            }),
        ])
        .build_streaming();

    let body = build_request_body(&request, true);
    let input = body["input"].as_array().unwrap();
    // 1 user message + 2 replay items.
    assert_eq!(input.len(), 3);
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["name"], "exa_search");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["output"], "Sunny, 28°C");
}

#[test]
fn metadata_and_max_tool_calls_omitted_even_when_set() {
    use crate::infra::llm::request::{RequestMetadata, RequestType};

    let request = llm_request("test-model")
        .message(LlmMessage::user("Hello"))
        .metadata(RequestMetadata {
            tenant_id: "t1".into(),
            user_id: "u1".into(),
            chat_id: "c1".into(),
            request_type: RequestType::Chat,
            features: vec![],
        })
        .max_tool_calls(5)
        .build_streaming();

    let body = build_request_body(&request, true);
    assert!(body.get("metadata").is_none());
    assert!(body.get("max_tool_calls").is_none());
    assert!(body.get("previous_response_id").is_none());
}

#[test]
fn system_messages_become_instructions() {
    let request = llm_request("test-model")
        .system_instructions("Be helpful")
        .message(LlmMessage::user("Hello"))
        .build_streaming();

    let body = build_request_body(&request, true);
    assert_eq!(body["instructions"], "Be helpful");

    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
}

#[test]
fn additional_params_are_merged() {
    let request = llm_request("test-model")
        .message(LlmMessage::user("Hello"))
        .additional_params(serde_json::json!({
            "temperature": 0.5,
            "top_p": 0.9
        }))
        .build_streaming();

    let body = build_request_body(&request, true);
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["top_p"], 0.9);
}

// ── VllmProviderEvent: event type carried in JSON `type` ──────────────
// vLLM omits the SSE `event:` line and puts the type only inside the JSON
// payload. The wrapper must recover it so events are recognised; otherwise
// the stream ends without a terminal event.

#[test]
fn vllm_event_resolves_text_delta_from_payload_type() {
    let event = ServerEvent {
        event: None,
        data: r#"{"type":"response.output_text.delta","delta":"Hello","item_id":"msg_1","sequence_number":4}"#.to_string(),
        id: None,
        retry: None,
    };
    let VllmProviderEvent::Shared(result) = VllmProviderEvent::from_server_event(event).unwrap()
    else {
        panic!("expected a shared ProviderEvent");
    };
    assert!(
        matches!(result, ProviderEvent::ResponseOutputTextDelta { delta } if delta == "Hello"),
        "payload `type` should be honoured when the SSE `event:` field is absent"
    );
}

#[test]
fn vllm_event_resolves_completed_from_payload_type() {
    let event = ServerEvent {
        event: None,
        data: r#"{"type":"response.completed","sequence_number":10,"response":{"id":"resp_1","output":[],"usage":{"input_tokens":1,"output_tokens":1}}}"#.to_string(),
        id: None,
        retry: None,
    };
    let VllmProviderEvent::Shared(result) = VllmProviderEvent::from_server_event(event).unwrap()
    else {
        panic!("expected a shared ProviderEvent");
    };
    assert!(
        matches!(result, ProviderEvent::ResponseCompleted { response } if response.id == "resp_1"),
        "terminal event must be recognised from the JSON `type` field"
    );
}

#[test]
fn vllm_event_prefers_sse_event_field_when_present() {
    // If the upstream does send `event:` (OpenAI shape), it wins.
    let event = ServerEvent {
        event: Some("response.output_text.delta".to_string()),
        data: r#"{"type":"response.completed","delta":"Hi"}"#.to_string(),
        id: None,
        retry: None,
    };
    let VllmProviderEvent::Shared(result) = VllmProviderEvent::from_server_event(event).unwrap()
    else {
        panic!("expected a shared ProviderEvent");
    };
    assert!(matches!(result, ProviderEvent::ResponseOutputTextDelta { delta } if delta == "Hi"));
}

#[test]
fn vllm_event_captures_reasoning_delta() {
    // gpt-oss streams chain-of-thought as `response.reasoning_text.delta`.
    let event = ServerEvent {
        event: None,
        data: r#"{"type":"response.reasoning_text.delta","content_index":0,"delta":"thinking…","item_id":"msg_1"}"#.to_string(),
        id: None,
        retry: None,
    };
    let result = VllmProviderEvent::from_server_event(event).unwrap();
    assert!(
        matches!(result, VllmProviderEvent::ReasoningDelta { delta } if delta == "thinking…"),
        "reasoning deltas must be captured as a dedicated variant"
    );
}

#[test]
fn vllm_event_unmodelled_lifecycle_resolves_to_unknown() {
    // A lifecycle event vLLM emits but the adapter doesn't model must parse
    // cleanly as Unknown, not error the stream.
    let event = ServerEvent {
        event: None,
        data: r#"{"type":"response.reasoning_part.added","content_index":0}"#.to_string(),
        id: None,
        retry: None,
    };
    let VllmProviderEvent::Shared(result) = VllmProviderEvent::from_server_event(event).unwrap()
    else {
        panic!("expected a shared ProviderEvent");
    };
    assert!(matches!(result, ProviderEvent::Unknown { .. }));
}
