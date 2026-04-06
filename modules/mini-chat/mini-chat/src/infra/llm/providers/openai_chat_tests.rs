use super::*;
use crate::infra::llm::{LlmMessage, llm_request};
use oagw_sdk::sse::ServerEvent;

// ── FromServerEvent tests ─────────────────────────────────────────────

#[test]
fn parse_text_delta() {
    let event = ServerEvent {
        event: None,
        data: r#"{"choices":[{"delta":{"content":"Hello"}}]}"#.into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ChatCompletionEvent::Delta { content, .. } if content == "Hello"));
}

#[test]
fn parse_done_with_usage() {
    let event = ServerEvent {
            event: None,
            data: r#"{"usage":{"prompt_tokens":500,"completion_tokens":120},"choices":[{"finish_reason":"stop"}]}"#.into(),
            id: None,
            retry: None,
        };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    match result {
        ChatCompletionEvent::Done {
            usage,
            finish_reason,
        } => {
            assert_eq!(usage.prompt_tokens, 500);
            assert_eq!(usage.completion_tokens, 120);
            assert_eq!(finish_reason, "stop");
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn parse_done_with_token_details() {
    let event = ServerEvent {
            event: None,
            data: r#"{"usage":{"prompt_tokens":500,"completion_tokens":120,"prompt_tokens_details":{"cached_tokens":200},"completion_tokens_details":{"reasoning_tokens":40}},"choices":[{"finish_reason":"stop"}]}"#.into(),
            id: None,
            retry: None,
        };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    match result {
        ChatCompletionEvent::Done { usage, .. } => {
            assert_eq!(
                usage.prompt_tokens_details.as_ref().unwrap().cached_tokens,
                200
            );
            assert_eq!(
                usage
                    .completion_tokens_details
                    .as_ref()
                    .unwrap()
                    .reasoning_tokens,
                40
            );
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn parse_finish_reason_without_usage() {
    let event = ServerEvent {
        event: None,
        data: r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    assert!(matches!(
        result,
        ChatCompletionEvent::FinishReason { finish_reason } if finish_reason == "stop"
    ));
}

#[test]
fn parse_usage_only_chunk() {
    let event = ServerEvent {
        event: None,
        data: r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50}}"#.into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    match result {
        ChatCompletionEvent::Usage { usage } => {
            assert_eq!(usage.prompt_tokens, 100);
            assert_eq!(usage.completion_tokens, 50);
        }
        _ => panic!("expected Usage"),
    }
}

#[test]
fn parse_done_sentinel() {
    let event = ServerEvent {
        event: None,
        data: "[DONE]".into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ChatCompletionEvent::StreamEnd));
}

#[test]
fn parse_malformed_json_returns_error() {
    let event = ServerEvent {
        event: None,
        data: "not json at all".into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event);
    assert!(matches!(
        result.unwrap_err(),
        StreamingError::ServerEventsParse { .. }
    ));
}

#[test]
fn parse_empty_delta_is_unknown() {
    let event = ServerEvent {
        event: None,
        data: r#"{"choices":[{"delta":{}}]}"#.into(),
        id: None,
        retry: None,
    };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    assert!(matches!(result, ChatCompletionEvent::Unknown));
}

// ── Translation tests ─────────────────────────────────────────────────

/// Helper: unwrap a single-event translation result.
fn translate_one(event: &ChatCompletionEvent, state: &mut ChatCompletionsState) -> TranslatedEvent {
    let mut events = translate_chat_event(event, state);
    assert_eq!(events.len(), 1, "expected 1 event, got {}", events.len());
    events.remove(0)
}

#[test]
fn translate_delta_to_sse() {
    let event = ChatCompletionEvent::Delta {
        content: "Hi".into(),
        chunk_id: Some("chatcmpl-abc".into()),
    };
    let mut state = ChatCompletionsState::new();
    let translated = translate_one(&event, &mut state);
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Delta { r#type, content }) => {
            assert_eq!(r#type, "text");
            assert_eq!(content, "Hi");
        }
        _ => panic!("expected Sse(Delta)"),
    }
}

#[test]
fn translate_delta_captures_response_id() {
    let mut state = ChatCompletionsState::new();
    let delta = ChatCompletionEvent::Delta {
        content: "Hi".into(),
        chunk_id: Some("chatcmpl-abc123".into()),
    };
    translate_one(&delta, &mut state);
    assert_eq!(state.response_id, "chatcmpl-abc123");

    // Terminal should carry the response_id.
    let done = ChatCompletionEvent::Done {
        usage: ChatUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
        finish_reason: "stop".into(),
    };
    let translated = translate_one(&done, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed { response_id, .. }) => {
            assert_eq!(response_id, "chatcmpl-abc123");
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

#[test]
fn translate_done_stop_to_completed() {
    let event = ChatCompletionEvent::Done {
        usage: ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 120,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
        finish_reason: "stop".into(),
    };
    let mut state = ChatCompletionsState::new();
    state.accumulated_text = "Hello world".into();
    let translated = translate_one(&event, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed { usage, content, .. }) => {
            assert_eq!(usage.input_tokens, 500);
            assert_eq!(usage.output_tokens, 120);
            assert_eq!(content, "Hello world");
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

#[test]
fn translate_done_propagates_token_details() {
    let event = ChatCompletionEvent::Done {
        usage: ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 120,
            prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: 200 }),
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: 40,
            }),
        },
        finish_reason: "stop".into(),
    };
    let mut state = ChatCompletionsState::new();
    let translated = translate_one(&event, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed { usage, .. }) => {
            assert_eq!(usage.cache_read_input_tokens, 200);
            assert_eq!(usage.reasoning_tokens, 40);
            assert_eq!(usage.cache_write_input_tokens, 0);
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

#[test]
fn translate_done_length_to_incomplete() {
    let event = ChatCompletionEvent::Done {
        usage: ChatUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
        finish_reason: "length".into(),
    };
    let mut state = ChatCompletionsState::new();
    state.accumulated_text = "partial".into();
    let translated = translate_one(&event, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
            reason,
            partial_content,
            ..
        }) => {
            assert_eq!(reason, "max_tokens");
            assert_eq!(partial_content, "partial");
        }
        _ => panic!("expected Terminal(Incomplete)"),
    }
}

#[test]
fn translate_stream_end_without_finish_is_skip() {
    let event = ChatCompletionEvent::StreamEnd;
    let mut state = ChatCompletionsState::new();
    let translated = translate_one(&event, &mut state);
    assert!(matches!(translated, TranslatedEvent::Skip));
}

#[test]
fn translate_finish_then_usage_produces_completed() {
    let mut state = ChatCompletionsState::new();
    state.accumulated_text = "Hello".into();

    // Step 1: finish_reason arrives without usage — stashed, skip
    let finish = ChatCompletionEvent::FinishReason {
        finish_reason: "stop".into(),
    };
    let translated = translate_one(&finish, &mut state);
    assert!(matches!(translated, TranslatedEvent::Skip));
    assert_eq!(state.finish_reason.as_deref(), Some("stop"));

    // Step 2: usage-only chunk arrives — terminal with correct usage
    let usage = ChatCompletionEvent::Usage {
        usage: ChatUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
    };
    let translated = translate_one(&usage, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Completed { usage, content, .. }) => {
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 50);
            assert_eq!(content, "Hello");
        }
        _ => panic!("expected Terminal(Completed)"),
    }
}

#[test]
fn translate_finish_length_then_usage_produces_incomplete() {
    let mut state = ChatCompletionsState::new();
    state.accumulated_text = "partial".into();

    let finish = ChatCompletionEvent::FinishReason {
        finish_reason: "length".into(),
    };
    translate_chat_event(&finish, &mut state);

    let usage = ChatCompletionEvent::Usage {
        usage: ChatUsage {
            prompt_tokens: 200,
            completion_tokens: 100,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
    };
    let translated = translate_one(&usage, &mut state);
    match translated {
        TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
            reason,
            usage,
            partial_content,
        }) => {
            assert_eq!(reason, "max_tokens");
            assert_eq!(usage.input_tokens, 200);
            assert_eq!(usage.output_tokens, 100);
            assert_eq!(partial_content, "partial");
        }
        _ => panic!("expected Terminal(Incomplete)"),
    }
}

#[test]
fn translate_stream_end_with_stashed_finish_emits_terminal() {
    let mut state = ChatCompletionsState::new();
    state.accumulated_text = "text".into();
    state.finish_reason = Some("stop".into());

    let translated = translate_one(&ChatCompletionEvent::StreamEnd, &mut state);
    assert!(matches!(
        translated,
        TranslatedEvent::Terminal(TerminalOutcome::Completed { .. })
    ));
}

// ── Tool call translation tests ──────────────────────────────────────

#[test]
fn parse_tool_call_delta() {
    let event = ServerEvent {
            event: None,
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"get_weather","arguments":""}}]}}]}"#.into(),
            id: None,
            retry: None,
        };
    let result = ChatCompletionEvent::from_server_event(event).unwrap();
    match result {
        ChatCompletionEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(index, 0);
            assert_eq!(id.as_deref(), Some("call_abc"));
            assert_eq!(name.as_deref(), Some("get_weather"));
            assert_eq!(arguments.as_deref(), Some(""));
        }
        _ => panic!("expected ToolCallDelta"),
    }
}

