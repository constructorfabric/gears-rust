// Updated: 2026-04-07 by Constructor Tech
//! Provider-agnostic LLM integration layer.
//!
//! This gear defines shared types, the [`LlmProvider`] trait, and
//! [`ProviderStream`] for communicating with LLM providers via OAGW.
//! Provider-specific adapters live in [`providers`].
//!
//! # Architecture
//!
//! ```text
//! Consumer → LlmProvider::stream() → OAGW (proxy_request) → Provider
//!                                   ← ProviderStream (ClientSseEvent items)
//!                                   ← TerminalOutcome (via into_outcome)
//! ```

pub mod oagw_responses;
pub mod provider_resolver;
pub mod providers;
pub mod request;

use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};

use futures::Stream;
use futures::StreamExt;
use oagw_sdk::error::{ServiceGatewayError, StreamingError};
use oagw_sdk::reason::auth::FailureReason as AuthFailureReason;
use regex::Regex;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

// Re-export commonly used request types.
pub use request::{
    FeatureFlag, LlmMessage, LlmRequest, LlmRequestBuilder, LlmTool, RequestMetadata, RequestType,
    Role, UserIdentity,
};

// Re-export provider factory types.
pub use providers::{ProviderKind, create_provider};

// ════════════════════════════════════════════════════════════════════════════
// Streaming mode markers
// ════════════════════════════════════════════════════════════════════════════

/// Marker: request will be sent as streaming SSE.
pub struct Streaming;
/// Marker: request will be sent as single JSON response.
pub struct NonStreaming;

// ════════════════════════════════════════════════════════════════════════════
// Error types
// ════════════════════════════════════════════════════════════════════════════

/// Errors from the LLM provider layer.
///
/// Variants containing provider-originated text apply sanitization at
/// construction time — no provider IDs, URLs, or credentials leak.
#[derive(Debug, thiserror::Error)]
pub enum LlmProviderError {
    /// Provider returned 429 (after OAGW retry exhaustion).
    #[error("rate limited")]
    RateLimited { retry_after_secs: Option<u64> },

    /// Connection or request timeout from OAGW.
    #[error("provider timeout")]
    Timeout,

    /// Provider returned an error response (sanitized).
    #[error("provider error: {code}: {message}")]
    ProviderError {
        code: String,
        /// Sanitized message safe for client exposure.
        message: String,
        /// Raw unsanitized detail for internal logging only.
        #[source]
        raw_detail: Option<RawDetail>,
    },

    /// Upstream disabled or unreachable.
    #[error("provider unavailable")]
    ProviderUnavailable,

    /// Unparseable provider response.
    #[error("invalid response: {detail}")]
    InvalidResponse { detail: String },

    /// SSE stream-level error from oagw-sdk.
    #[error("stream error: {0}")]
    StreamError(#[from] StreamingError),
}

/// Wrapper for raw error detail (private, only accessible via `raw_detail()`).
pub struct RawDetail(pub(crate) String);

impl std::fmt::Debug for RawDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RawDetail").field(&self.0).finish()
    }
}

impl std::fmt::Display for RawDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RawDetail {}

impl LlmProviderError {
    /// Raw unsanitized error detail for internal logging/persistence.
    /// Stored in `chat_turns.error_detail`, never exposed via API.
    #[must_use]
    pub fn raw_detail(&self) -> Option<&str> {
        match self {
            LlmProviderError::ProviderError {
                raw_detail: Some(rd),
                ..
            } => Some(&rd.0),
            _ => None,
        }
    }
}

#[allow(clippy::unwrap_used)] // Compile-time-known regex patterns; panics in init are intentional
static RE_RESP_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(resp_|chatcmpl-|cmpl-|msg_)[A-Za-z0-9]+").unwrap());
/// Provider file and vector-store IDs (`OpenAI` `file-…`/`vs_…`, Azure
/// `assistant-…`, Anthropic `file_…`). The length floor keeps words like
/// "file-based" and `file_search` intact.
#[allow(clippy::unwrap_used)]
static RE_STORAGE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(file-|file_|assistant-|vs_)[A-Za-z0-9]{12,}").unwrap());
#[allow(clippy::unwrap_used)]
static RE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s,\])}"']+"#).unwrap());
#[allow(clippy::unwrap_used)]
static RE_CRED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(sk-[A-Za-z0-9]{10,}|Bearer\s+[A-Za-z0-9._\-]+)").unwrap());

