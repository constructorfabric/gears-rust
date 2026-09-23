//! Anthropic Messages API adapter.
//!
//! Implements [`LlmProvider`] using the Anthropic Messages API (`/v1/messages`).
//! Translates [`LlmRequest`] to the Anthropic wire format, processes named SSE
//! events via a stateful scan pass, and converts them to the shared
//! `TranslatedEvent` contract.
//!
//! ## Agentic loop integration
//!
//! `raw_input_items` arriving in OpenAI Responses API format (`function_call` /
//! `function_call_output`) are converted to Anthropic message pairs (assistant
//! `tool_use` block + user `tool_result` block) before the request is sent.
//!
//! ## Tool result content
//!
//! When `function_call_output.output` parses as a JSON array whose elements all
//! carry a `"type"` field, the blocks are forwarded verbatim as `tool_result`
//! content (enabling `search_result` blocks with citations). Otherwise the raw
//! string is wrapped in a single `{"type":"text"}` block.

#![allow(
    clippy::non_ascii_literal,
    clippy::assigning_clones,
    clippy::struct_field_names,
    clippy::doc_markdown,
    clippy::single_match_else,
    clippy::cognitive_complexity,
    clippy::match_same_arms,
    clippy::collapsible_if
)]

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::debug;

use crate::infra::llm::request::{ContentPart as MessageContentPart, LlmTool};
use crate::infra::llm::{
    ClientSseEvent, LlmProviderError, LlmRequest, NonStreaming, ProviderStream, RawDetail,
    ResponseResult, Streaming, TerminalOutcome, ToolPhase, TranslatedEvent, Usage,
};

/// Anthropic Messages API version. Pinned to the request/response shape this
/// adapter is written against — bumping it requires adapter code changes.
pub(crate) const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Anthropic Files API beta opt-in. Required on `/v1/messages` requests that
/// reference attachments by `file_id` (image/document blocks via the Files
/// API), AND on every `/v1/files` request. See
/// `anthropic-provider-support.md` §4.5.
pub(crate) const ANTHROPIC_FILES_BETA: &str = "files-api-2025-04-14";

/// OAGW upstream header rules for any Anthropic Messages API upstream.
///
/// `anthropic-version` is the wire-protocol contract for this adapter and is
/// injected on every outbound request, so individual call sites don't need
/// to set it (and OAGW would strip it under the default `PassthroughMode::None`).
///
/// `accept` and `anthropic-beta` are allowlisted for passthrough so adapters
/// can vary them per request: SSE vs JSON for `Accept`, beta feature flags
/// (e.g. `files-api-2025-04-14`) for `anthropic-beta`.
#[must_use]
pub fn upstream_headers() -> oagw_sdk::HeadersConfig {
    oagw_sdk::HeadersConfig {
        request: Some(oagw_sdk::RequestHeaderRules {
            set: HashMap::from([(
                "anthropic-version".to_owned(),
                ANTHROPIC_API_VERSION.to_owned(),
            )]),
            passthrough: oagw_sdk::PassthroughMode::Allowlist,
            passthrough_allowlist: vec!["accept".to_owned(), "anthropic-beta".to_owned()],
            ..Default::default()
        }),
        response: None,
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Anthropic SSE event types
// ════════════════════════════════════════════════════════════════════════════

/// Decoded Anthropic Messages API SSE event.
#[derive(Debug)]
pub(super) enum AnthropicEvent {
    MessageStart {
        message_id: String,
        input_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
    },
    ContentBlockStartText {
        #[allow(dead_code)]
        index: u32,
    },
    ContentBlockStartToolUse {
        #[allow(dead_code)]
        index: u32,
        id: String,
        name: String,
    },
    ContentBlockStartServerToolUse {
        #[allow(dead_code)]
        index: u32,
        name: String,
    },
    ContentBlockDeltaText {
        #[allow(dead_code)]
        index: u32,
        text: String,
    },
    ContentBlockDeltaInputJson {
        #[allow(dead_code)]
        index: u32,
        partial_json: String,
    },
    ContentBlockStop {
        #[allow(dead_code)]
        index: u32,
    },
    MessageDelta {
        stop_reason: String,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
    },
    MessageStop,
    Ping,
    Error {
        error_type: String,
        message: String,
    },
    Unknown {
        #[allow(dead_code)]
        event_name: String,
    },
}

// ════════════════════════════════════════════════════════════════════════════
// SSE deserialization helpers
// ════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct MessageStartData {
    message: MessageStartMessage,
}

#[derive(Deserialize)]
struct MessageStartMessage {
    id: String,
    #[serde(default)]
    usage: MessageStartUsage,
}

#[derive(Deserialize, Default)]
struct MessageStartUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[derive(Deserialize)]
struct ContentBlockStartData {
    index: u32,
    content_block: ContentBlockInfo,
}

#[derive(Deserialize)]
struct ContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ContentBlockDeltaData {
    index: u32,
    delta: ContentBlockDelta,
}

#[derive(Deserialize)]
struct ContentBlockDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    partial_json: String,
}

#[derive(Deserialize)]
struct ContentBlockStopData {
    index: u32,
}

#[derive(Deserialize)]
struct MessageDeltaData {
    delta: MessageDeltaInner,
    #[serde(default)]
    usage: MessageDeltaUsage,
}

#[derive(Deserialize)]
struct MessageDeltaInner {
    #[serde(default)]
    stop_reason: String,
}

#[derive(Deserialize, Default)]
struct MessageDeltaUsage {
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[derive(Deserialize)]
struct StreamErrorData {
    error: StreamErrorDetail,
}

#[derive(Deserialize, Default)]
struct StreamErrorDetail {
    #[serde(rename = "type", default)]
    error_type: String,
    #[serde(default)]
    message: String,
}

// ════════════════════════════════════════════════════════════════════════════
// FromServerEvent
// ════════════════════════════════════════════════════════════════════════════

impl FromServerEvent for AnthropicEvent {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        let event_name = event.event.as_deref().unwrap_or("unknown");