#[test]
fn translate_tool_call_start_emitted_on_first_delta() {
    let event = ChatCompletionEvent::ToolCallDelta {
        index: 0,
        id: Some("call_abc".into()),
        name: Some("get_weather".into()),
        arguments: Some(String::new()),
        extra: vec![],
    };
    let mut state = ChatCompletionsState::new();
    let translated = translate_one(&event, &mut state);
    match translated {
        TranslatedEvent::Sse(ClientSseEvent::Tool {
            phase,
            name,
            details,
        }) => {
            assert!(matches!(phase, ToolPhase::Start));
            assert_eq!(name, "function_call");
            assert_eq!(details["name"], "get_weather");
            assert_eq!(details["call_id"], "call_abc");
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

#[test]
fn translate_tool_call_argument_deltas_are_skip() {
    let mut state = ChatCompletionsState::new();

    // First delta: Start event
    let first = ChatCompletionEvent::ToolCallDelta {
        index: 0,
        id: Some("call_abc".into()),
        name: Some("get_weather".into()),
        arguments: Some("{\"lo".into()),
        extra: vec![],
    };
    translate_one(&first, &mut state);

    // Subsequent delta: Skip (arguments accumulated)
    let cont = ChatCompletionEvent::ToolCallDelta {
        index: 0,
        id: None,
        name: None,
        arguments: Some("cation\":\"SF\"}".into()),
        extra: vec![],
    };
    let translated = translate_one(&cont, &mut state);
    assert!(matches!(translated, TranslatedEvent::Skip));

    // Verify arguments were accumulated
    assert_eq!(state.tool_calls[0].arguments, "{\"location\":\"SF\"}");
}

#[test]
fn translate_finish_tool_calls_emits_done_events() {
    let mut state = ChatCompletionsState::new();

    // Simulate accumulated tool call
    state.tool_calls.push(AccumulatedToolCall {
        id: "call_abc".into(),
        name: "get_weather".into(),
        arguments: r#"{"location":"SF"}"#.into(),
    });

    let finish = ChatCompletionEvent::FinishReason {
        finish_reason: "tool_calls".into(),
    };
    let events = translate_chat_event(&finish, &mut state);

    // Should emit 1 Done event for the tool call
    assert_eq!(events.len(), 1);
    match &events[0] {
        TranslatedEvent::Sse(ClientSseEvent::Tool {
            phase,
            name,
            details,
        }) => {
            assert!(matches!(phase, ToolPhase::Done));
            assert_eq!(*name, "function_call");
            assert_eq!(details["name"], "get_weather");
            assert_eq!(details["arguments"], r#"{"location":"SF"}"#);
        }
        _ => panic!("expected Sse(Tool)"),
    }
}

// ── Request serialization tests ───────────────────────────────────────

#[test]
fn request_basic_text() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hello"))
        .system_instructions("Be helpful")
        .max_output_tokens(4096)
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["max_completion_tokens"], 4096);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "Be helpful");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Hello");
}

#[test]
fn request_multi_turn() {
    let request = llm_request("gpt-4o")
        .message(LlmMessage::user("Hi"))
        .message(LlmMessage::assistant("Hello!"))
        .message(LlmMessage::user("How are you?"))
        .build_streaming();

    let body = build_request_body(&request, true);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
}

#[test]
fn request_user_identity_mapped() {
    let request = llm_request("gpt-4o")
        .user_identity("abc", "def")
        .message(LlmMessage::user("Hi"))
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["user"], "abc:def");
}

#[test]
fn request_function_tool_mapped() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::Function {
            name: "get_weather".into(),
            description: "Get weather".into(),
            parameters: serde_json::json!({"type": "object"}),
        })
        .message(LlmMessage::user("Hi"))
        .build_streaming();

    let body = build_request_body(&request, true);

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(body["tools"][0]["function"]["description"], "Get weather");
}

#[test]
fn request_file_search_dropped() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::FileSearch {
            vector_store_ids: vec!["vs-1".into()],
            filters: None,
            max_num_results: None,
        })
        .message(LlmMessage::user("Hi"))
        .build_streaming();

    let body = build_request_body(&request, true);

    assert!(body.get("tools").is_none());
}

#[test]
fn request_code_interpreter_dropped() {
    let request = llm_request("gpt-4o")
        .tool(LlmTool::CodeInterpreter {
            file_ids: vec!["file-1".into()],
        })
        .message(LlmMessage::user("Run analysis"))
        .build_streaming();

    let body = build_request_body(&request, true);

    assert!(body.get("tools").is_none());
}