/// Classifies a non-SSE response by status before its body is parsed:
/// the gateway's own HTTP 504 Problem (deadline) is a timeout, and a
/// provider's own HTTP 429
/// (passed through by OAGW) is a rate limit, not a generic provider error.
/// `Retry-After` in seconds is forwarded when present.
pub(crate) fn error_from_status(parts: &http::response::Parts) -> Option<LlmProviderError> {
    // A 504 with a Problem body is the gateway's own deadline
    // (`deadline_exceeded`): a timeout. A provider's own 504 keeps its JSON
    // error body and stays a provider error.
    let is_problem = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/problem+json"));
    if parts.status == http::StatusCode::GATEWAY_TIMEOUT && is_problem {
        return Some(LlmProviderError::Timeout);
    }
    if parts.status != http::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let retry_after_secs = parts
        .headers
        .get(http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    Some(LlmProviderError::RateLimited { retry_after_secs })
}

/// Regex-based scrubbing of provider response/file/vector-store IDs, URLs,
/// and credential fragments.
pub(crate) fn sanitize_provider_message(msg: &str) -> String {
    let sanitized = RE_RESP_ID.replace_all(msg, "[provider_id]");
    let sanitized = RE_STORAGE_ID.replace_all(&sanitized, "[provider_id]");
    let sanitized = RE_URL.replace_all(&sanitized, "[url]");
    RE_CRED.replace_all(&sanitized, "[credential]").into_owned()
}

// TODO(DE1302): `LlmProviderError::ProviderError` carries `raw_detail` as a
// `String`, so stringifying the gateway error is the intended fallback for the
// catch-all arm. If the variant gains a structured source, wire it up and drop
// this allow.
//
// Mapping table — ServiceGatewayError variant → LlmProviderError:
//
// | ServiceGatewayError variant                           | LlmProviderError         | Notes |
// |-------------------------------------------------------|--------------------------|-------|
// | `RateLimited`                                         | `RateLimited`            | `retry_after_secs` forwarded from the canonical `QuotaViolation.retry_after_seconds` (may be `None`) |
// | `Timeout`                                             | `Timeout`                | connect / request / idle timeouts collapse here |
// | `Unavailable`                                         | `ProviderUnavailable`    | upstream-disabled / transport / circuit-breaker — all retryable gateway-side |
// | `AuthFailed { reason: PluginInternal }`               | `ProviderUnavailable`    | gateway-side auth machinery broken; user creds are not at fault |
// | _ (everything else)                                   | `ProviderError`          | sanitized detail; raw kept for internal logging |
//
// The pre-canonical split between `UpstreamDisabled` → ProviderUnavailable and
// `DownstreamError` → ProviderError is collapsed: both upstream-disabled and
// downstream-transport failures land on canonical `service_unavailable` and
// are indistinguishable to consumers. ProviderUnavailable is the right
// disposition because both are *retryable* gateway-side failures.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<ServiceGatewayError> for LlmProviderError {
    fn from(err: ServiceGatewayError) -> Self {
        match err {
            ServiceGatewayError::RateLimited { retry_after_secs } => {
                LlmProviderError::RateLimited { retry_after_secs }
            }

            ServiceGatewayError::Timeout => LlmProviderError::Timeout,

            // Both upstream-unavailable and gateway-side auth machinery
            // failures (plugin panic, transport) surface as transient
            // provider-unavailability — retry is reasonable. AuthFailed
            // with `PluginInternal` specifically is not a user-credential
            // failure; it's broken gateway plumbing.
            ServiceGatewayError::Unavailable { .. }
            | ServiceGatewayError::AuthFailed {
                reason: AuthFailureReason::PluginInternal,
                ..
            } => LlmProviderError::ProviderUnavailable,

            other => {
                let raw = other.to_string();
                let sanitized = sanitize_provider_message(&raw);
                LlmProviderError::ProviderError {
                    code: "gateway_error".to_owned(),
                    message: sanitized,
                    raw_detail: Some(RawDetail(raw)),
                }
            }
        }
    }
}

