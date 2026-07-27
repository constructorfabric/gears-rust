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
pub mod exa_search_tool;
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
/// to [`LlmProvider::stream()`] and [`LlmProvider::complete()`].
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
