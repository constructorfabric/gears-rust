//! Provider-specific LLM adapters.
//!
//! Each adapter implements [`LlmProvider`](super::LlmProvider) by converting
//! [`LlmRequest`](super::LlmRequest) to the provider's wire format, proxying
//! through OAGW, and translating SSE events back to `TranslatedEvent`.

pub mod azure_file_storage;
pub mod azure_vector_store;
pub mod dispatching_storage;
pub mod openai_chat;
pub mod openai_file_storage;
pub mod openai_responses;
pub mod openai_vector_store;
pub mod rag_http_client;

use std::sync::Arc;

use oagw_sdk::ServiceGatewayClientV1;
use serde::{Deserialize, Serialize};

pub use openai_chat::OpenAiChatProvider;
pub use openai_responses::OpenAiResponsesProvider;

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
    }
}