/// Chain `?`-propagation from OAGW calls (which return `CanonicalError`)
/// into [`LlmProviderError`] via the SDK's typed projection. Call sites
/// stay clean — no `.map_err(ServiceGatewayError::from)` boilerplate
/// when the consumer already wants the typed disposition.
impl From<CanonicalError> for LlmProviderError {
    fn from(err: CanonicalError) -> Self {
        ServiceGatewayError::from(err).into()
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Usage, Citation, Response types
// ════════════════════════════════════════════════════════════════════════════

// Domain-canonical definitions — re-exported for infra consumers.
pub use crate::domain::llm::{Citation, CitationSource, TextSpan, Usage};

/// Successful LLM response (non-streaming path).
#[derive(Debug)]
pub struct ResponseResult {
    pub content: String,
    pub usage: Usage,
    pub response_id: String,
    pub citations: Vec<Citation>,
    pub raw_response: serde_json::Value,
}

/// Terminal outcome when a stream ends.
#[derive(Debug)]
pub enum TerminalOutcome {
    /// Provider completed successfully.
    Completed {
        usage: Usage,
        response_id: String,
        content: String,
        citations: Vec<Citation>,
        raw_response: serde_json::Value,
    },
    /// Provider returned an error or stream failed.
    Failed {
        error: LlmProviderError,
        usage: Option<Usage>,
        partial_content: String,
    },
    /// Provider stopped early (e.g., `max_output_tokens` hit).
    Incomplete {
        reason: String,
        usage: Usage,
        partial_content: String,
    },
    /// Provider signalled a function tool call — agentic loop iteration.
    ToolUse {
        /// Provider-assigned call ID (used when replying with `function_call_output`).
        tool_use_id: String,
        /// Tool name (e.g. `"search_knowledge"`).
        name: String,
        /// Parsed arguments JSON (empty object if unparseable).
        input: serde_json::Value,
    },
}

// ════════════════════════════════════════════════════════════════════════════
// Translated events (internal)
// ════════════════════════════════════════════════════════════════════════════

/// Result of translating a provider event.
///
/// Produced by adapter streams, consumed by [`ProviderStream`] which
/// intercepts `Terminal` and `Skip`, only yielding `Sse` to consumers.
#[derive(Debug)]
pub(crate) enum TranslatedEvent {
    /// Forward to client as an SSE event.
    Sse(ClientSseEvent),
    /// Terminal outcome — captured by `ProviderStream`, not yielded.
    Terminal(TerminalOutcome),
    /// No client-visible action.
    Skip,
}

// ════════════════════════════════════════════════════════════════════════════
// Client SSE events
// ════════════════════════════════════════════════════════════════════════════

/// A client-facing SSE event payload (before SSE framing).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum ClientSseEvent {
    /// Incremental text chunk.
    #[serde(rename = "delta")]
    Delta {
        r#type: &'static str,
        content: String,
    },
    /// Tool lifecycle event.
    #[serde(rename = "tool")]
    Tool {
        phase: ToolPhase,
        name: &'static str,
        details: serde_json::Value,
    },
    /// Citations from provider annotations.
    #[serde(rename = "citations")]
    Citations { items: Vec<Citation> },
}

pub use crate::domain::llm::ToolPhase;

// ════════════════════════════════════════════════════════════════════════════
// ProviderStream
// ════════════════════════════════════════════════════════════════════════════

/// A streaming response from an LLM provider, yielding [`ClientSseEvent`]s.
///
/// Wraps a type-erased inner stream with cancellation and terminal capture.
/// Implements `Stream<Item = Result<ClientSseEvent, StreamingError>>`.
///
/// Terminal events are captured internally — call [`into_outcome`](Self::into_outcome)
/// after the stream ends to retrieve the final result.
pub struct ProviderStream {
    #[allow(clippy::type_complexity)]
    inner: Pin<Box<dyn Stream<Item = Result<TranslatedEvent, StreamingError>> + Send>>,
    cancel: CancellationToken,
    terminal: Option<TerminalOutcome>,
    accumulated_text: String,
    finished: bool,
}

impl std::fmt::Debug for ProviderStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderStream")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("finished", &self.finished)
            .field("accumulated_len", &self.accumulated_text.len())
            .finish_non_exhaustive()
    }
}

