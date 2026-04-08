//! `OpenAI` Responses API adapter (`/v1/responses`).
//!
//! Implements [`LlmProvider`] by converting [`LlmRequest`] to the Responses
//! API wire format, proxying through OAGW, parsing SSE events, and
//! translating them to the shared `TranslatedEvent` contract.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use modkit_security::SecurityContext;
use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::llm::request::{ContentPart as MessageContentPart, FileSearchFilter, LlmTool};
use crate::infra::llm::{
    Citation, CitationSource, ClientSseEvent, LlmProviderError, LlmRequest, NonStreaming,
    ProviderStream, RawDetail, ResponseResult, Streaming, TerminalOutcome, TextSpan, ToolPhase,
    TranslatedEvent, Usage,
};

/// Safety cap for code-interpreter log output forwarded to clients via SSE.
const MAX_CODE_INTERPRETER_OUTPUT_CHARS: usize = 8_192;

// ════════════════════════════════════════════════════════════════════════════
// Provider event types (internal)
// ════════════════════════════════════════════════════════════════════════════

/// Raw provider SSE event from the Responses API.
#[derive(Debug, Clone)]
enum ProviderEvent {
    ResponseOutputTextDelta {
        delta: String,
    },
    ResponseOutputTextDone {
        #[allow(dead_code)]
        text: String,
    },
    ResponseFileSearchCallSearching,
    ResponseFileSearchCallCompleted {
        results: Vec<FileSearchResult>,
    },
    ResponseWebSearchCallSearching,
    ResponseWebSearchCallCompleted,
    ResponseCodeInterpreterCallInProgress,
    ResponseCodeInterpreterCallCompleted {
        /// Concatenated text from all `logs` output items.
        output: String,
    },
    ResponseCompleted {
        response: ResponseObject,
    },
    ResponseFailed {
        error: ProviderErrorPayload,
    },
    ResponseIncomplete {
        reason: String,
    },
    Unknown {
        #[allow(dead_code)]
        event_name: String,
    },
}

// ════════════════════════════════════════════════════════════════════════════
// Response data types
// ════════════════════════════════════════════════════════════════════════════

/// Raw provider response object (Responses API schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseObject {
    pub id: String,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    pub usage: RawUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Provider error payload from `response.failed` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProviderErrorPayload {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// `OpenAI` wraps errors in `{"error": {...}}`.
#[derive(Deserialize)]
struct ProviderErrorEnvelope {
    error: ProviderErrorPayload,
}

/// Parse an error response body, handling both `{"error":{...}}` (`OpenAI`)
/// and flat `{"code":"...","message":"..."}` shapes.
fn parse_error_response(bytes: &[u8]) -> LlmProviderError {
    // Try OpenAI envelope first: {"error": {"message": "...", "code": "..."}}
    if let Ok(envelope) = serde_json::from_slice::<ProviderErrorEnvelope>(bytes) {
        let raw = envelope.error.message.clone();
        return LlmProviderError::ProviderError {
            code: envelope.error.code,
            message: crate::infra::llm::sanitize_provider_message(&envelope.error.message),
            raw_detail: Some(RawDetail(raw)),
        };
    }

    // Try flat shape: {"code": "...", "message": "..."}
    if let Ok(payload) = serde_json::from_slice::<ProviderErrorPayload>(bytes)
        && (!payload.message.is_empty() || !payload.code.is_empty())
    {
        let raw = payload.message.clone();
        return LlmProviderError::ProviderError {
            code: payload.code,
            message: crate::infra::llm::sanitize_provider_message(&payload.message),
            raw_detail: Some(RawDetail(raw)),
        };
    }

    // Fallback: unparseable body
    let body_str = String::from_utf8_lossy(bytes);
    let snippet = crate::infra::llm::sanitize_provider_message(
        &body_str.chars().take(200).collect::<String>(),
    );
    LlmProviderError::InvalidResponse {
        detail: format!("non-SSE response with unparseable body: {snippet}"),
    }
}

/// File search result from `response.file_search_call.completed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResult {
    #[serde(default)]
    pub file_id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub text: String,
}

/// An output item from the provider response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputItem {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub content: Vec<ResponseContentPart>,
}

/// A content part within an output item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseContentPart {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