        match event_name {
            "message_start" => {
                let data: MessageStartData = serde_json::from_str(&event.data).map_err(|e| {
                    StreamingError::ServerEventsParse {
                        detail: format!("failed to parse message_start: {e}"),
                    }
                })?;
                Ok(AnthropicEvent::MessageStart {
                    message_id: data.message.id,
                    input_tokens: data.message.usage.input_tokens,
                    cache_read_input_tokens: data.message.usage.cache_read_input_tokens,
                    cache_creation_input_tokens: data.message.usage.cache_creation_input_tokens,
                })
            }

            "content_block_start" => {
                let data: ContentBlockStartData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse content_block_start: {e}"),
                        }
                    })?;
                match data.content_block.block_type.as_str() {
                    "tool_use" => Ok(AnthropicEvent::ContentBlockStartToolUse {
                        index: data.index,
                        id: data.content_block.id,
                        name: data.content_block.name,
                    }),
                    "server_tool_use" => Ok(AnthropicEvent::ContentBlockStartServerToolUse {
                        index: data.index,
                        name: data.content_block.name,
                    }),
                    _ => Ok(AnthropicEvent::ContentBlockStartText { index: data.index }),
                }
            }

            "content_block_delta" => {
                let data: ContentBlockDeltaData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse content_block_delta: {e}"),
                        }
                    })?;
                match data.delta.delta_type.as_str() {
                    "input_json_delta" => Ok(AnthropicEvent::ContentBlockDeltaInputJson {
                        index: data.index,
                        partial_json: data.delta.partial_json,
                    }),
                    _ => Ok(AnthropicEvent::ContentBlockDeltaText {
                        index: data.index,
                        text: data.delta.text,
                    }),
                }
            }

            "content_block_stop" => {
                let data: ContentBlockStopData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse content_block_stop: {e}"),
                        }
                    })?;
                Ok(AnthropicEvent::ContentBlockStop { index: data.index })
            }

            "message_delta" => {
                let data: MessageDeltaData = serde_json::from_str(&event.data).map_err(|e| {
                    StreamingError::ServerEventsParse {
                        detail: format!("failed to parse message_delta: {e}"),
                    }
                })?;
                Ok(AnthropicEvent::MessageDelta {
                    stop_reason: data.delta.stop_reason,
                    output_tokens: data.usage.output_tokens,
                    cache_read_input_tokens: data.usage.cache_read_input_tokens,
                    cache_creation_input_tokens: data.usage.cache_creation_input_tokens,
                })
            }

            "message_stop" => Ok(AnthropicEvent::MessageStop),

            "ping" => Ok(AnthropicEvent::Ping),

            "error" => {
                if let Ok(data) = serde_json::from_str::<StreamErrorData>(&event.data) {
                    Ok(AnthropicEvent::Error {
                        error_type: data.error.error_type,
                        message: data.error.message,
                    })
                } else {
                    Ok(AnthropicEvent::Error {
                        error_type: "unknown".to_owned(),
                        message: event.data.clone(),
                    })
                }
            }

            other => {
                debug!(event_name = other, "ignoring unhandled Anthropic SSE event");
                Ok(AnthropicEvent::Unknown {
                    event_name: other.to_owned(),
                })
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Stream state machine
// ════════════════════════════════════════════════════════════════════════════

/// Mutable state carried across the SSE event scan pass.
#[derive(Default)]
struct AnthropicStreamState {
    message_id: String,
    accumulated_text: String,
    tool_use_id: String,
    tool_name: String,
    tool_input_json: String,
    /// Name of the currently-open server tool block (`web_search` or
    /// `code_execution`). Set on `content_block_start` and cleared on
    /// matching `content_block_stop` so we can emit `Tool { Done, name }`.
    current_server_tool: Option<&'static str>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_write_input_tokens: i64,
    stop_reason: String,
}

/// Translate one Anthropic SSE event into the shared contract.
fn translate_anthropic_event(
    event: &AnthropicEvent,
    state: &mut AnthropicStreamState,
) -> TranslatedEvent {
    match event {
        AnthropicEvent::MessageStart {
            message_id,
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => {
            state.message_id = message_id.clone();
            state.input_tokens = *input_tokens;
            state.cache_read_input_tokens = *cache_read_input_tokens;
            state.cache_write_input_tokens = *cache_creation_input_tokens;
            TranslatedEvent::Skip
        }

        AnthropicEvent::ContentBlockStartText { .. } => TranslatedEvent::Skip,

        AnthropicEvent::ContentBlockStartToolUse { id, name, .. } => {
            state.tool_use_id = id.clone();
            state.tool_name = name.clone();
            state.tool_input_json.clear();
            let static_name: &'static str = match name.as_str() {
                "search_knowledge" => "search_knowledge",
                "load_files" => "load_files",
                _ => "unknown_tool",
            };
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: static_name,
                details: serde_json::json!({}),
            })
        }

        AnthropicEvent::ContentBlockStartServerToolUse { name, .. } => {
            // Anthropic prefixes server-tool names with the tool family
            // (`web_search`, `code_execution`); strip any version suffix and
            // map to the shared SSE name used across adapters. Anthropic's
            // `code_execution` is the same concept as OpenAI's
            // `code_interpreter` — emit the shared name so `provider_task.rs`
            // (which keys limits/persistence on `"code_interpreter"`)
            // accounts for Anthropic code runs the same way.
            let static_name: Option<&'static str> = if name.starts_with("web_search") {
                Some("web_search")
            } else if name.starts_with("code_execution") {
                Some("code_interpreter")
            } else {
                None
            };
            match static_name {
                Some(n) => {
                    state.current_server_tool = Some(n);
                    TranslatedEvent::Sse(ClientSseEvent::Tool {
                        phase: ToolPhase::Start,
                        name: n,
                        details: serde_json::json!({}),
                    })
                }
                None => {
                    debug!(server_tool = %name, "unrecognised Anthropic server tool, skipping");
                    TranslatedEvent::Skip
                }
            }
        }

        AnthropicEvent::ContentBlockDeltaText { text, .. } => {
            state.accumulated_text.push_str(text);
            TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: text.clone(),
            })
        }

        AnthropicEvent::ContentBlockDeltaInputJson { partial_json, .. } => {
            state.tool_input_json.push_str(partial_json);
            TranslatedEvent::Skip
        }

        AnthropicEvent::ContentBlockStop { .. } => match state.current_server_tool.take() {
            Some(name) => TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name,
                details: serde_json::json!({}),
            }),
            None => TranslatedEvent::Skip,
        },

        AnthropicEvent::MessageDelta {
            stop_reason,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        } => {
            state.stop_reason = stop_reason.clone();
            state.output_tokens = *output_tokens;
            // Anthropic only echoes cache totals in message_delta when non-zero;
            // keep the values from message_start if the delta omits them.
            if *cache_read_input_tokens != 0 {
                state.cache_read_input_tokens = *cache_read_input_tokens;
            }
            if *cache_creation_input_tokens != 0 {
                state.cache_write_input_tokens = *cache_creation_input_tokens;
            }
            TranslatedEvent::Skip
        }

        AnthropicEvent::MessageStop => {
            // Anthropic reports cache tokens separately from `input_tokens`,
            // while OpenAI folds them in. Sum them here so the credits
            // formula sees a provider-agnostic input total. The raw cache
            // breakdown is still preserved for observability.
            let usage = Usage {
                input_tokens: state.input_tokens
                    + state.cache_read_input_tokens
                    + state.cache_write_input_tokens,
                output_tokens: state.output_tokens,
                cache_read_input_tokens: state.cache_read_input_tokens,
                cache_write_input_tokens: state.cache_write_input_tokens,
                reasoning_tokens: 0,
            };

            match state.stop_reason.as_str() {
                "tool_use" => {
                    let input: serde_json::Value = serde_json::from_str(&state.tool_input_json)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    debug!(
                        tool_use_id = %state.tool_use_id,
                        tool_name = %state.tool_name,
                        "Anthropic signalled tool use — starting agentic loop iteration"
                    );
                    TranslatedEvent::Terminal(TerminalOutcome::ToolUse {
                        tool_use_id: state.tool_use_id.clone(),
                        name: state.tool_name.clone(),
                        input,
                    })
                }
                "max_tokens" => TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
                    reason: "max_tokens".to_owned(),
                    usage,
                    partial_content: state.accumulated_text.clone(),
                }),
                _ => TranslatedEvent::Terminal(TerminalOutcome::Completed {
                    usage,
                    response_id: state.message_id.clone(),
                    content: state.accumulated_text.clone(),
                    citations: vec![],
                    raw_response: serde_json::json!({ "id": state.message_id }),
                }),
            }
        }

        AnthropicEvent::Ping | AnthropicEvent::Unknown { .. } => TranslatedEvent::Skip,

        AnthropicEvent::Error {
            error_type,
            message,
        } => {
            let sanitized = crate::infra::llm::sanitize_provider_message(message);
            TranslatedEvent::Terminal(TerminalOutcome::Failed {
                error: LlmProviderError::ProviderError {
                    code: error_type.clone(),
                    message: sanitized,
                    raw_detail: Some(RawDetail(message.clone())),
                },
                usage: None,
                partial_content: state.accumulated_text.clone(),
            })
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// LlmRequest → Anthropic Messages API conversion
// ════════════════════════════════════════════════════════════════════════════

/// Build the Anthropic Messages API JSON body from an [`LlmRequest`].
fn build_request_body<M>(request: &LlmRequest<M>, stream: bool) -> serde_json::Value {
    let mut body = serde_json::json!({ "stream": stream });

    body["model"] = serde_json::json!(&request.model);

    // max_tokens is required by the Anthropic Messages API.
    body["max_tokens"] = serde_json::json!(request.max_output_tokens.unwrap_or(4096));

    // Inference params Anthropic accepts. `frequency_penalty`, `presence_penalty`,
    // `reasoning_effort`, and `extra_body` are OpenAI-shaped and intentionally
    // dropped here.
    //
    // Anthropic 4.x reasoning models reject requests that specify both
    // `temperature` and `top_p` ("cannot both be specified for this model").
    // Treat `top_p == 1.0` (the no-op identity) as "operator left top_p alone"
    // and forward only `temperature`. If `top_p` is genuinely tuned (< 1.0),
    // forward `top_p` only and drop `temperature`.
    if let Some(p) = request.api_params.as_ref() {
        if p.top_p < 1.0 {
            body["top_p"] = serde_json::json!(p.top_p);
            debug!(
                top_p = p.top_p,
                "Anthropic adapter: forwarding top_p only (temperature dropped)"
            );
        } else {
            body["temperature"] = serde_json::json!(p.temperature);
        }
        if !p.stop.is_empty() {
            body["stop_sequences"] = serde_json::json!(&p.stop);
        }
    }

    if let Some(ref instructions) = request.system_instructions {
        body["system"] = serde_json::json!(instructions);
    }

    let mut messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter(|msg| msg.role != crate::infra::llm::request::Role::System)
        .map(|msg| {
            let role = match msg.role {
                crate::infra::llm::request::Role::User => "user",
                crate::infra::llm::request::Role::Assistant => "assistant",
                crate::infra::llm::request::Role::System => unreachable!(),
            };
            let content: Vec<serde_json::Value> = msg
                .content
                .iter()
                .filter_map(|part| match part {
                    MessageContentPart::Text { text } => {
                        Some(serde_json::json!({ "type": "text", "text": text }))
                    }
                    // The `file_id` carried in `MessageContentPart::Image` is
                    // the *primary* (e.g. Azure) provider id. Anthropic's
                    // Files API uses its own ids — substitute via the map
                    // populated from `attachments.anthropic_file_id`. If the
                    // parallel upload to Anthropic failed (or the file
                    // pre-dates Anthropic support), the entry is missing —
                    // skip the block with a warning rather than send a
                    // bogus reference that would 4xx the whole request.
                    MessageContentPart::Image { file_id } => {
                        match request.anthropic_file_ids.get(file_id) {
                            Some(anthropic_id) => Some(serde_json::json!({
                                "type": "image",
                                "source": { "type": "file", "file_id": anthropic_id }
                            })),
                            None => {
                                debug!(
                                    primary_file_id = %file_id,
                                    "Anthropic adapter: dropping image block — \
                                     no anthropic_file_id (parallel upload missing/failed)"
                                );
                                None
                            }
                        }
                    }
                })
                .collect();
            serde_json::json!({ "role": role, "content": content })
        })
        .collect();

    // Convert raw_input_items from OpenAI Responses format to Anthropic message pairs.
    if !request.raw_input_items.is_empty() {
        messages.extend(convert_raw_input_items(&request.raw_input_items));
    }

    if !messages.is_empty() {
        body["messages"] = serde_json::Value::Array(messages);
    }

    if let Some(ref identity) = request.user_identity {
        body["metadata"] = serde_json::json!({
            "user_id": format!("{}:{}", identity.tenant_id, identity.user_id)
        });
    }

    // WebSearch and CodeInterpreter map to Anthropic's native server-side
    // tools; Function tools pass through. FileSearch is silently dropped —
    // RAG on Anthropic is delivered via a custom function tool, not a native
    // server tool.
    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .filter_map(|tool| match tool {
            LlmTool::Function {
                name,
                description,
                parameters,
            } => Some(serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": parameters
            })),
            LlmTool::WebSearch { .. } => Some(serde_json::json!({
                "type": "web_search_20260209",
                "name": "web_search"
            })),
            LlmTool::CodeInterpreter { .. } => Some(serde_json::json!({
                "type": "code_execution_20250825",
                "name": "code_execution"
            })),
            LlmTool::FileSearch { .. } => {
                debug!("Anthropic adapter: skipping FileSearch (handled via custom function tool)");
                None
            }
        })
        .collect();
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        // The Anthropic stream state machine only preserves a single
        // tool_use block per assistant message (one `tool_use_id` /
        // `tool_name` / `tool_input_json` slot, overwritten on each
        // ContentBlockStartToolUse). Without this flag, parallel
        // `search_knowledge` / `web_search` calls in one assistant turn
        // would be silently lost. Force the API to emit at most one
        // tool_use per message until the state machine grows multi-tool
        // tracking.
        body["tool_choice"] = serde_json::json!({
            "type": "auto",
            "disable_parallel_tool_use": true,
        });
    }

    // Merge additional provider-specific params (e.g. `thinking` configuration).
    if let Some(ref extra) = request.additional_params
        && let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object())
    {
        for (k, v) in extra_obj {
            body_obj.insert(k.clone(), v.clone());
        }
    }

    body
}

