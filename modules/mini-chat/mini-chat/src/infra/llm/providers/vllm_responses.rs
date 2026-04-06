//! vLLM Responses API adapter (`/v1/responses`).
//!
//! Implements [`LlmProvider`] for vLLM's OpenAI-compatible Responses API.
//! vLLM supports the same SSE event format as `OpenAI` but has stricter input
//! validation: assistant messages must use plain string content (not the
//! `output_text` array format), and tool-related fields are omitted.
//!
//! ## `<think>` tag handling
//!
//! Models like Qwen3 emit `<think>…</think>` blocks for chain-of-thought
//! reasoning. This provider parses those tags out of the delta stream and
//! emits them as `"reasoning"` deltas (instead of `"text"`), allowing the
//! UI to render a collapsible "Thinking" panel.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use modkit_security::SecurityContext;
use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{ServerEventsResponse, ServerEventsStream};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::llm::request::ContentPart as MessageContentPart;
use crate::infra::llm::{
    ClientSseEvent, LlmProviderError, LlmRequest, NonStreaming, ProviderStream, ResponseResult,
    Streaming, TranslatedEvent,
};

use super::openai_responses::{
    ProviderEvent, ResponseObject, extract_citations, parse_error_response,
    translate_provider_event,
};

// ════════════════════════════════════════════════════════════════════════════
// Think-tag state machine
// ════════════════════════════════════════════════════════════════════════════

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Tracks whether the stream is inside a `<think>…</think>` block.
///
/// Handles tags split across multiple deltas by buffering partial matches.
#[derive(Debug)]
struct ThinkState {
    inside: bool,
    /// Holds characters that *might* be the start of a tag but haven't been
    /// fully matched yet (e.g. `"<thi"` waiting for `"nk>"`).
    pending: String,
}

/// A chunk of text with its resolved delta type.
struct Chunk {
    delta_type: &'static str,
    text: String,
}

impl ThinkState {
    fn new() -> Self {
        Self {
            inside: false,
            pending: String::new(),
        }
    }

    /// Feed a raw delta string and return zero or more typed chunks.
    ///
    /// The state machine scans character-by-character, looking for `<think>`
    /// and `</think>` boundaries. Characters that are *not* part of a tag
    /// are grouped into chunks tagged as either `"reasoning"` (inside) or
    /// `"text"` (outside).
    fn feed(&mut self, delta: &str) -> Vec<Chunk> {
        let mut chunks: Vec<Chunk> = Vec::new();
        self.pending.push_str(delta);

        // Work on the pending buffer (which may contain leftovers from the
        // previous delta that partially matched a tag).
        let buf = std::mem::take(&mut self.pending);
        let mut pos = 0;

        while pos < buf.len() {
            let remainder = &buf[pos..];
            if remainder.starts_with('<') {
                // Try to match a tag starting at `pos`.
                let tag = if self.inside { THINK_CLOSE } else { THINK_OPEN };

                if remainder.len() >= tag.len() {
                    if remainder.starts_with(tag) {
                        // Full tag matched — toggle state, skip tag.
                        self.inside = !self.inside;
                        pos += tag.len();
                        continue;
                    }
                    // Not a tag — emit the `<` as content and advance.
                    push_char(&mut chunks, self.delta_type(), '<');
                    pos += 1;
                } else if tag.starts_with(remainder) {
                    // Partial tag at the end of buffer — stash for next delta.
                    remainder.clone_into(&mut self.pending);
                    return chunks;
                } else {
                    // Not a tag prefix — emit the `<` as content.
                    push_char(&mut chunks, self.delta_type(), '<');
                    pos += 1;
                }
            } else {
                // Advance by one full Unicode character.
                // SAFETY: `remainder` is non-empty because `pos < buf.len()`.
                #[allow(clippy::expect_used)]
                let ch = remainder.chars().next().expect("non-empty remainder");
                push_char(&mut chunks, self.delta_type(), ch);
                pos += ch.len_utf8();
            }
        }

        chunks
    }

    /// Flush any remaining pending buffer (called when the stream ends or
    /// on terminal events).
    fn flush(&mut self) -> Vec<Chunk> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let leftover = std::mem::take(&mut self.pending);
        let dt = self.delta_type();
        vec![Chunk {
            delta_type: dt,
            text: leftover,
        }]
    }

    fn delta_type(&self) -> &'static str {
        if self.inside { "reasoning" } else { "text" }
    }
}

/// Append a character to the last chunk if its type matches, or start a new one.
fn push_char(chunks: &mut Vec<Chunk>, delta_type: &'static str, ch: char) {
    if let Some(last) = chunks.last_mut()
        && last.delta_type == delta_type
    {
        last.text.push(ch);
        return;
    }
    chunks.push(Chunk {
        delta_type,
        text: ch.to_string(),
    });
}

// ════════════════════════════════════════════════════════════════════════════
// Think-aware event translation
// ════════════════════════════════════════════════════════════════════════════

