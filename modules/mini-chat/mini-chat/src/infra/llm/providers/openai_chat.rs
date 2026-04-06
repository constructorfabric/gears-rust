//! `OpenAI` Chat Completions API adapter (`/v1/chat/completions`).
//!
//! Implements [`LlmProvider`] by converting [`LlmRequest`] to the Chat
//! Completions API format, proxying through OAGW, parsing SSE events, and
//! translating them to the shared `TranslatedEvent` contract.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use modkit_security::SecurityContext;
use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::llm::request::{ContentPart as MessageContentPart, LlmTool, Role};
use crate::infra::llm::{
    ClientSseEvent, LlmProviderError, LlmRequest, NonStreaming, ProviderStream, RawDetail,
    ResponseResult, Streaming, TerminalOutcome, ToolPhase, TranslatedEvent, Usage,
};

// ════════════════════════════════════════════════════════════════════════════
// Chat Completions SSE event types
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug)]
enum ChatCompletionEvent {
    /// Text content delta.
    Delta {
        content: String,
        chunk_id: Option<String>,
    },
    /// Tool call delta (streamed incrementally).
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
        /// Additional tool-call deltas from the same chunk.
        extra: Vec<ToolCallPiece>,
    },
    /// Chunk with `finish_reason` set (but usage may arrive in a later chunk).
    FinishReason { finish_reason: String },
    /// Final usage-only chunk (empty choices, populated usage).
    Usage { usage: ChatUsage },
    /// Combined finish + usage in a single chunk.
    Done {
        usage: ChatUsage,
        finish_reason: String,
    },
    /// `data: [DONE]` sentinel.
    StreamEnd,
    /// Unrecognized chunk (ignored).
    Unknown,
}

/// A single tool-call delta extracted from the chunk.
#[derive(Debug)]
struct ToolCallPiece {
    index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: i64,
}

#[derive(Debug, Deserialize, Default)]
struct CompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionTokensDetails>,
}

// ════════════════════════════════════════════════════════════════════════════
// SSE deserialization helpers
// ════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: Option<ChatDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatDeltaToolCall>>,
}

#[derive(Deserialize)]
struct ChatDeltaToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChatDeltaFunction>,
}

#[derive(Deserialize)]
struct ChatDeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════════
// FromServerEvent
// ════════════════════════════════════════════════════════════════════════════

impl FromServerEvent for ChatCompletionEvent {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        let data = event.data.trim();

        // [DONE] sentinel
        if data == "[DONE]" {
            return Ok(ChatCompletionEvent::StreamEnd);
        }

        let chunk: ChatChunk =
            serde_json::from_str(data).map_err(|e| StreamingError::ServerEventsParse {
                detail: format!("failed to parse chat completion chunk: {e}"),
            })?;

        let finish_reason = chunk.choices.first().and_then(|c| c.finish_reason.clone());

        // Usage-only chunk: empty choices with populated usage (final chunk
        // when stream_options.include_usage = true).
        if chunk.choices.is_empty() {
            if let Some(usage) = chunk.usage {
                return Ok(ChatCompletionEvent::Usage { usage });
            }
            return Ok(ChatCompletionEvent::Unknown);
        }

        // Combined finish + usage in a single chunk.
        if let (Some(reason), Some(usage)) = (finish_reason.clone(), chunk.usage) {
            return Ok(ChatCompletionEvent::Done {
                usage,
                finish_reason: reason,
            });
        }

        // Finish reason without usage — usage arrives in a later chunk.
        if let Some(reason) = finish_reason {
            return Ok(ChatCompletionEvent::FinishReason {
                finish_reason: reason,
            });
        }

        // Tool call deltas — a chunk may carry more than one.
        if let Some(tool_calls) = chunk
            .choices
            .first()
            .and_then(|c| c.delta.as_ref())
            .and_then(|d| d.tool_calls.as_ref())
            && let Some(tc) = tool_calls.first()
        {
            // Return the first delta as this event; additional deltas in the
            // same chunk are accumulated in `translate_chat_event`.
            return Ok(ChatCompletionEvent::ToolCallDelta {
                index: tc.index,
                id: tc.id.clone(),
                name: tc.function.as_ref().and_then(|f| f.name.clone()),
                arguments: tc.function.as_ref().and_then(|f| f.arguments.clone()),
                extra: tool_calls
                    .iter()
                    .skip(1)
                    .map(|tc| ToolCallPiece {
                        index: tc.index,
                        id: tc.id.clone(),
                        name: tc.function.as_ref().and_then(|f| f.name.clone()),
                        arguments: tc.function.as_ref().and_then(|f| f.arguments.clone()),
                    })
                    .collect(),
            });
        }

        // Delta content.
        let content = chunk
            .choices
            .first()
            .and_then(|c| c.delta.as_ref())
            .and_then(|d| d.content.clone())
            .unwrap_or_default();