/// Convert OpenAI Responses API `raw_input_items` to Anthropic message pairs.
///
/// Groups consecutive `function_call` items into an assistant message with
/// `tool_use` blocks, and `function_call_output` items into a user message
/// with `tool_result` blocks.
fn convert_raw_input_items(items: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    let mut tool_use_blocks: Vec<serde_json::Value> = Vec::new();
    let mut tool_result_blocks: Vec<serde_json::Value> = Vec::new();

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "function_call" => {
                if !tool_result_blocks.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": std::mem::take(&mut tool_result_blocks)
                    }));
                }
                let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let input: serde_json::Value =
                    serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
                tool_use_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input
                }));
            }
            "function_call_output" => {
                if !tool_use_blocks.is_empty() {
                    result.push(serde_json::json!({
                        "role": "assistant",
                        "content": std::mem::take(&mut tool_use_blocks)
                    }));
                }
                let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                let output = item.get("output").and_then(|v| v.as_str()).unwrap_or("");
                let content = parse_tool_result_content(output);
                tool_result_blocks.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content
                }));
            }
            _ => {}
        }
    }

    if !tool_use_blocks.is_empty() {
        result.push(serde_json::json!({
            "role": "assistant",
            "content": tool_use_blocks
        }));
    }
    if !tool_result_blocks.is_empty() {
        result.push(serde_json::json!({
            "role": "user",
            "content": tool_result_blocks
        }));
    }

    result
}

/// Parse tool result output for Anthropic `tool_result.content`.
///
/// If the output is a JSON array where every element has a `"type"` field,
/// the blocks are forwarded verbatim (supporting `search_result` with
/// citations). Otherwise the raw string is wrapped in a plain text block.
pub(super) fn parse_tool_result_content(output: &str) -> serde_json::Value {
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(output) {
        if !arr.is_empty() && arr.iter().all(|v| v.get("type").is_some()) {
            return serde_json::Value::Array(arr);
        }
    }
    serde_json::json!([{ "type": "text", "text": output }])
}