/// An annotation on a content part (citation source).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub start_index: Option<usize>,
    #[serde(default)]
    pub end_index: Option<usize>,
    #[serde(default)]
    pub text: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════════
// SSE deserialization helpers
// ════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct TextDeltaData {
    delta: String,
}

#[derive(Deserialize)]
struct TextDoneData {
    text: String,
}

#[derive(Deserialize)]
struct FileSearchCompletedData {
    #[serde(default)]
    results: Vec<FileSearchResult>,
}

#[derive(Deserialize)]
struct ResponseCompletedData {
    response: ResponseObject,
}

#[derive(Deserialize)]
struct ResponseFailedData {
    #[serde(default)]
    error: ProviderErrorPayload,
}

#[derive(Deserialize)]
struct ResponseIncompleteData {
    #[serde(default)]
    reason: String,
}

/// A single output item from `response.code_interpreter_call.completed`.
#[derive(Deserialize)]
struct CodeInterpreterOutputItem {
    #[serde(default, rename = "type")]
    output_type: String,
    /// Present when `output_type == "logs"`.
    #[serde(default)]
    logs: String,
}

#[derive(Deserialize)]
struct CodeInterpreterCompletedData {
    #[serde(default)]
    outputs: Vec<CodeInterpreterOutputItem>,
}

// ════════════════════════════════════════════════════════════════════════════
// FromServerEvent
// ════════════════════════════════════════════════════════════════════════════

impl FromServerEvent for ProviderEvent {
    fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
        let event_name = event.event.as_deref().unwrap_or("message");