        if content.is_empty() {
            return Ok(ChatCompletionEvent::Unknown);
        }

        Ok(ChatCompletionEvent::Delta {
            content,
            chunk_id: chunk.id,
        })
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Scan state + translation
// ════════════════════════════════════════════════════════════════════════════

struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct ChatCompletionsState {
    accumulated_text: String,
    finish_reason: Option<String>,
    tool_calls: Vec<AccumulatedToolCall>,
    response_id: String,
}

impl ChatCompletionsState {
    fn new() -> Self {
        Self {
            accumulated_text: String::new(),
            finish_reason: None,
            tool_calls: Vec::new(),
            response_id: String::new(),
        }
    }

    /// Emit `Tool(Done)` events for all accumulated tool calls.
    fn tool_call_done_events(&self) -> Vec<TranslatedEvent> {
        self.tool_calls
            .iter()
            .map(|tc| {
                TranslatedEvent::Sse(ClientSseEvent::Tool {
                    phase: ToolPhase::Done,
                    name: "function_call",
                    details: serde_json::json!({
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": tc.arguments,
                    }),
                })
            })
            .collect()
    }

    fn make_terminal(&self, usage: &ChatUsage, finish_reason: &str) -> TranslatedEvent {
        let mapped_usage = Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_input_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .map_or(0, |d| d.cached_tokens),
            cache_write_input_tokens: 0,
            reasoning_tokens: usage
                .completion_tokens_details
                .as_ref()
                .map_or(0, |d| d.reasoning_tokens),
        };

        match finish_reason {
            "length" => TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
                reason: "max_tokens".to_owned(),
                usage: mapped_usage,
                partial_content: self.accumulated_text.clone(),
            }),
            _ => TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage: mapped_usage,
                response_id: self.response_id.clone(),
                content: self.accumulated_text.clone(),
                citations: vec![],
                raw_response: serde_json::Value::Null,
            }),
        }
    }
}

/// Accumulate a single tool-call delta into state, returning a `Start` event
/// on the first delta for an index or `Skip` for continuations.
fn accumulate_tool_call(
    state: &mut ChatCompletionsState,
    index: usize,
    id: Option<&String>,
    name: Option<&String>,
    arguments: Option<&String>,
) -> Vec<TranslatedEvent> {
    while state.tool_calls.len() <= index {
        state.tool_calls.push(AccumulatedToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
    }
    let tc = &mut state.tool_calls[index];
    if let Some(id) = id {
        tc.id.clone_from(id);
    }
    if let Some(name) = name {
        tc.name.clone_from(name);
    }
    if let Some(args) = arguments {
        tc.arguments.push_str(args);
    }
    if id.is_some() {
        vec![TranslatedEvent::Sse(ClientSseEvent::Tool {
            phase: ToolPhase::Start,
            name: "function_call",
            details: serde_json::json!({
                "index": index,
                "call_id": tc.id,
                "name": tc.name,
            }),
        })]
    } else {
        vec![TranslatedEvent::Skip]
    }
}

fn translate_chat_event(
    event: &ChatCompletionEvent,
    state: &mut ChatCompletionsState,
) -> Vec<TranslatedEvent> {
    match event {
        ChatCompletionEvent::Delta { content, chunk_id } => {
            if let Some(id) = chunk_id
                && state.response_id.is_empty()
            {
                state.response_id.clone_from(id);
            }
            state.accumulated_text.push_str(content);
            vec![TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: content.clone(),
            })]
        }

        ChatCompletionEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
            extra,
        } => {
            let mut events = accumulate_tool_call(
                state,
                *index,
                id.as_ref(),
                name.as_ref(),
                arguments.as_ref(),
            );
            for piece in extra {
                events.extend(accumulate_tool_call(
                    state,
                    piece.index,
                    piece.id.as_ref(),
                    piece.name.as_ref(),
                    piece.arguments.as_ref(),
                ));
            }
            events
        }

        // finish_reason arrived without usage — stash it for the usage chunk.
        ChatCompletionEvent::FinishReason { finish_reason } => {
            state.finish_reason = Some(finish_reason.clone());
            // Emit Done for accumulated tool calls when finish_reason is "tool_calls".
            if finish_reason == "tool_calls" {
                state.tool_call_done_events()
            } else {
                vec![TranslatedEvent::Skip]
            }
        }

        // Usage-only chunk (after finish_reason chunk).
        ChatCompletionEvent::Usage { usage } => {
            let reason = state.finish_reason.as_deref().unwrap_or("stop");
            vec![state.make_terminal(usage, reason)]
        }

        // Combined finish + usage in one chunk.
        ChatCompletionEvent::Done {
            usage,
            finish_reason,
        } => {
            let mut events = Vec::new();
            if finish_reason == "tool_calls" {
                events.extend(state.tool_call_done_events());
            }
            events.push(state.make_terminal(usage, finish_reason));
            events
        }

        // [DONE] sentinel — if we have a stashed finish_reason but never got
        // usage, emit terminal with zero usage as fallback.
        ChatCompletionEvent::StreamEnd => {
            if let Some(reason) = state.finish_reason.take() {
                let zero = ChatUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    prompt_tokens_details: None,
                    completion_tokens_details: None,
                };
                return vec![state.make_terminal(&zero, &reason)];
            }
            vec![TranslatedEvent::Skip]
        }

        ChatCompletionEvent::Unknown => vec![TranslatedEvent::Skip],
    }
}