/// Translate a provider event, splitting `<think>` blocks into `"reasoning"`
/// deltas and normal text into `"text"` deltas.
///
/// For non-delta events, delegates to [`translate_provider_event`].
fn translate_with_think(
    event: &ProviderEvent,
    accumulated_text: &str,
    think: &mut ThinkState,
) -> Vec<Result<TranslatedEvent, StreamingError>> {
    match event {
        ProviderEvent::ResponseOutputTextDelta { delta } => {
            let chunks = think.feed(delta);
            chunks
                .into_iter()
                .filter(|c| !c.text.is_empty())
                .map(|c| {
                    Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                        r#type: c.delta_type,
                        content: c.text,
                    }))
                })
                .collect()
        }
        other => {
            // Flush any buffered partial-tag text before terminal events
            // so it isn't silently dropped.
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = think
                .flush()
                .into_iter()
                .filter(|c| !c.text.is_empty())
                .map(|c| {
                    Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                        r#type: c.delta_type,
                        content: c.text,
                    }))
                })
                .collect();

            // Strip think tags from accumulated text so terminal outcomes
            // (Completed.content, Failed.partial_content) contain only
            // visible text.
            let clean_text = strip_think_tags(accumulated_text);
            events.push(Ok(translate_provider_event(other, &clean_text)));
            events
        }
    }
}

/// Strip `<think>…</think>` from the final non-streaming response content.
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(THINK_OPEN) {
        result.push_str(&rest[..start]);
        match rest[start..].find(THINK_CLOSE) {
            Some(end) => rest = &rest[start + end + THINK_CLOSE.len()..],
            None => {
                // Unclosed tag — drop the rest as reasoning
                return result;
            }
        }
    }
    result.push_str(rest);
    result
}

// ════════════════════════════════════════════════════════════════════════════
// Request body construction
// ════════════════════════════════════════════════════════════════════════════

/// Build the vLLM Responses API JSON body from an [`LlmRequest`].
///
/// Compared to the `OpenAI` variant, this:
/// - Uses plain string `content` for assistant messages (vLLM rejects the
///   `output_text` array format).
/// - Omits tool definitions (`file_search`, `web_search`, `code_interpreter`).
/// - Omits `metadata` and `max_tool_calls`.
fn build_request_body<M>(request: &LlmRequest<M>, stream: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "stream": stream,
        "store": false,
    });

    body["model"] = serde_json::json!(&request.model);

    let input: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter(|msg| msg.role != crate::infra::llm::request::Role::System)
        .map(|msg| {
            let role = match msg.role {
                crate::infra::llm::request::Role::User => "user",
                crate::infra::llm::request::Role::Assistant => "assistant",
                crate::infra::llm::request::Role::System => unreachable!(),
            };

            // vLLM requires assistant content as a plain string.
            if role == "assistant" {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        MessageContentPart::Text { text } => Some(text.as_str()),
                        MessageContentPart::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                return serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": text
                });
            }

            // User messages keep the structured content array.
            let content: Vec<serde_json::Value> = msg
                .content
                .iter()
                .map(|part| match part {
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

    if let Some(ref identity) = request.user_identity {
        body["user"] = serde_json::json!(format!("{}:{}", identity.tenant_id, identity.user_id));
    }

    // Merge additional provider-specific params (temperature, top_p, etc.).
    if let Some(ref extra) = request.additional_params
        && let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object())
    {
        for (k, v) in extra_obj {
            body_obj.insert(k.clone(), v.clone());
        }
    }

    body
}

/// Serialize a request body to `Body::Bytes`.
#[allow(clippy::expect_used)]
fn body_to_bytes(body: &serde_json::Value) -> Body {
    let json = serde_json::to_vec(body).expect("serde_json::Value always serializes");
    Body::Bytes(Bytes::from(json))
}

// ════════════════════════════════════════════════════════════════════════════
// VllmResponsesProvider
// ════════════════════════════════════════════════════════════════════════════

/// vLLM Responses API adapter. Routes all calls through OAGW.
///
/// Parses `<think>…</think>` tags in the response stream and emits them as
/// `"reasoning"` deltas, enabling the UI to show a "Thinking" panel.
#[derive(Clone)]
pub struct VllmResponsesProvider {
    gateway: Arc<dyn ServiceGatewayClientV1>,
}

impl VllmResponsesProvider {
    #[must_use]
    pub fn new(gateway: Arc<dyn ServiceGatewayClientV1>) -> Self {
        Self { gateway }
    }
}

#[async_trait::async_trait]
impl crate::infra::llm::LlmProvider for VllmResponsesProvider {
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
                // Scan state: (accumulated_text, think_state_machine).
                let translated = event_stream
                    .scan(
                        (String::new(), ThinkState::new()),
                        |(accumulated, think), result| {
                            let output: Vec<Result<TranslatedEvent, StreamingError>> = match result
                            {
                                Ok(event) => {
                                    if let ProviderEvent::ResponseOutputTextDelta { ref delta } =
                                        event
                                    {
                                        accumulated.push_str(delta);
                                    }
                                    translate_with_think(&event, accumulated, think)
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "provider SSE stream error");
                                    vec![Err(e)]
                                }
                            };
                            async move { Some(futures::stream::iter(output)) }
                        },
                    )
                    .flatten();

                Ok(ProviderStream::new(translated, cancel))
            }
            ServerEventsResponse::Response(resp) => {
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

        let raw_content = response_obj
            .output
            .iter()
            .flat_map(|item| &item.content)
            .filter(|part| part.r#type == "output_text")
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        // Strip think tags from non-streaming response.
        let content = strip_think_tags(&raw_content);

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
#[allow(clippy::non_ascii_literal)]
#[path = "vllm_responses_tests.rs"]
mod tests;