        match event_name {
            "response.output_text.delta" => {
                let data: TextDeltaData = serde_json::from_str(&event.data).map_err(|e| {
                    StreamingError::ServerEventsParse {
                        detail: format!("failed to parse text delta: {e}"),
                    }
                })?;
                Ok(ProviderEvent::ResponseOutputTextDelta { delta: data.delta })
            }

            "response.output_text.done" => {
                let data: TextDoneData = serde_json::from_str(&event.data).map_err(|e| {
                    StreamingError::ServerEventsParse {
                        detail: format!("failed to parse text done: {e}"),
                    }
                })?;
                Ok(ProviderEvent::ResponseOutputTextDone { text: data.text })
            }

            "response.file_search_call.searching" => {
                Ok(ProviderEvent::ResponseFileSearchCallSearching)
            }

            "response.file_search_call.completed" => {
                let data: FileSearchCompletedData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse file search completed: {e}"),
                        }
                    })?;
                Ok(ProviderEvent::ResponseFileSearchCallCompleted {
                    results: data.results,
                })
            }

            "response.web_search_call.searching" => {
                Ok(ProviderEvent::ResponseWebSearchCallSearching)
            }

            "response.web_search_call.completed" => {
                Ok(ProviderEvent::ResponseWebSearchCallCompleted)
            }

            "response.code_interpreter_call.in_progress" => {
                Ok(ProviderEvent::ResponseCodeInterpreterCallInProgress)
            }

            "response.code_interpreter_call.interpreting" => {
                // Intermediate event — no client-visible update needed.
                Ok(ProviderEvent::Unknown {
                    event_name: event_name.to_owned(),
                })
            }

            "response.code_interpreter_call.completed" => {
                let data: CodeInterpreterCompletedData = serde_json::from_str(&event.data)
                    .map_err(|e| StreamingError::ServerEventsParse {
                        detail: format!("failed to parse code interpreter completed: {e}"),
                    })?;
                let mut output = data
                    .outputs
                    .into_iter()
                    .filter(|o| o.output_type == "logs")
                    .map(|o| o.logs)
                    .collect::<Vec<_>>()
                    .join("\n");
                if output.chars().count() > MAX_CODE_INTERPRETER_OUTPUT_CHARS {
                    output = output
                        .chars()
                        .take(MAX_CODE_INTERPRETER_OUTPUT_CHARS)
                        .collect::<String>();
                    output.push_str("...[truncated]");
                }
                Ok(ProviderEvent::ResponseCodeInterpreterCallCompleted { output })
            }

            "response.completed" => {
                let data: ResponseCompletedData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse response completed: {e}"),
                        }
                    })?;
                Ok(ProviderEvent::ResponseCompleted {
                    response: data.response,
                })
            }

            "response.failed" => {
                let data: ResponseFailedData = serde_json::from_str(&event.data).map_err(|e| {
                    StreamingError::ServerEventsParse {
                        detail: format!("failed to parse response failed: {e}"),
                    }
                })?;
                Ok(ProviderEvent::ResponseFailed { error: data.error })
            }

            "response.incomplete" => {
                let data: ResponseIncompleteData =
                    serde_json::from_str(&event.data).map_err(|e| {
                        StreamingError::ServerEventsParse {
                            detail: format!("failed to parse response incomplete: {e}"),
                        }
                    })?;
                Ok(ProviderEvent::ResponseIncomplete {
                    reason: data.reason,
                })
            }

            "error" => {
                // OpenAI sends `event: error` with the actual error details.
                // Try nested `{"error": {...}}` shape first, then flat shape.
                let sanitized_data = crate::infra::llm::sanitize_provider_message(&event.data);
                tracing::warn!(data = %sanitized_data, "provider error SSE event");
                let error =
                    if let Ok(data) = serde_json::from_str::<ResponseFailedData>(&event.data) {
                        data.error
                    } else if let Ok(payload) =
                        serde_json::from_str::<ProviderErrorPayload>(&event.data)
                    {
                        payload
                    } else {
                        ProviderErrorPayload {
                            code: String::new(),
                            message: event.data.clone(),
                        }
                    };
                Ok(ProviderEvent::ResponseFailed { error })
            }

            other => {
                debug!(
                    event_name = other,
                    "ignoring unhandled provider lifecycle event"
                );
                Ok(ProviderEvent::Unknown {
                    event_name: other.to_owned(),
                })
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Event translation + citation extraction
// ════════════════════════════════════════════════════════════════════════════

/// Translate a raw Responses API event into the shared contract.
fn translate_provider_event(event: &ProviderEvent, accumulated_text: &str) -> TranslatedEvent {
    match event {
        ProviderEvent::ResponseOutputTextDelta { delta } => {
            TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: delta.clone(),
            })
        }

        ProviderEvent::ResponseOutputTextDone { .. } | ProviderEvent::Unknown { .. } => {
            TranslatedEvent::Skip
        }

        ProviderEvent::ResponseFileSearchCallSearching => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "file_search",
                details: serde_json::json!({}),
            })
        }

        ProviderEvent::ResponseFileSearchCallCompleted { results } => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name: "file_search",
                details: serde_json::json!({ "files_searched": results.len() }),
            })
        }

        ProviderEvent::ResponseWebSearchCallSearching => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "web_search",
                details: serde_json::json!({}),
            })
        }

        ProviderEvent::ResponseWebSearchCallCompleted => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name: "web_search",
                details: serde_json::json!({}),
            })
        }

        ProviderEvent::ResponseCodeInterpreterCallInProgress => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "code_interpreter",
                details: serde_json::json!({}),
            })
        }

        ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
            TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name: "code_interpreter",
                details: serde_json::json!({ "output": output }),
            })
        }

        ProviderEvent::ResponseCompleted { response } => {
            let citations = extract_citations(response, accumulated_text);
            let usage = Usage {
                input_tokens: response.usage.input_tokens,
                output_tokens: response.usage.output_tokens,
            };
            let raw = serde_json::to_value(response).unwrap_or_default();
            TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage,
                response_id: response.id.clone(),
                content: accumulated_text.to_owned(),
                citations,
                raw_response: raw,
            })
        }

        ProviderEvent::ResponseFailed { error } => {
            let sanitized = crate::infra::llm::sanitize_provider_message(&error.message);
            TranslatedEvent::Terminal(TerminalOutcome::Failed {
                error: LlmProviderError::ProviderError {
                    code: error.code.clone(),
                    message: sanitized,
                    raw_detail: Some(RawDetail(error.message.clone())),
                },
                usage: None,
                partial_content: accumulated_text.to_owned(),
            })
        }

        ProviderEvent::ResponseIncomplete { reason } => {
            TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
                reason: reason.clone(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                },
                partial_content: accumulated_text.to_owned(),
            })
        }
    }
}