// ════════════════════════════════════════════════════════════════════════════
// LlmRequest → Chat Completions conversion
// ════════════════════════════════════════════════════════════════════════════

#[allow(clippy::cognitive_complexity)]
fn build_request_body<M>(request: &LlmRequest<M>, stream: bool) -> serde_json::Value {
    let mut body = serde_json::json!({});

    body["model"] = serde_json::json!(&request.model);

    if stream {
        body["stream"] = serde_json::json!(true);
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }

    // Build messages array: system instruction as first system message
    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(ref instructions) = request.system_instructions {
        messages.push(serde_json::json!({
            "role": "system",
            "content": instructions
        }));
    }

    for msg in &request.messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };

        // Simple text messages use string content
        if msg.content.len() == 1
            && let MessageContentPart::Text { text } = &msg.content[0]
        {
            messages.push(serde_json::json!({
                "role": role,
                "content": text
            }));
            continue;
        }

        let content: Vec<serde_json::Value> = msg
            .content
            .iter()
            .map(|part| match part {
                MessageContentPart::Text { text } => serde_json::json!({
                    "type": "text",
                    "text": text
                }),
                MessageContentPart::Image { file_id } => serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": file_id }
                }),
            })
            .collect();

        messages.push(serde_json::json!({
            "role": role,
            "content": content
        }));
    }
    body["messages"] = serde_json::Value::Array(messages);

    if let Some(max_tokens) = request.max_output_tokens {
        body["max_completion_tokens"] = serde_json::json!(max_tokens);
    }

    // User field: "{tenant_id}:{user_id}"
    if let Some(ref identity) = request.user_identity {
        body["user"] = serde_json::json!(format!("{}:{}", identity.tenant_id, identity.user_id));
    }

    // Map tools: Function → Chat Completions function format, others dropped
    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .filter_map(|tool| match tool {
            LlmTool::Function {
                name,
                description,
                parameters,
            } => Some(serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            })),
            LlmTool::FileSearch { .. } => {
                debug!("FileSearch tool not supported by Chat Completions, dropping");
                None
            }
            LlmTool::WebSearch { .. } => {
                debug!("WebSearch tool not supported by Chat Completions, dropping");
                None
            }
            LlmTool::CodeInterpreter { .. } => {
                debug!("CodeInterpreter tool not supported by Chat Completions, dropping");
                None
            }
        })
        .collect();
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
    }

    body
}

fn body_to_bytes(body: &serde_json::Value) -> Body {
    #[allow(clippy::expect_used)]
    let json = serde_json::to_vec(body).expect("serde_json::Value always serializes");
    Body::Bytes(Bytes::from(json))
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAiChatProvider
// ════════════════════════════════════════════════════════════════════════════

/// `OpenAI` Chat Completions API adapter. Routes all calls through OAGW.
///
/// The upstream alias is not stored — it is passed per-request to allow
/// different tenants to route to different OAGW upstreams.
#[derive(Clone)]
pub struct OpenAiChatProvider {
    gateway: Arc<dyn ServiceGatewayClientV1>,
}

impl OpenAiChatProvider {
    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { gateway }
    }
}

/// Chat Completions error response payload.
#[derive(Deserialize)]
struct ChatErrorPayload {
    error: ChatErrorDetail,
}

#[derive(Deserialize)]
struct ChatErrorDetail {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: String,
}