// ════════════════════════════════════════════════════════════════════════════
// Anthropic non-streaming response types
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, Serialize)]
struct AnthropicMessageResponse {
    id: String,
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize, Serialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

// ════════════════════════════════════════════════════════════════════════════
// Error parsing
// ════════════════════════════════════════════════════════════════════════════

/// Parse an Anthropic error response body into an `LlmProviderError`.
///
/// Anthropic wraps errors as `{"type":"error","error":{"type":"...","message":"..."}}`.
/// The HTTP status and headers are folded into the surfaced message so
/// downstream UIs (and operators reading logs) get actionable information
/// even when Anthropic's `message` is uninformative ("Error", empty, etc.).
///
/// `error.type == "rate_limit_error"` is recognised and routed to
/// [`LlmProviderError::RateLimited`] so the UI gets the dedicated rate-limit
/// treatment (with `retry-after` if Anthropic supplied one).
pub(super) fn parse_anthropic_error(
    status: http::StatusCode,
    headers: &http::HeaderMap,
    bytes: &[u8],
) -> LlmProviderError {
    #[derive(Deserialize)]
    struct Envelope {
        error: ErrorBody,
    }
    #[derive(Deserialize, Default)]
    struct ErrorBody {
        #[serde(rename = "type", default)]
        error_type: String,
        #[serde(default)]
        message: String,
    }

    let retry_after_secs = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    if let Ok(e) = serde_json::from_slice::<Envelope>(bytes) {
        // Route rate-limit bodies to the dedicated variant so the UI shows
        // "Rate limited" instead of a generic provider error.
        if e.error.error_type == "rate_limit_error" || status == http::StatusCode::TOO_MANY_REQUESTS
        {
            return LlmProviderError::RateLimited { retry_after_secs };
        }

        // Build a message that is useful even when Anthropic's `message` is
        // terse: prefix the upstream error code, suffix the HTTP status.
        let upstream_msg = e.error.message.trim();
        let composed = if upstream_msg.is_empty() {
            format!("[{}] (HTTP {})", e.error.error_type, status.as_u16())
        } else {
            format!(
                "[{}] {} (HTTP {})",
                e.error.error_type,
                upstream_msg,
                status.as_u16()
            )
        };
        let raw = e.error.message.clone();
        return LlmProviderError::ProviderError {
            code: e.error.error_type,
            message: crate::infra::llm::sanitize_provider_message(&composed),
            raw_detail: Some(RawDetail(raw)),
        };
    }

    let body_str = String::from_utf8_lossy(bytes);
    let snippet = crate::infra::llm::sanitize_provider_message(
        &body_str.chars().take(200).collect::<String>(),
    );
    LlmProviderError::InvalidResponse {
        detail: format!(
            "non-SSE response (HTTP {}) with unparseable body: {snippet}",
            status.as_u16()
        ),
    }
}

/// Filter response headers to the diagnostically-interesting subset for
/// Anthropic — rate-limit, request-id, content-type, and any `anthropic-*`
/// headers. Other headers (e.g. `cf-ray`, `x-frame-options`) are noise.
fn format_diag_headers(headers: &http::HeaderMap) -> String {
    let mut pairs: Vec<String> = headers
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str();
            n == "request-id"
                || n == "retry-after"
                || n == "content-type"
                || n.starts_with("anthropic-")
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| format!("{}={}", name.as_str(), v))
        })
        .collect();
    pairs.sort();
    pairs.join(", ")
}

/// Truncate a string to `max_chars` characters with an ellipsis suffix when
/// truncated — char-aware so multibyte characters aren't split mid-codepoint.
fn truncate_for_log(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push_str("…[truncated]");
        out
    }
}

/// Emit a single structured `warn!` capturing every wire-level detail of a
/// failed Anthropic request: the outbound URI and request body (verbatim,
/// no provider-id stripping — there are none in our request), the response
/// status, the diagnostically-relevant response headers, and the response
/// body (sanitized of any leaked provider IDs).
///
/// Centralised so the streaming and non-streaming paths log identically.
///
/// Body content is suppressed by default — request bodies contain user
/// prompts/tool args, response bodies contain model output and retrieved
/// knowledge chunks, both of which are sensitive. Set
/// `MINI_CHAT_LOG_LLM_BODIES=1` to opt in to truncated body logging for
/// debugging adapter/upstream contract drift.
fn log_anthropic_error_response(
    op: &'static str,
    uri: &str,
    request_body: &serde_json::Value,
    status: http::StatusCode,
    headers: &http::HeaderMap,
    response_bytes: &[u8],
) {
    let diag_headers = format_diag_headers(headers);
    let response_size = response_bytes.len();

    if llm_body_logging_enabled() {
        let request_body_str =
            serde_json::to_string(request_body).unwrap_or_else(|_| "<unrepresentable>".to_owned());
        let request_body_log = truncate_for_log(&request_body_str, 4000);

        let response_body_str = String::from_utf8_lossy(response_bytes);
        let response_body_sanitized =
            crate::infra::llm::sanitize_provider_message(&response_body_str);
        let response_body_log = truncate_for_log(&response_body_sanitized, 4000);

        tracing::warn!(
            op,
            uri,
            status = status.as_u16(),
            response_headers = %diag_headers,
            request_body = %request_body_log,
            response_body = %response_body_log,
            "Anthropic non-success response (full diagnostic; bodies opt-in)"
        );
    } else {
        tracing::warn!(
            op,
            uri,
            status = status.as_u16(),
            response_headers = %diag_headers,
            response_size_bytes = response_size,
            "Anthropic non-success response (bodies redacted; \
             set MINI_CHAT_LOG_LLM_BODIES=1 to include)"
        );
    }
}

/// Returns `true` when raw request/response body logging is opt-in enabled.
/// Cached at first read; never changes for the lifetime of the process.
fn llm_body_logging_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("MINI_CHAT_LOG_LLM_BODIES")
            .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
    })
}

// ════════════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════════════

#[allow(clippy::expect_used)]
fn body_to_bytes(body: &serde_json::Value) -> Body {
    let json = serde_json::to_vec(body).expect("serde_json::Value always serializes");
    Body::Bytes(Bytes::from(json))
}

// ════════════════════════════════════════════════════════════════════════════
// AnthropicMessagesProvider
// ════════════════════════════════════════════════════════════════════════════

/// Anthropic Messages API adapter. Routes all calls through OAGW.
///
/// The upstream alias is not stored — it is passed per-request to allow
/// different tenants to route to different OAGW upstreams.
#[derive(Clone)]
pub struct AnthropicMessagesProvider {
    gateway: Arc<dyn ServiceGatewayClientV1>,
}

impl AnthropicMessagesProvider {
    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { gateway }
    }
}

#[async_trait::async_trait]
impl crate::infra::llm::LlmProvider for AnthropicMessagesProvider {
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
            // Required when the body references attachments by `file_id`
            // (image / document blocks via Anthropic Files API). Harmless
            // when no attachments are referenced — Anthropic ignores the
            // beta opt-in for requests that don't touch beta features.
            .header("anthropic-beta", ANTHROPIC_FILES_BETA)
            .body(body_to_bytes(&body))
            .map_err(|e| LlmProviderError::InvalidResponse {
                detail: format!("failed to build HTTP request: {e}"),
            })?;

        // Always log the outbound URI at debug so successful streams have
        // correlatable wire-level traces. Body content is suppressed unless
        // opt-in via `MINI_CHAT_LOG_LLM_BODIES=1` — see
        // `log_anthropic_error_response` for the rationale.
        if llm_body_logging_enabled() {
            debug!(
                uri = %uri,
                request_body = %truncate_for_log(&body.to_string(), 4000),
                "sending streaming request to Anthropic (body opt-in)"
            );
        } else {
            debug!(
                uri = %uri,
                "sending streaming request to Anthropic (body redacted)"
            );
        }

        let response = self.gateway.proxy_request(ctx, http_request).await?;

