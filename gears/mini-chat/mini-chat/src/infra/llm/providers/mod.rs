//! Provider-specific LLM adapters.
//!
//! Each adapter implements [`LlmProvider`](super::LlmProvider) by converting
//! [`LlmRequest`](super::LlmRequest) to the provider's wire format, proxying
//! through OAGW, and translating SSE events back to `TranslatedEvent`.

pub mod anthropic_files_client;
pub mod anthropic_messages;
pub mod azure_file_storage;
pub mod azure_knowledge_retriever;
pub mod azure_vector_store;
pub mod dispatching_storage;
pub mod openai_chat;
pub mod openai_file_storage;
pub mod openai_responses;
pub mod openai_vector_store;
pub mod rag_http_client;
pub mod vllm_responses;

use std::sync::Arc;

use oagw_sdk::ServiceGatewayClientV1;
use serde::{Deserialize, Serialize};

pub use anthropic_messages::AnthropicMessagesProvider;
pub use openai_chat::OpenAiChatProvider;
pub use openai_responses::OpenAiResponsesProvider;
pub use vllm_responses::VllmResponsesProvider;

// ════════════════════════════════════════════════════════════════════════════
// Provider selection
// ════════════════════════════════════════════════════════════════════════════

/// Which provider adapter to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    /// `OpenAI` Responses API (`/v1/responses`).
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// `OpenAI` Chat Completions API (`/v1/chat/completions`).
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    /// vLLM Responses API (`/v1/responses`).
    #[serde(rename = "vllm_responses")]
    VllmResponses,
    /// Anthropic Messages API (`/v1/messages`).
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
}

/// Create a provider adapter from a [`ProviderKind`].
///
/// The upstream alias is not stored in the adapter — it is passed per-request
/// to [`LlmProvider::stream`](crate::infra::llm::LlmProvider::stream) and
/// [`LlmProvider::complete`](crate::infra::llm::LlmProvider::complete).
#[must_use]
pub fn create_provider(
    gateway: Arc<dyn ServiceGatewayClientV1>,
    kind: ProviderKind,
) -> Arc<dyn super::LlmProvider> {
    match kind {
        ProviderKind::OpenAiResponses => Arc::new(OpenAiResponsesProvider::new(gateway)),
        ProviderKind::OpenAiChatCompletions => Arc::new(OpenAiChatProvider::new(gateway)),
        ProviderKind::VllmResponses => Arc::new(VllmResponsesProvider::new(gateway)),
        ProviderKind::AnthropicMessages => Arc::new(AnthropicMessagesProvider::new(gateway)),
    }
}

/// Adapter-mandated OAGW upstream header rules for a [`ProviderKind`], if any.
///
/// Lives next to [`create_provider`] so that adapter-specific wire-protocol
/// requirements (e.g. the `anthropic-version` header) stay in the providers
/// gear. The OAGW provisioning layer applies the result generically and
/// never branches on `kind` itself.
///
/// Returns `None` when the adapter has no protocol-level header requirements
/// (the OAGW default — `Content-Type` auto-forwarded, everything else
/// stripped — is sufficient).
#[must_use]
pub fn upstream_headers_for_kind(kind: ProviderKind) -> Option<oagw_sdk::HeadersConfig> {
    match kind {
        ProviderKind::AnthropicMessages => Some(anthropic_messages::upstream_headers()),
        ProviderKind::OpenAiResponses
        | ProviderKind::OpenAiChatCompletions
        | ProviderKind::VllmResponses => None,
    }
}

/// Request fields `extra_body` must not overwrite: the model and input, the
/// output and tool-call caps derived from the quota, the tools the turn was
/// granted and how they are chosen, the usage report that settlement reads
/// (`stream_options`), the requested tool outputs (`include`), provider-side
/// storage (`store`, `previous_response_id`), and the caller identity.
/// Sampling knobs (`stop`, `reasoning`, `reasoning_effort`, ...) are not
/// reserved: they are catalog settings like `extra_body` itself.
const RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "input",
    "messages",
    "instructions",
    "system",
    "stream",
    "stream_options",
    "max_output_tokens",
    "max_completion_tokens",
    "max_tokens",
    "max_tool_calls",
    "tools",
    "tool_choice",
    "include",
    "store",
    "previous_response_id",
    "user",
    "metadata",
];

/// Merge the model policy's `extra_body` object into the top level of a
/// request body. Reserved keys are skipped (with a warning), so a catalog
/// entry cannot lift quota-derived caps or change the model or identity.
pub(super) fn merge_extra_body(body: &mut serde_json::Value, extra: &serde_json::Value) {
    let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (k, v) in extra_obj {
        if RESERVED_BODY_KEYS.contains(&k.as_str()) {
            tracing::warn!(key = %k, "extra_body key ignored: the request sets it");
            continue;
        }
        body_obj.insert(k.clone(), v.clone());
    }
}