/// Chat Completions non-streaming response.
#[derive(Deserialize)]
struct ChatResponse {
    id: String,
    choices: Vec<ChatResponseChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatResponseChoice {
    #[serde(default)]
    message: Option<ChatResponseMessage>,
    #[serde(default)]
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[async_trait::async_trait]
impl crate::infra::llm::LlmProvider for OpenAiChatProvider {
    #[tracing::instrument(
        skip(self, ctx, request, upstream_alias, cancel),
        fields(model = %request.model(), upstream = %upstream_alias)
    )]
    async fn stream(
        &self,
        ctx: SecurityContext,
        request: LlmRequest<Streaming>,
        upstream_alias: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, LlmProviderError> {
        let body = build_request_body(&request, true);
        let uri = format!("/{upstream_alias}");

        let http_request = http::Request::builder()
            .method(http::Method::POST)
            .uri(&uri)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "text/event-stream")
            .body(body_to_bytes(&body))
            .map_err(|e| LlmProviderError::InvalidResponse {
                detail: format!("failed to build HTTP request: {e}"),
            })?;

        let response = self.gateway.proxy_request(ctx, http_request).await?;

        match ServerEventsStream::from_response::<ChatCompletionEvent>(response) {
            ServerEventsResponse::Events(event_stream) => {
                let translated = event_stream
                    .scan(ChatCompletionsState::new(), |state, result| {
                        let outputs: Vec<Result<TranslatedEvent, StreamingError>> = match result {
                            Ok(event) => translate_chat_event(&event, state)
                                .into_iter()
                                .map(Ok)
                                .collect(),
                            Err(e) => vec![Err(e)],
                        };
                        async move { Some(futures::stream::iter(outputs)) }
                    })
                    .flatten();

                Ok(ProviderStream::new(translated, cancel))
            }
            ServerEventsResponse::Response(resp) => {
                let (_parts, body) = resp.into_parts();
                match body.into_bytes().await {
                    Ok(bytes) => {
                        if let Ok(error_payload) =
                            serde_json::from_slice::<ChatErrorPayload>(&bytes)
                        {
                            let raw = error_payload.error.message.clone();
                            Err(LlmProviderError::ProviderError {
                                code: error_payload.error.code.unwrap_or_default(),
                                message: crate::infra::llm::sanitize_provider_message(&raw),
                                raw_detail: Some(RawDetail(raw)),
                            })
                        } else {
                            let body_str = String::from_utf8_lossy(&bytes);
                            let snippet = crate::infra::llm::sanitize_provider_message(
                                &body_str.chars().take(200).collect::<String>(),
                            );
                            Err(LlmProviderError::InvalidResponse {
                                detail: format!(
                                    "non-SSE response with unparseable body: {snippet}"
                                ),
                            })
                        }
                    }
                    Err(e) => Err(LlmProviderError::InvalidResponse {
                        detail: format!("failed to read response body: {e}"),
                    }),
                }
            }
        }
    }

    #[tracing::instrument(
        skip(self, ctx, request, upstream_alias),
        fields(model = %request.model(), upstream = %upstream_alias)
    )]
    async fn complete(
        &self,
        ctx: SecurityContext,
        request: LlmRequest<NonStreaming>,
        upstream_alias: &str,
    ) -> Result<ResponseResult, LlmProviderError> {
        let body = build_request_body(&request, false);
        let uri = format!("/{upstream_alias}");

        let http_request = http::Request::builder()
            .method(http::Method::POST)
            .uri(&uri)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(body_to_bytes(&body))
            .map_err(|e| LlmProviderError::InvalidResponse {
                detail: format!("failed to build HTTP request: {e}"),
            })?;

        let response = self.gateway.proxy_request(ctx, http_request).await?;

        let (parts, resp_body) = response.into_parts();
        let bytes =
            resp_body
                .into_bytes()
                .await
                .map_err(|e| LlmProviderError::InvalidResponse {
                    detail: format!("failed to read response body: {e}"),
                })?;

        if !parts.status.is_success() {
            if let Ok(error_payload) = serde_json::from_slice::<ChatErrorPayload>(&bytes) {
                let raw = error_payload.error.message.clone();
                return Err(LlmProviderError::ProviderError {
                    code: error_payload.error.code.unwrap_or_default(),
                    message: crate::infra::llm::sanitize_provider_message(&raw),
                    raw_detail: Some(RawDetail(raw)),
                });
            }
            let body_str = String::from_utf8_lossy(&bytes);
            let snippet = crate::infra::llm::sanitize_provider_message(
                &body_str.chars().take(200).collect::<String>(),
            );
            return Err(LlmProviderError::InvalidResponse {
                detail: format!("HTTP {}: {snippet}", parts.status),
            });
        }

        let resp: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|e| LlmProviderError::InvalidResponse {
                detail: format!("failed to parse response: {e}"),
            })?;

        let content = resp
            .choices
            .first()
            .and_then(|c| c.message.as_ref())
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let usage = resp.usage.map_or(
            Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            |u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_read_input_tokens: u
                    .prompt_tokens_details
                    .as_ref()
                    .map_or(0, |d| d.cached_tokens),
                cache_write_input_tokens: 0,
                reasoning_tokens: u
                    .completion_tokens_details
                    .as_ref()
                    .map_or(0, |d| d.reasoning_tokens),
            },
        );

        Ok(ResponseResult {
            content,
            usage,
            response_id: resp.id,
            citations: vec![],
            raw_response: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        })
    }
}
// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "openai_chat_tests.rs"]
mod tests;