        match ServerEventsStream::from_response::<AnthropicEvent>(response) {
            ServerEventsResponse::Events(event_stream) => {
                let translated =
                    event_stream.scan(AnthropicStreamState::default(), |state, result| {
                        let output = match result {
                            Ok(event) => Ok(translate_anthropic_event(&event, state)),
                            Err(e) => {
                                tracing::warn!(error = %e, "Anthropic SSE stream error");
                                Err(e)
                            }
                        };
                        async move { Some(output) }
                    });
                Ok(ProviderStream::new(translated, cancel))
            }
            ServerEventsResponse::Response(resp) => {
                let (parts, resp_body) = resp.into_parts();
                match resp_body.into_bytes().await {
                    Ok(bytes) => {
                        log_anthropic_error_response(
                            "stream",
                            &uri,
                            &body,
                            parts.status,
                            &parts.headers,
                            &bytes,
                        );
                        Err(parse_anthropic_error(parts.status, &parts.headers, &bytes))
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
            .header("anthropic-beta", ANTHROPIC_FILES_BETA)
            .body(body_to_bytes(&body))
            .map_err(|e| LlmProviderError::InvalidResponse {
                detail: format!("failed to build HTTP request: {e}"),
            })?;

        if llm_body_logging_enabled() {
            debug!(
                uri = %uri,
                request_body = %truncate_for_log(&body.to_string(), 4000),
                "sending non-streaming request to Anthropic (body opt-in)"
            );
        } else {
            debug!(
                uri = %uri,
                "sending non-streaming request to Anthropic (body redacted)"
            );
        }

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
            log_anthropic_error_response(
                "complete",
                &uri,
                &body,
                parts.status,
                &parts.headers,
                &bytes,
            );
            return Err(parse_anthropic_error(parts.status, &parts.headers, &bytes));
        }

        let resp: AnthropicMessageResponse = serde_json::from_slice(&bytes).map_err(|e| {
            // Body parse failed on a 2xx — log the wire-level detail with the
            // parse error so we can spot adapter/upstream contract drift.
            log_anthropic_error_response(
                "complete (body parse failed)",
                &uri,
                &body,
                parts.status,
                &parts.headers,
                &bytes,
            );
            tracing::warn!(error = %e, "failed to deserialize Anthropic complete() response");
            parse_anthropic_error(parts.status, &parts.headers, &bytes)
        })?;

        build_complete_result(resp)
    }
}

/// Convert a parsed Anthropic non-streaming response into a `ResponseResult`.
///
/// Rejects responses containing `tool_use` blocks: the non-streaming path
/// can't drive the agentic loop, so silently dropping them would return
/// empty content. Surfacing an error makes the mode misuse explicit.
fn build_complete_result(
    resp: AnthropicMessageResponse,
) -> Result<ResponseResult, LlmProviderError> {
    if resp.content.iter().any(|b| b.block_type == "tool_use") {
        return Err(LlmProviderError::InvalidResponse {
            detail: "Anthropic returned a tool_use block on a non-streaming complete() call; \
                     tools are not supported in this mode"
                .to_owned(),
        });
    }

    let content = resp
        .content
        .iter()
        .filter(|b| b.block_type == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    // Anthropic reports cache tokens separately from `input_tokens`, while
    // OpenAI folds them in. Sum them here so the credits formula sees a
    // provider-agnostic input total. The raw cache breakdown is still
    // preserved for observability.
    let usage = Usage {
        input_tokens: resp.usage.input_tokens
            + resp.usage.cache_read_input_tokens
            + resp.usage.cache_creation_input_tokens,
        output_tokens: resp.usage.output_tokens,
        cache_read_input_tokens: resp.usage.cache_read_input_tokens,
        cache_write_input_tokens: resp.usage.cache_creation_input_tokens,
        reasoning_tokens: 0,
    };

    let raw = serde_json::to_value(&resp).unwrap_or_default();

    Ok(ResponseResult {
        content,
        usage,
        response_id: resp.id,
        citations: vec![],
        raw_response: raw,
    })
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[allow(clippy::str_to_string)]
mod tests {
    use oagw_sdk::sse::{FromServerEvent, ServerEvent};

    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn sse(event: &str, data: &str) -> ServerEvent {
        ServerEvent {
            event: Some(event.to_owned()),
            data: data.to_owned(),
            id: None,
            retry: None,
        }
    }

    // ── FromServerEvent — SSE parsing ─────────────────────────────────────

    #[test]
    fn parse_message_start() {
        let ev = sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_abc","type":"message","role":"assistant","content":[],"model":"claude-3-5-sonnet-20241022","stop_reason":null,"usage":{"input_tokens":42,"output_tokens":1}}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::MessageStart {
                message_id,
                input_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                assert_eq!(message_id, "msg_abc");
                assert_eq!(input_tokens, 42);
                assert_eq!(cache_read_input_tokens, 0);
                assert_eq!(cache_creation_input_tokens, 0);
            }
            _ => panic!("expected MessageStart, got {result:?}"),
        }
    }

    #[test]
    fn parse_message_start_with_cache_tokens() {
        let ev = sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_cache","type":"message","role":"assistant","content":[],"model":"claude-3-5-sonnet-20241022","stop_reason":null,"usage":{"input_tokens":100,"cache_read_input_tokens":500,"cache_creation_input_tokens":250,"output_tokens":1}}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::MessageStart {
                input_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(cache_read_input_tokens, 500);
                assert_eq!(cache_creation_input_tokens, 250);
            }
            _ => panic!("expected MessageStart, got {result:?}"),
        }
    }