/// Extract citations from a `ResponseCompleted`'s output annotations.
fn extract_citations(response: &ResponseObject, accumulated_text: &str) -> Vec<Citation> {
    let mut citations = Vec::new();

    for output_item in &response.output {
        for content_part in &output_item.content {
            for annotation in &content_part.annotations {
                let citation = match annotation.r#type.as_str() {
                    "file_citation" | "url_citation" => {
                        let snippet = annotation
                            .text
                            .clone()
                            .or_else(|| {
                                annotation.start_index.zip(annotation.end_index).and_then(
                                    |(start, end)| {
                                        accumulated_text.get(start..end).map(ToOwned::to_owned)
                                    },
                                )
                            })
                            .unwrap_or_default();
                        let span = match (annotation.start_index, annotation.end_index) {
                            (Some(start), Some(end)) => Some(TextSpan { start, end }),
                            _ => None,
                        };

                        let is_file = annotation.r#type == "file_citation";
                        Citation {
                            source: if is_file {
                                CitationSource::File
                            } else {
                                CitationSource::Web
                            },
                            title: annotation.title.clone(),
                            url: if is_file {
                                None
                            } else {
                                annotation.url.clone()
                            },
                            attachment_id: if is_file {
                                annotation.file_id.clone()
                            } else {
                                None
                            },
                            snippet,
                            score: None,
                            span,
                        }
                    }
                    _ => continue,
                };
                citations.push(citation);
            }
        }
    }

    citations
}

// ════════════════════════════════════════════════════════════════════════════
// LlmRequest → Responses API conversion
// ════════════════════════════════════════════════════════════════════════════

/// Build the Responses API JSON body from an `LlmRequest`.
fn build_request_body<M>(request: &LlmRequest<M>, stream: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "stream": stream,
        "store": false,
        "previous_response_id": null,
    });

    body["model"] = serde_json::json!(&request.model);

    // Build Responses API input array — each LlmMessage becomes a
    // {"type": "message", "role": "…", "content": [{type: "input_text", …}, …]}
    let input: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter(|msg| msg.role != crate::infra::llm::request::Role::System)
        .map(|msg| {
            let role = match msg.role {
                crate::infra::llm::request::Role::User => "user",
                crate::infra::llm::request::Role::Assistant => "assistant",
                // System messages are handled via the `instructions` field above.
                crate::infra::llm::request::Role::System => unreachable!(),
            };
            let is_assistant = role == "assistant";
            let content: Vec<serde_json::Value> = msg
                .content
                .iter()
                .map(|part| match part {
                    MessageContentPart::Text { text } if is_assistant => serde_json::json!({
                        "type": "output_text",
                        "text": text
                    }),
                    MessageContentPart::Text { text } => serde_json::json!({
                        "type": "input_text",
                        "text": text
                    }),
                    MessageContentPart::Image { file_id } => serde_json::json!({
                        "type": "input_image",
                        "file_id": file_id
                    }),
                })
                .collect();
            serde_json::json!({
                "type": "message",
                "role": role,
                "content": content
            })
        })
        .collect();
    if !input.is_empty() {
        body["input"] = serde_json::Value::Array(input);
    }

    if let Some(ref instructions) = request.system_instructions {
        body["instructions"] = serde_json::json!(instructions);
    }

    if let Some(max_tokens) = request.max_output_tokens {
        body["max_output_tokens"] = serde_json::json!(max_tokens);
    }

    if let Some(max_calls) = request.max_tool_calls {
        body["max_tool_calls"] = serde_json::json!(max_calls);
    }

    // User field: "{tenant_id}:{user_id}"
    if let Some(ref identity) = request.user_identity {
        body["user"] = serde_json::json!(format!("{}:{}", identity.tenant_id, identity.user_id));
    }

    if let Some(ref metadata) = request.metadata {
        body["metadata"] = serde_json::to_value(metadata).unwrap_or_default();
    }

    // Map tools: FileSearch → file_search, WebSearch → web_search, Function → drop
    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .filter_map(|tool| match tool {
            LlmTool::FileSearch {
                vector_store_ids,
                filters,
                max_num_results,
            } => {
                // Responses API uses flat format (vector_store_ids at top level),
                // NOT nested inside a "file_search" key.
                let mut tool = serde_json::json!({
                    "type": "file_search",
                    "vector_store_ids": vector_store_ids
                });
                if let Some(f) = filters {
                    tool["filters"] = serialize_file_search_filter(f);
                }
                if let Some(n) = max_num_results {
                    tool["max_num_results"] = serde_json::json!(n);
                }
                Some(tool)
            }
            LlmTool::WebSearch {
                search_context_size,
            } => Some(serde_json::json!({
                "type": "web_search",
                "search_context_size": search_context_size
            })),
            LlmTool::CodeInterpreter { file_ids } => Some(serde_json::json!({
                "type": "code_interpreter",
                "container": {
                    "type": "auto",
                    "file_ids": file_ids
                }
            })),
            LlmTool::Function { name, .. } => {
                debug!(tool_name = %name, "Function tool not supported by Responses API, dropping");
                None
            }
        })
        .collect();
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
    }

    // Merge additional params
    if let Some(ref extra) = request.additional_params
        && let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object())
    {
        for (k, v) in extra_obj {
            body_obj.insert(k.clone(), v.clone());
        }
    }

    body
}

