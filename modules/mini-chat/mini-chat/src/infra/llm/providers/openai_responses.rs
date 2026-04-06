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
pub(super) enum ProviderEvent {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InputTokensDetails {
    #[serde(default)]
    cached_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct OutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    input_tokens_details: Option<InputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<OutputTokensDetails>,
}

impl RawUsage {
    /// Convert to the domain [`Usage`] type.
    pub(super) fn to_usage(&self) -> Usage {
        Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self
                .input_tokens_details
                .as_ref()
                .map_or(0, |d| d.cached_tokens),
            cache_write_input_tokens: 0,
            reasoning_tokens: self
                .output_tokens_details
                .as_ref()
                .map_or(0, |d| d.reasoning_tokens),
        }
    }
}

/// Provider error payload from `response.failed` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct ProviderErrorPayload {
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
pub(super) fn parse_error_response(bytes: &[u8]) -> LlmProviderError {
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
pub(super) fn translate_provider_event(
    event: &ProviderEvent,
    accumulated_text: &str,
) -> TranslatedEvent {
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
            let usage = response.usage.to_usage();
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
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                partial_content: accumulated_text.to_owned(),
            })
        }
    }
}

/// Extract citations from a `ResponseCompleted`'s output annotations.
pub(super) fn extract_citations(
    response: &ResponseObject,
    accumulated_text: &str,
) -> Vec<Citation> {
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

        let usage = response_obj.usage.to_usage();

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
#[path = "openai_responses_tests.rs"]
mod tests;