    #[test]
    fn parse_content_block_start_text() {
        let ev = sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(
            result,
            AnthropicEvent::ContentBlockStartText { index: 0 }
        ));
    }

    #[test]
    fn parse_content_block_start_tool_use() {
        let ev = sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_xyz","name":"search_knowledge","input":{}}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::ContentBlockStartToolUse { index, id, name } => {
                assert_eq!(index, 1);
                assert_eq!(id, "toolu_xyz");
                assert_eq!(name, "search_knowledge");
            }
            _ => panic!("expected ContentBlockStartToolUse, got {result:?}"),
        }
    }

    #[test]
    fn parse_content_block_start_server_tool_use() {
        let ev = sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"server_tool_use","id":"srvtool_abc","name":"web_search"}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::ContentBlockStartServerToolUse { index, name } => {
                assert_eq!(index, 2);
                assert_eq!(name, "web_search");
            }
            _ => panic!("expected ContentBlockStartServerToolUse, got {result:?}"),
        }
    }

    #[test]
    fn parse_content_block_delta_text() {
        let ev = sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello!"}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::ContentBlockDeltaText { index, text } => {
                assert_eq!(index, 0);
                assert_eq!(text, "Hello!");
            }
            _ => panic!("expected ContentBlockDeltaText, got {result:?}"),
        }
    }

    #[test]
    fn parse_content_block_delta_input_json() {
        let ev = sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\""}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::ContentBlockDeltaInputJson {
                index,
                partial_json,
            } => {
                assert_eq!(index, 1);
                assert_eq!(partial_json, r#"{"query""#);
            }
            _ => panic!("expected ContentBlockDeltaInputJson, got {result:?}"),
        }
    }

    #[test]
    fn parse_content_block_stop() {
        let ev = sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(
            result,
            AnthropicEvent::ContentBlockStop { index: 0 }
        ));
    }

    #[test]
    fn parse_message_delta_end_turn() {
        let ev = sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":37}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::MessageDelta {
                stop_reason,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                assert_eq!(stop_reason, "end_turn");
                assert_eq!(output_tokens, 37);
                assert_eq!(cache_read_input_tokens, 0);
                assert_eq!(cache_creation_input_tokens, 0);
            }
            _ => panic!("expected MessageDelta, got {result:?}"),
        }
    }

    #[test]
    fn parse_message_delta_with_cache_tokens() {
        let ev = sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":37,"cache_read_input_tokens":500,"cache_creation_input_tokens":250}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::MessageDelta {
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                ..
            } => {
                assert_eq!(output_tokens, 37);
                assert_eq!(cache_read_input_tokens, 500);
                assert_eq!(cache_creation_input_tokens, 250);
            }
            _ => panic!("expected MessageDelta, got {result:?}"),
        }
    }

    #[test]
    fn parse_message_stop() {
        let ev = sse("message_stop", r#"{"type":"message_stop"}"#);
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(result, AnthropicEvent::MessageStop));
    }

    #[test]
    fn parse_ping() {
        let ev = sse("ping", r#"{"type":"ping"}"#);
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(result, AnthropicEvent::Ping));
    }

    #[test]
    fn parse_error_event() {
        let ev = sse(
            "error",
            r#"{"error":{"type":"overloaded_error","message":"Service overloaded"}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        match result {
            AnthropicEvent::Error {
                error_type,
                message,
            } => {
                assert_eq!(error_type, "overloaded_error");
                assert_eq!(message, "Service overloaded");
            }
            _ => panic!("expected Error, got {result:?}"),
        }
    }

    #[test]
    fn parse_unknown_event_does_not_fail() {
        let ev = sse("some_future_event", r#"{"foo":"bar"}"#);
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(result, AnthropicEvent::Unknown { .. }));
    }

    #[test]
    fn parse_message_start_missing_usage_defaults_to_zero() {
        let ev = sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_no_usage","type":"message","role":"assistant","content":[],"model":"claude-3"}}"#,
        );
        let result = AnthropicEvent::from_server_event(ev).unwrap();
        assert!(matches!(
            result,
            AnthropicEvent::MessageStart {
                input_tokens: 0,
                ..
            }
        ));
    }

    // ── State machine — translate_anthropic_event ─────────────────────────

    #[test]
    fn state_machine_text_completion_flow() {
        let mut state = AnthropicStreamState::default();

        // message_start
        let ev = AnthropicEvent::MessageStart {
            message_id: "msg_001".to_owned(),
            input_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert!(matches!(
            translate_anthropic_event(&ev, &mut state),
            TranslatedEvent::Skip
        ));
        assert_eq!(state.message_id, "msg_001");
        assert_eq!(state.input_tokens, 10);

        // content_block_start (text)
        let ev = AnthropicEvent::ContentBlockStartText { index: 0 };
        assert!(matches!(
            translate_anthropic_event(&ev, &mut state),
            TranslatedEvent::Skip
        ));

        // two text deltas
        let ev = AnthropicEvent::ContentBlockDeltaText {
            index: 0,
            text: "Hello".to_owned(),
        };
        let out = translate_anthropic_event(&ev, &mut state);
        assert!(
            matches!(out, TranslatedEvent::Sse(ClientSseEvent::Delta { ref content, .. }) if content == "Hello")
        );
        assert_eq!(state.accumulated_text, "Hello");

        let ev = AnthropicEvent::ContentBlockDeltaText {
            index: 0,
            text: " world".to_owned(),
        };
        translate_anthropic_event(&ev, &mut state);
        assert_eq!(state.accumulated_text, "Hello world");

        // message_delta with stop_reason
        let ev = AnthropicEvent::MessageDelta {
            stop_reason: "end_turn".to_owned(),
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert!(matches!(
            translate_anthropic_event(&ev, &mut state),
            TranslatedEvent::Skip
        ));
        assert_eq!(state.stop_reason, "end_turn");
        assert_eq!(state.output_tokens, 5);

        // message_stop → Completed terminal
        let ev = AnthropicEvent::MessageStop;
        let out = translate_anthropic_event(&ev, &mut state);
        match out {
            TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage,
                response_id,
                content,
                ..
            }) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 5);
                assert_eq!(response_id, "msg_001");
                assert_eq!(content, "Hello world");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn state_machine_tool_use_flow() {
        let mut state = AnthropicStreamState::default();

        translate_anthropic_event(
            &AnthropicEvent::MessageStart {
                message_id: "msg_002".to_owned(),
                input_tokens: 20,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            &mut state,
        );

        // tool_use block starts — should emit Tool { Start }
        let out = translate_anthropic_event(
            &AnthropicEvent::ContentBlockStartToolUse {
                index: 0,
                id: "toolu_123".to_owned(),
                name: "search_knowledge".to_owned(),
            },
            &mut state,
        );
        assert!(matches!(
            out,
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "search_knowledge",
                ..
            })
        ));
        assert_eq!(state.tool_use_id, "toolu_123");

        // accumulate JSON input across two deltas
        translate_anthropic_event(
            &AnthropicEvent::ContentBlockDeltaInputJson {
                index: 0,
                partial_json: r#"{"query":"te"#.to_owned(),
            },
            &mut state,
        );
        translate_anthropic_event(
            &AnthropicEvent::ContentBlockDeltaInputJson {
                index: 0,
                partial_json: r#"st"}"#.to_owned(),
            },
            &mut state,
        );
        assert_eq!(state.tool_input_json, r#"{"query":"test"}"#);

        // message_delta with tool_use stop reason
        translate_anthropic_event(
            &AnthropicEvent::MessageDelta {
                stop_reason: "tool_use".to_owned(),
                output_tokens: 8,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            &mut state,
        );

        // message_stop → ToolUse terminal
        let out = translate_anthropic_event(&AnthropicEvent::MessageStop, &mut state);
        match out {
            TranslatedEvent::Terminal(TerminalOutcome::ToolUse {
                tool_use_id,
                name,
                input,
            }) => {
                assert_eq!(tool_use_id, "toolu_123");
                assert_eq!(name, "search_knowledge");
                assert_eq!(input["query"], "test");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn state_machine_max_tokens_produces_incomplete() {
        let mut state = AnthropicStreamState::default();
        translate_anthropic_event(
            &AnthropicEvent::MessageStart {
                message_id: "msg_003".to_owned(),
                input_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            &mut state,
        );
        translate_anthropic_event(
            &AnthropicEvent::ContentBlockDeltaText {
                index: 0,
                text: "partial".to_owned(),
            },
            &mut state,
        );
        translate_anthropic_event(
            &AnthropicEvent::MessageDelta {
                stop_reason: "max_tokens".to_owned(),
                output_tokens: 3,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            &mut state,
        );
        let out = translate_anthropic_event(&AnthropicEvent::MessageStop, &mut state);
        assert!(matches!(
            out,
            TranslatedEvent::Terminal(TerminalOutcome::Incomplete { ref reason, .. })
            if reason == "max_tokens"
        ));
    }

    #[test]
    fn build_complete_result_rejects_tool_use_block() {
        let resp: AnthropicMessageResponse = serde_json::from_value(serde_json::json!({
            "id": "msg_tool",
            "content": [
                { "type": "text", "text": "thinking..." },
                { "type": "tool_use" },
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 },
        }))
        .unwrap();

        match build_complete_result(resp) {
            Err(LlmProviderError::InvalidResponse { detail }) => {
                assert!(detail.contains("tool_use"));
                assert!(detail.contains("complete()"));
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    #[test]
    fn build_complete_result_text_only_succeeds_with_normalized_usage() {
        let resp: AnthropicMessageResponse = serde_json::from_value(serde_json::json!({
            "id": "msg_text",
            "content": [
                { "type": "text", "text": "hello " },
                { "type": "text", "text": "world" },
            ],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 7,
                "cache_read_input_tokens": 500,
                "cache_creation_input_tokens": 250,
            },
        }))
        .unwrap();

        let result = build_complete_result(resp).unwrap();
        assert_eq!(result.content, "hello world");
        assert_eq!(result.response_id, "msg_text");
        assert_eq!(result.usage.input_tokens, 100 + 500 + 250);
        assert_eq!(result.usage.output_tokens, 7);
        assert_eq!(result.usage.cache_read_input_tokens, 500);
        assert_eq!(result.usage.cache_write_input_tokens, 250);
    }

    #[test]
    fn state_machine_propagates_cache_tokens_to_terminal_usage() {
        let mut state = AnthropicStreamState::default();

        // message_start carries cache totals (set at request time).
        translate_anthropic_event(
            &AnthropicEvent::MessageStart {
                message_id: "msg_cache".to_owned(),
                input_tokens: 100,
                cache_read_input_tokens: 500,
                cache_creation_input_tokens: 250,
            },
            &mut state,
        );
        translate_anthropic_event(
            &AnthropicEvent::ContentBlockDeltaText {
                index: 0,
                text: "ok".to_owned(),
            },
            &mut state,
        );
        // message_delta omits cache fields (zero from serde default) — should
        // not clobber the values captured from message_start.
        translate_anthropic_event(
            &AnthropicEvent::MessageDelta {
                stop_reason: "end_turn".to_owned(),
                output_tokens: 7,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            &mut state,
        );

        let out = translate_anthropic_event(&AnthropicEvent::MessageStop, &mut state);
        match out {
            TranslatedEvent::Terminal(TerminalOutcome::Completed { usage, .. }) => {
                // input_tokens is normalized to wire input + cache_read +
                // cache_creation so the credits formula matches OpenAI.
                assert_eq!(usage.input_tokens, 100 + 500 + 250);
                assert_eq!(usage.output_tokens, 7);
                assert_eq!(usage.cache_read_input_tokens, 500);
                assert_eq!(usage.cache_write_input_tokens, 250);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn state_machine_server_tool_emits_start_and_done_events() {
        let mut state = AnthropicStreamState::default();

        let start = translate_anthropic_event(
            &AnthropicEvent::ContentBlockStartServerToolUse {
                index: 0,
                name: "web_search".to_owned(),
            },
            &mut state,
        );
        assert!(matches!(
            start,
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "web_search",
                ..
            })
        ));
        assert_eq!(state.current_server_tool, Some("web_search"));

        let stop =
            translate_anthropic_event(&AnthropicEvent::ContentBlockStop { index: 0 }, &mut state);
        assert!(matches!(
            stop,
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name: "web_search",
                ..
            })
        ));
        // The Done event consumed the tracked name so subsequent stops Skip.
        assert_eq!(state.current_server_tool, None);
        let next_stop =
            translate_anthropic_event(&AnthropicEvent::ContentBlockStop { index: 1 }, &mut state);
        assert!(matches!(next_stop, TranslatedEvent::Skip));
    }

    #[test]
    fn state_machine_unknown_server_tool_skipped() {
        let mut state = AnthropicStreamState::default();
        let out = translate_anthropic_event(
            &AnthropicEvent::ContentBlockStartServerToolUse {
                index: 0,
                name: "future_tool_v2".to_owned(),
            },
            &mut state,
        );
        assert!(matches!(out, TranslatedEvent::Skip));
        assert_eq!(state.current_server_tool, None);
    }

    #[test]
    fn state_machine_error_event_produces_failed() {
        let mut state = AnthropicStreamState::default();
        let out = translate_anthropic_event(
            &AnthropicEvent::Error {
                error_type: "overloaded_error".to_owned(),
                message: "Service is overloaded".to_owned(),
            },
            &mut state,
        );
        match out {
            TranslatedEvent::Terminal(TerminalOutcome::Failed { error, .. }) => match error {
                LlmProviderError::ProviderError { code, .. } => {
                    assert_eq!(code, "overloaded_error");
                }
                _ => panic!("expected ProviderError"),
            },
            other => panic!("expected Failed terminal, got {other:?}"),
        }
    }

    // ── parse_anthropic_error ─────────────────────────────────────────────

    #[test]
    fn parse_anthropic_error_valid_json() {
        let bytes =
            br#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens is required"}}"#;
        let err = parse_anthropic_error(
            http::StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            bytes,
        );
        match err {
            LlmProviderError::ProviderError { code, message, .. } => {
                assert_eq!(code, "invalid_request_error");
                // New behavior: surfaced message wraps the upstream code and HTTP status
                // around the original message so the UI shows actionable detail.
                assert!(message.contains("invalid_request_error"));
                assert!(message.contains("max_tokens is required"));
                assert!(message.contains("400"));
            }
            _ => panic!("expected ProviderError"),
        }
    }

    #[test]
    fn parse_anthropic_error_rate_limit_routes_to_rate_limited() {
        let bytes = br#"{"type":"error","error":{"type":"rate_limit_error","message":"Error"}}"#;
        let mut headers = http::HeaderMap::new();
        headers.insert("retry-after", http::HeaderValue::from_static("23"));
        let err = parse_anthropic_error(http::StatusCode::TOO_MANY_REQUESTS, &headers, bytes);
        match err {
            LlmProviderError::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(23));
            }
            _ => panic!("expected RateLimited"),
        }
    }

    #[test]
    fn parse_anthropic_error_unparseable_falls_back_to_invalid_response() {
        let bytes = b"not json at all";
        let err = parse_anthropic_error(
            http::StatusCode::BAD_GATEWAY,
            &http::HeaderMap::new(),
            bytes,
        );
        assert!(matches!(err, LlmProviderError::InvalidResponse { .. }));
    }

    // ── parse_tool_result_content ─────────────────────────────────────────

    #[test]
    fn parse_tool_result_content_wraps_plain_string() {
        let result = parse_tool_result_content("hello world");
        assert_eq!(
            result,
            serde_json::json!([{"type": "text", "text": "hello world"}])
        );
    }

    #[test]
    fn parse_tool_result_content_passes_through_typed_array() {
        let output = r#"[{"type":"search_result","source":"s3://bucket/file.pdf","title":"Doc","content":[{"type":"text","text":"relevant text"}]}]"#;
        let result = parse_tool_result_content(output);
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "search_result");
        assert_eq!(arr[0]["source"], "s3://bucket/file.pdf");
    }

    #[test]
    fn parse_tool_result_content_wraps_array_without_type_fields() {
        let output = r#"[{"value": 1}, {"value": 2}]"#;
        let result = parse_tool_result_content(output);
        assert_eq!(
            result,
            serde_json::json!([{"type": "text", "text": output}])
        );
    }

    #[test]
    fn parse_tool_result_content_wraps_empty_string() {
        let result = parse_tool_result_content("");
        assert_eq!(result, serde_json::json!([{"type": "text", "text": ""}]));
    }

    // ── convert_raw_input_items ───────────────────────────────────────────

    #[test]
    fn convert_raw_input_items_produces_anthropic_messages() {
        let items = vec![
            serde_json::json!({
                "type": "function_call",
                "call_id": "toolu_abc",
                "name": "search_knowledge",
                "arguments": r#"{"query":"test query"}"#
            }),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "toolu_abc",
                "output": "some result"
            }),
        ];
        let messages = convert_raw_input_items(&items);
        assert_eq!(messages.len(), 2);

        let assistant = &messages[0];
        assert_eq!(assistant["role"], "assistant");
        let content = assistant["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "toolu_abc");
        assert_eq!(content[0]["name"], "search_knowledge");
        assert_eq!(content[0]["input"]["query"], "test query");

        let user = &messages[1];
        assert_eq!(user["role"], "user");
        let ucontent = user["content"].as_array().unwrap();
        assert_eq!(ucontent[0]["type"], "tool_result");
        assert_eq!(ucontent[0]["tool_use_id"], "toolu_abc");
    }

    #[test]
    fn convert_raw_input_items_empty_returns_empty() {
        assert_eq!(convert_raw_input_items(&[]).len(), 0);
    }

    #[test]
    fn convert_raw_input_items_unknown_type_ignored() {
        let items = vec![serde_json::json!({"type": "some_future_type", "data": "x"})];
        assert_eq!(convert_raw_input_items(&items).len(), 0);
    }

    #[test]
    fn convert_raw_input_items_multiple_calls_grouped() {
        let items = vec![
            serde_json::json!({"type":"function_call","call_id":"c1","name":"search_knowledge","arguments":"{}"}),
            serde_json::json!({"type":"function_call","call_id":"c2","name":"search_knowledge","arguments":"{}"}),
            serde_json::json!({"type":"function_call_output","call_id":"c1","output":"result1"}),
            serde_json::json!({"type":"function_call_output","call_id":"c2","output":"result2"}),
        ];
        let messages = convert_raw_input_items(&items);
        // Both function_calls → one assistant message with 2 tool_use blocks
        // Both function_call_outputs → one user message with 2 tool_result blocks
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn convert_raw_input_items_search_result_content_forwarded_verbatim() {
        let search_result_json = r#"[{"type":"search_result","source":"uri","title":"T","content":[{"type":"text","text":"chunk"}]}]"#;
        let items = vec![
            serde_json::json!({"type":"function_call","call_id":"c1","name":"search_knowledge","arguments":"{}"}),
            serde_json::json!({"type":"function_call_output","call_id":"c1","output": search_result_json}),
        ];
        let messages = convert_raw_input_items(&items);
        let user_content = &messages[1]["content"][0];
        assert_eq!(user_content["type"], "tool_result");
        // content should be an array of search_result blocks, not a text wrapper
        let inner = user_content["content"].as_array().unwrap();
        assert_eq!(inner[0]["type"], "search_result");
    }

    #[test]
    fn convert_raw_input_items_trailing_function_call_is_flushed() {
        // Single function_call with no following function_call_output —
        // the trailing flush path at the end of the function must emit the
        // assistant message. Without this flush the model would not see its
        // own tool call on replay.
        let items = vec![serde_json::json!({
            "type": "function_call",
            "call_id": "toolu_solo",
            "name": "search_knowledge",
            "arguments": r#"{"query":"q"}"#,
        })];
        let messages = convert_raw_input_items(&items);
        assert_eq!(messages.len(), 1, "trailing tool_use must be flushed");
        assert_eq!(messages[0]["role"], "assistant");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "toolu_solo");
        assert_eq!(content[0]["input"]["query"], "q");
    }

    #[test]
    fn convert_raw_input_items_orphan_output_does_not_panic() {
        // Degenerate input: a function_call_output with no preceding
        // function_call. The conversion must not panic; the orphan output is
        // emitted as a user message so subsequent items still process.
        let items = vec![serde_json::json!({
            "type": "function_call_output",
            "call_id": "toolu_orphan",
            "output": "orphan result",
        })];
        let messages = convert_raw_input_items(&items);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_orphan");
    }

    #[test]
    fn convert_raw_input_items_missing_call_id_and_name_fall_back_to_empty() {
        // Documents the current `unwrap_or("")` fallback: missing `call_id`
        // and `name` become empty strings. Anthropic will then reject the
        // request at the API level — this test pins the contract so any
        // future change (e.g. drop the block, or hard-fail here) is a
        // deliberate decision rather than a silent drift.
        let items = vec![
            serde_json::json!({"type": "function_call", "arguments": "{}"}),
            serde_json::json!({"type": "function_call_output", "output": "x"}),
        ];
        let messages = convert_raw_input_items(&items);
        assert_eq!(messages.len(), 2);

        let tool_use = &messages[0]["content"][0];
        assert_eq!(tool_use["type"], "tool_use");
        assert_eq!(tool_use["id"], "");
        assert_eq!(tool_use["name"], "");

        let tool_result = &messages[1]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        assert_eq!(tool_result["tool_use_id"], "");
    }

    // ── build_request_body ────────────────────────────────────────────────

    #[test]
    fn request_body_includes_required_max_tokens() {
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022").build_streaming();
        let body = build_request_body(&req, true);
        assert!(body.get("max_tokens").is_some());
    }

    #[test]
    fn request_body_uses_explicit_max_tokens() {
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .max_output_tokens(1024)
            .build_streaming();
        let body = build_request_body(&req, true);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn request_body_system_at_top_level() {
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .system_instructions("Be helpful.")
            .build_streaming();
        let body = build_request_body(&req, true);
        assert_eq!(body["system"], "Be helpful.");
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn request_body_system_role_excluded_from_messages() {
        use crate::domain::llm::ContentPart;
        use crate::infra::llm::{LlmMessage, Role, llm_request};
        let system_msg = LlmMessage {
            role: Role::System,
            content: vec![ContentPart::Text {
                text: "You are helpful.".to_owned(),
            }],
        };
        let user_msg = LlmMessage::user("Hello");
        let req = llm_request("claude-3-5-sonnet-20241022")
            .messages(vec![system_msg, user_msg])
            .build_streaming();
        let body = build_request_body(&req, true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn request_body_user_and_assistant_messages() {
        use crate::infra::llm::{LlmMessage, llm_request};
        let req = llm_request("claude-3-5-sonnet-20241022")
            .messages(vec![
                LlmMessage::user("Hello"),
                LlmMessage::assistant("Hi!"),
            ])
            .build_streaming();
        let body = build_request_body(&req, true);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["type"], "text");
        assert_eq!(msgs[0]["content"][0]["text"], "Hello");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn request_body_image_content_uses_anthropic_file_id_substitution() {
        use crate::domain::llm::{ContentPart, LlmMessage, Role};
        use crate::infra::llm::llm_request;
        let msg = LlmMessage {
            role: Role::User,
            content: vec![ContentPart::Image {
                file_id: "azure_primary_id".to_owned(),
            }],
        };
        // The adapter substitutes the primary `provider_file_id` with the
        // Anthropic file id from the lookup map populated at request time
        // from successful parallel uploads.
        let mut map = std::collections::HashMap::new();
        map.insert("azure_primary_id".to_owned(), "file_011_anth".to_owned());

        let req = llm_request("claude-3-5-sonnet-20241022")
            .messages(vec![msg])
            .anthropic_file_ids(map)
            .build_streaming();
        let body = build_request_body(&req, true);
        let content = &body["messages"][0]["content"][0];
        assert_eq!(content["type"], "image");
        assert_eq!(content["source"]["type"], "file");
        assert_eq!(content["source"]["file_id"], "file_011_anth");
    }

    #[test]
    fn request_body_image_block_dropped_when_anthropic_file_id_missing() {
        use crate::domain::llm::{ContentPart, LlmMessage, Role};
        use crate::infra::llm::llm_request;
        // Image referenced by primary id, but the parallel upload failed —
        // the map is empty. Adapter must skip the image block rather than
        // send a primary id that Anthropic cannot resolve (would 4xx).
        let msg = LlmMessage {
            role: Role::User,
            content: vec![
                ContentPart::Text {
                    text: "Describe this".to_owned(),
                },
                ContentPart::Image {
                    file_id: "azure_primary_id".to_owned(),
                },
            ],
        };
        let req = llm_request("claude-3-5-sonnet-20241022")
            .messages(vec![msg])
            .build_streaming();
        let body = build_request_body(&req, true);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "image block should be dropped");
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn request_body_function_tool_uses_input_schema() {
        use crate::domain::llm::LlmTool;
        use crate::infra::llm::llm_request;
        let tool = LlmTool::Function {
            name: "search_knowledge".to_owned(),
            description: "Search knowledge base".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let req = llm_request("claude-3-5-sonnet-20241022")
            .tool(tool)
            .build_streaming();
        let body = build_request_body(&req, true);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search_knowledge");
        assert!(tools[0].get("input_schema").is_some());
        assert!(tools[0].get("parameters").is_none());
        assert!(tools[0].get("type").is_none());
    }

    #[test]
    fn request_body_file_search_tool_dropped() {
        use crate::domain::llm::LlmTool;
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .tool(LlmTool::FileSearch {
                vector_store_ids: vec!["vs_1".to_owned()],
                filters: None,
                max_num_results: None,
            })
            .build_streaming();
        let body = build_request_body(&req, true);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn request_body_web_search_uses_native_server_tool() {
        use crate::domain::llm::{LlmTool, WebSearchContextSize};
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .tool(LlmTool::WebSearch {
                search_context_size: WebSearchContextSize::Medium,
            })
            .build_streaming();
        let body = build_request_body(&req, true);
        let tools = body["tools"].as_array().expect("tools array present");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "web_search_20260209");
        assert_eq!(tools[0]["name"], "web_search");
    }

    #[test]
    fn request_body_code_interpreter_uses_native_server_tool() {
        use crate::domain::llm::LlmTool;
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .tool(LlmTool::CodeInterpreter { file_ids: vec![] })
            .build_streaming();
        let body = build_request_body(&req, true);
        let tools = body["tools"].as_array().expect("tools array present");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "code_execution_20250825");
        assert_eq!(tools[0]["name"], "code_execution");
    }

    #[test]
    fn request_body_metadata_user_id_format() {
        use crate::infra::llm::llm_request;
        let req = llm_request("claude-3-5-sonnet-20241022")
            .user_identity("tenant-1", "user-2")
            .build_streaming();
        let body = build_request_body(&req, true);
        assert_eq!(body["metadata"]["user_id"], "tenant-1:user-2");
    }

    #[test]
    fn request_body_raw_input_items_appended_as_messages() {
        use crate::infra::llm::llm_request;
        let items = vec![
            serde_json::json!({"type":"function_call","call_id":"c1","name":"search_knowledge","arguments":"{}"}),
            serde_json::json!({"type":"function_call_output","call_id":"c1","output":"result"}),
        ];
        let req = llm_request("claude-3-5-sonnet-20241022")
            .raw_input_items(items)
            .build_streaming();
        let body = build_request_body(&req, true);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[1]["role"], "user");
    }
}