/// Serialize a `FileSearchFilter` to the provider's wire JSON format.
fn serialize_file_search_filter(filter: &FileSearchFilter) -> serde_json::Value {
    match filter {
        FileSearchFilter::Eq { key, value } => serde_json::json!({
            "type": "eq",
            "key": key,
            "value": value,
        }),
        FileSearchFilter::In { key, values } => serde_json::json!({
            "type": "in",
            "key": key,
            "values": values,
        }),
        FileSearchFilter::And(filters) => serde_json::json!({
            "type": "and",
            "filters": filters.iter().map(serialize_file_search_filter).collect::<Vec<_>>(),
        }),
        FileSearchFilter::Or(filters) => serde_json::json!({
            "type": "or",
            "filters": filters.iter().map(serialize_file_search_filter).collect::<Vec<_>>(),
        }),
    }
}

/// Serialize a request body to `Body::Bytes`.
#[allow(clippy::expect_used)] // serde_json::Value always serializes successfully
fn body_to_bytes(body: &serde_json::Value) -> Body {
    let json = serde_json::to_vec(body).expect("serde_json::Value always serializes");
    Body::Bytes(Bytes::from(json))
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAiResponsesProvider
// ════════════════════════════════════════════════════════════════════════════

/// `OpenAI` Responses API adapter. Routes all calls through OAGW.
///
/// The upstream alias is not stored — it is passed per-request to allow
/// different tenants to route to different OAGW upstreams.
#[derive(Clone)]
pub struct OpenAiResponsesProvider {
    gateway: Arc<dyn ServiceGatewayClientV1>,
}

impl OpenAiResponsesProvider {
    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { gateway }
    }
}

#[async_trait::async_trait]
impl crate::infra::llm::LlmProvider for OpenAiResponsesProvider {
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

        debug!(uri = %uri, "sending streaming request to provider");

        let response = self.gateway.proxy_request(ctx, http_request).await?;