impl ProviderStream {
    /// Create a new provider stream from a translated event stream.
    pub(crate) fn new(
        inner: impl Stream<Item = Result<TranslatedEvent, StreamingError>> + Send + 'static,
        cancel: CancellationToken,
    ) -> Self {
        ProviderStream {
            inner: Box::pin(inner),
            cancel,
            terminal: None,
            accumulated_text: String::new(),
            finished: false,
        }
    }

    /// Cancel the stream. Drops the underlying HTTP connection.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Whether the stream has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Consume the stream, draining all remaining events, and return
    /// the terminal outcome.
    pub async fn into_outcome(mut self) -> TerminalOutcome {
        // Drain remaining events — terminal will be captured in poll_next
        loop {
            match self.next().await {
                Some(Ok(_)) => {} // SSE events accumulated in poll_next
                Some(Err(e)) => {
                    return TerminalOutcome::Failed {
                        error: LlmProviderError::StreamError(e),
                        usage: None,
                        partial_content: self.accumulated_text,
                    };
                }
                None => break,
            }
        }

        // After the terminal event, drain remaining inner-stream frames so
        // the upstream HTTP body is fully consumed before the connection is
        // dropped. This prevents Pingora "Downstream ConnectionClosed"
        // errors when the provider sends trailing SSE frames after the
        // terminal event (e.g. response.incomplete → stream close).
        if self.terminal.is_some() && !self.cancel.is_cancelled() {
            let _drain = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while self.inner.next().await.is_some() {}
            })
            .await;
        }

        match self.terminal {
            Some(terminal) => terminal,
            None if self.cancel.is_cancelled() => TerminalOutcome::Incomplete {
                reason: "cancelled".to_owned(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                partial_content: self.accumulated_text,
            },
            None => TerminalOutcome::Failed {
                error: LlmProviderError::InvalidResponse {
                    detail: "stream ended without terminal event".to_owned(),
                },
                usage: None,
                partial_content: self.accumulated_text,
            },
        }
    }
}

impl Stream for ProviderStream {
    type Item = Result<ClientSseEvent, StreamingError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.finished {
            return Poll::Ready(None);
        }

        if this.cancel.is_cancelled() {
            this.finished = true;
            return Poll::Ready(None);
        }

        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(TranslatedEvent::Sse(event)))) => {
                    // Accumulate only visible text (not reasoning) for DB content.
                    if let ClientSseEvent::Delta {
                        r#type: "text",
                        ref content,
                    } = event
                    {
                        this.accumulated_text.push_str(content);
                    }
                    return Poll::Ready(Some(Ok(event)));
                }
                Poll::Ready(Some(Ok(TranslatedEvent::Terminal(outcome)))) => {
                    this.finished = true;
                    this.terminal = Some(outcome);
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Ok(TranslatedEvent::Skip))) => {}
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    this.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    if this.cancel.is_cancelled() {
                        this.finished = true;
                        return Poll::Ready(None);
                    }
                    return Poll::Pending;
                }
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// LlmProvider trait
// ════════════════════════════════════════════════════════════════════════════

/// Provider-agnostic LLM trait. Each provider adapter implements this.
///
/// The `upstream_alias` parameter identifies the OAGW upstream to route
/// through. It is resolved per-request by [`ProviderResolver`](crate::infra::llm::provider_resolver::ProviderResolver) based on
/// the model's `provider_id` and the tenant's endpoint configuration.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a streaming request. Returns a stream of SSE events.
    async fn stream(
        &self,
        ctx: SecurityContext,
        request: LlmRequest<Streaming>,
        upstream_alias: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, LlmProviderError>;

    /// Send a non-streaming request. Returns the complete response.
    async fn complete(
        &self,
        ctx: SecurityContext,
        request: LlmRequest<NonStreaming>,
        upstream_alias: &str,
    ) -> Result<ResponseResult, LlmProviderError>;
}

/// Start building a provider-agnostic LLM request with the given model.
#[must_use]
pub fn llm_request(model: impl Into<String>) -> LlmRequestBuilder {
    LlmRequestBuilder::new(model)
}

// ════════════════════════════════════════════════════════════════════════════
// Tests — shared types
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mod_tests.rs"]
mod mod_tests;