        match ServerEventsStream::from_response::<ProviderEvent>(response) {
            ServerEventsResponse::Events(event_stream) => {
                // Translate events with accumulated text state
                let translated = event_stream.scan(String::new(), |accumulated, result| {
                    let output = match result {
                        Ok(event) => {
                            if let ProviderEvent::ResponseOutputTextDelta { ref delta } = event {
                                accumulated.push_str(delta);
                            }
                            Ok(translate_provider_event(&event, accumulated))
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "provider SSE stream error");
                            Err(e)
                        }
                    };
                    async move { Some(output) }
                });

                Ok(ProviderStream::new(translated, cancel))
            }
            ServerEventsResponse::Response(resp) => {
                // Non-SSE response — parse as JSON error
                let (parts, body) = resp.into_parts();
                tracing::warn!(status = %parts.status, "provider returned non-SSE response");
                match body.into_bytes().await {
                    Ok(bytes) => {
                        let body_preview = crate::infra::llm::sanitize_provider_message(
                            &String::from_utf8_lossy(&bytes)
                                .chars()
                                .take(200)
                                .collect::<String>(),
                        );
                        debug!(body = %body_preview, "non-SSE response body");
                        Err(parse_error_response(&bytes))
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
            return Err(parse_error_response(&bytes));
        }

        let response_obj: ResponseObject =
            serde_json::from_slice(&bytes).map_err(|_| parse_error_response(&bytes))?;

        // Extract text content from output
        let content = response_obj
            .output
            .iter()
            .flat_map(|item| &item.content)
            .filter(|part| part.r#type == "output_text")
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        // Note: citations here contain raw provider file_ids, not mapped attachment UUIDs.
        // The non-streaming complete() path is not used for user-facing requests; if it
        // ever is, map_citation_ids() must be applied by the caller.
        let citations = extract_citations(&response_obj, &content);

        let usage = Usage {
            input_tokens: response_obj.usage.input_tokens,
            output_tokens: response_obj.usage.output_tokens,
        };

        let raw = serde_json::to_value(&response_obj).unwrap_or_default();

        Ok(ResponseResult {
            content,
            usage,
            response_id: response_obj.id,
            citations,
            raw_response: raw,
        })
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[allow(clippy::str_to_string)]
mod tests {
    use super::*;
    use crate::domain::llm::WebSearchContextSize;
    use crate::infra::llm::request::{Feature, RequestMetadata, RequestType};
    use crate::infra::llm::{LlmMessage, LlmProvider, LlmTool, llm_request};

    use std::sync::Mutex;

    use futures::StreamExt;
    use oagw_sdk::error::ServiceGatewayError;
    use oagw_sdk::models::*;

    // ── MockGateway ───────────────────────────────────────────────────────

    /// What the mock should return from `proxy_request`.
    enum MockResponse {
        /// Return an SSE stream from raw byte chunks.
        Sse(Vec<String>),
        /// Return a JSON body (non-SSE).
        Json(serde_json::Value),
        /// Return a `ServiceGatewayError`.
        Error(ServiceGatewayError),
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

        fn returning_error(err: ServiceGatewayError) -> Arc<Self> {
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
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: SecurityContext,
            _: &ListQuery,
        ) -> Result<Vec<Upstream>, ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: UpdateUpstreamRequest,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), ServiceGatewayError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: SecurityContext,
            _: CreateRouteRequest,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: &ListQuery,
        ) -> Result<Vec<Route>, ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: UpdateRouteRequest,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_proxy_target(
            &self,
            _: SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(Upstream, Route), ServiceGatewayError> {
            unimplemented!()
        }
        async fn proxy_request(
            &self,
            _ctx: SecurityContext,
            req: http::Request<Body>,
        ) -> Result<http::Response<Body>, ServiceGatewayError> {
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
                feature: Feature::None,
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
                feature: Feature::FileSearchAndWebSearch,
            })
            .build_streaming();

        let body = build_request_body(&request, true);

        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
        assert_eq!(body["tools"][0]["type"], "file_search");
        assert_eq!(body["tools"][1]["type"], "web_search");
        assert_eq!(body["metadata"]["feature"], "file_search+web_search");
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
    fn builder_function_tool_dropped() {
        let request = llm_request("gpt-4o")
            .tool(LlmTool::Function {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: serde_json::json!({}),
            })
            .build_streaming();

        let body = build_request_body(&request, true);

        // Function tools are dropped for Responses API
        assert!(body.get("tools").is_none());
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
        assert!(
            matches!(result, ProviderEvent::ResponseOutputTextDelta { delta } if delta == "Hello")
        );
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
            data: r#"{"results":[{"file_id":"f1","filename":"test.pdf","score":0.95,"text":"snippet"}]}"#.to_string(),
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
    fn parse_code_interpreter_completed_event_extracts_logs() {
        let event = ServerEvent {
            event: Some("response.code_interpreter_call.completed".to_string()),
            data: r#"{"outputs":[{"type":"logs","logs":"result text"}]}"#.to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        match result {
            ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
                assert_eq!(output, "result text");
            }
            _ => panic!("expected ResponseCodeInterpreterCallCompleted"),
        }
    }

    #[test]
    fn parse_code_interpreter_completed_event_ignores_file_outputs() {
        let event = ServerEvent {
            event: Some("response.code_interpreter_call.completed".to_string()),
            data: r#"{
                "outputs": [
                    {"type":"files","file_id":"file-abc"},
                    {"type":"logs","logs":"only this"},
                    {"type":"files","file_id":"file-def"}
                ]
            }"#
            .to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        match result {
            ProviderEvent::ResponseCodeInterpreterCallCompleted { output } => {
                assert_eq!(output, "only this");
            }
            _ => panic!("expected ResponseCodeInterpreterCallCompleted"),
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
    fn parse_response_failed_event() {
        let event = ServerEvent {
            event: Some("response.failed".to_string()),
            data: r#"{"error":{"code":"server_error","message":"internal failure"}}"#.to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        match result {
            ProviderEvent::ResponseFailed { error } => {
                assert_eq!(error.code, "server_error");
                assert_eq!(error.message, "internal failure");
            }
            _ => panic!("expected ResponseFailed"),
        }
    }

    #[test]
    fn parse_response_incomplete_event() {
        let event = ServerEvent {
            event: Some("response.incomplete".to_string()),
            data: r#"{"reason":"max_output_tokens"}"#.to_string(),
            id: None,
            retry: None,
        };
        let result = ProviderEvent::from_server_event(event).unwrap();
        assert!(
            matches!(result, ProviderEvent::ResponseIncomplete { reason } if reason == "max_output_tokens")
        );
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
                },
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
    fn translate_failed_returns_terminal() {
        let event = ProviderEvent::ResponseFailed {
            error: ProviderErrorPayload {
                code: "err".into(),
                message: "failed".into(),
            },
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
            reason: "max_output_tokens".into(),
        };
        let translated = translate_provider_event(&event, "partial");
        match translated {
            TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
                reason,
                partial_content,
                ..
            }) => {
                assert_eq!(reason, "max_output_tokens");
                assert_eq!(partial_content, "partial");
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
                        title: "Report.pdf".into(),
                        url: None,
                        file_id: Some("file-xyz".into()),
                        start_index: Some(0),
                        end_index: Some(5),
                        text: Some("snippet".into()),
                    }],
                }],
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        let citations = extract_citations(&response, "");
        assert_eq!(citations.len(), 1);
        assert!(matches!(citations[0].source, CitationSource::File));
        assert_eq!(citations[0].title, "Report.pdf");
        assert_eq!(citations[0].attachment_id.as_deref(), Some("file-xyz"));
        assert_eq!(citations[0].span.unwrap().start, 0);
        assert_eq!(citations[0].span.unwrap().end, 5);
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
                        start_index: None,
                        end_index: None,
                        text: None,
                    }],
                }],
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
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
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        let citations = extract_citations(&response, "");
        assert!(citations.is_empty());
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
                        start_index: Some(10),
                        end_index: Some(31),
                        text: None,
                    }],
                }],
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
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
                        start_index: Some(0),
                        end_index: Some(5),
                        text: Some("explicit snippet".into()),
                    }],
                }],
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
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
                        start_index: None,
                        end_index: None,
                        text: None,
                    }],
                }],
            }],
            usage: RawUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
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

    #[tokio::test]
    async fn oagw_rate_limit_error() {
        let gw = MockGateway::returning_error(ServiceGatewayError::RateLimitExceeded {
            detail: "too many requests".into(),
            instance: "/test".into(),
            retry_after_secs: Some(30),
        });
        let provider = OpenAiResponsesProvider::new(gw);

        let request = llm_request("gpt-4o").build_streaming();
        let cancel = CancellationToken::new();
        let result = provider
            .stream(test_security_context(), request, "openai", cancel)
            .await;

        assert!(matches!(
            result.unwrap_err(),
            LlmProviderError::RateLimited {
                retry_after_secs: Some(30)
            }
        ));
    }

    #[tokio::test]
    async fn oagw_connection_timeout_error() {
        let gw = MockGateway::returning_error(ServiceGatewayError::ConnectionTimeout {
            detail: "timed out".into(),
            instance: "/test".into(),
        });
        let provider = OpenAiResponsesProvider::new(gw);

        let request = llm_request("gpt-4o").build_streaming();
        let cancel = CancellationToken::new();
        let result = provider
            .stream(test_security_context(), request, "openai", cancel)
            .await;

        assert!(matches!(result.unwrap_err(), LlmProviderError::Timeout));
    }

    #[tokio::test]
    async fn oagw_upstream_disabled_error() {
        let gw = MockGateway::returning_error(ServiceGatewayError::UpstreamDisabled {
            detail: "disabled".into(),
            instance: "/test".into(),
        });
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
                feature: Feature::FileSearchAndWebSearch,
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
}
