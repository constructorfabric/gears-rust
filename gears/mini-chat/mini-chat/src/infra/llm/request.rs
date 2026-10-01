//! Provider-agnostic LLM request types.
//!
//! [`LlmRequest`] is the common input for all provider adapters. Each adapter
//! converts it to its provider-specific wire format.
//!
//! Core message and tool types (`Role`, `ContentPart`, `LlmMessage`, `LlmTool`)
//! are defined in [`crate::domain::llm`] and re-exported here for backward
//! compatibility with existing infra consumers.

use std::marker::PhantomData;

use serde::Serialize;

use super::{NonStreaming, Streaming};

// Re-export domain-level LLM types so existing `crate::infra::llm::request::*`
// imports continue to work.
pub use crate::domain::llm::{ContentPart, FileSearchFilter, LlmMessage, LlmTool, Role};

// ════════════════════════════════════════════════════════════════════════════
// User identity and metadata
// ════════════════════════════════════════════════════════════════════════════

/// User identity for provider abuse detection and observability.
#[derive(Debug, Clone)]
pub struct UserIdentity {
    pub tenant_id: String,
    pub user_id: String,
}

impl UserIdentity {
    /// Value of the provider `user` field (Anthropic: `metadata.user_id`).
    ///
    /// `OpenAI` and Azure `OpenAI` reject a `user` longer than 64 characters, and
    /// `{tenant}:{user}` with two UUIDs is 73. Two UUIDs are sent as their
    /// simple (hyphen-less) hex forms concatenated: exactly 64 characters,
    /// the tenant first, still mappable back to both ids. Other ids (tests)
    /// fall back to `{tenant}:{user}`.
    #[must_use]
    pub fn provider_user(&self) -> String {
        match (
            uuid::Uuid::parse_str(&self.tenant_id),
            uuid::Uuid::parse_str(&self.user_id),
        ) {
            (Ok(tenant), Ok(user)) => format!("{}{}", tenant.simple(), user.simple()),
            _ => format!("{}:{}", self.tenant_id, self.user_id),
        }
    }
}

/// Observability metadata attached to provider requests.
#[derive(Debug, Clone, Serialize)]
pub struct RequestMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub chat_id: String,
    pub request_type: RequestType,
    #[serde(rename = "feature", serialize_with = "serialize_feature")]
    pub features: Vec<FeatureFlag>,
}

fn serialize_feature<S: serde::Serializer>(
    features: &[FeatureFlag],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if features.is_empty() {
        return serializer.serialize_str("none");
    }
    let s: String = features
        .iter()
        .copied()
        .map(FeatureFlag::as_str)
        .collect::<Vec<_>>()
        .join("+");
    serializer.serialize_str(&s)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Chat,
    Summary,
    DocSummary,
}

/// Individual feature flag for observability metadata sent to the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureFlag {
    FileSearch,
    WebSearch,
    CodeInterpreter,
}

impl FeatureFlag {
    fn as_str(self) -> &'static str {
        match self {
            Self::FileSearch => "file_search",
            Self::WebSearch => "web_search",
            Self::CodeInterpreter => "code_interpreter",
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// LlmRequest
// ════════════════════════════════════════════════════════════════════════════

/// A provider-agnostic LLM request, parameterized by streaming mode.
///
/// Each provider adapter converts this to its wire format.
pub struct LlmRequest<Mode = Streaming> {
    pub(crate) model: String,
    pub(crate) messages: Vec<LlmMessage>,
    pub(crate) system_instructions: Option<String>,
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) tools: Vec<LlmTool>,
    pub(crate) user_identity: Option<UserIdentity>,
    pub(crate) metadata: Option<RequestMetadata>,
    pub(crate) max_tool_calls: Option<u32>,
    /// Typed model-policy inference params. Each adapter consumes only the
    /// fields its protocol supports — e.g. Anthropic ignores
    /// `frequency_penalty`/`presence_penalty` and renames `stop` to
    /// `stop_sequences`. Use [`LlmRequestBuilder::additional_params`] for
    /// adapter-specific extras outside this typed surface.
    pub(crate) api_params: Option<mini_chat_sdk::ModelApiParams>,
    /// Lookup map from primary `provider_file_id` (e.g. Azure Files API id) to
    /// `anthropic_file_id` for attachments where the secondary upload to
    /// Anthropic Files API succeeded. Consumed by the Anthropic adapter to
    /// substitute the right id into outbound `image`/`document` content
    /// blocks; ignored by other adapters. Empty when no chat attachments have
    /// a uploaded Anthropic counterpart.
    pub(crate) anthropic_file_ids: std::collections::HashMap<String, String>,
    pub(crate) additional_params: Option<serde_json::Value>,
    /// Raw provider-format input items appended to the `input` array after
    /// the normal messages. Used by the agentic loop to inject `function_call`
    /// and `function_call_output` items when replaying with tool results.
    pub(crate) raw_input_items: Vec<serde_json::Value>,
    pub(crate) _mode: PhantomData<Mode>,
}

impl<M> LlmRequest<M> {
    /// The model identifier set on this request.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The messages in this request.
    #[must_use]
    pub fn messages(&self) -> &[LlmMessage] {
        &self.messages
    }

    /// The tools in this request.
    #[must_use]
    pub fn tools(&self) -> &[LlmTool] {
        &self.tools
    }
}

/// Fluent builder for [`LlmRequest`].
pub struct LlmRequestBuilder {
    model: String,
    messages: Vec<LlmMessage>,
    system_instructions: Option<String>,
    max_output_tokens: Option<u64>,
    tools: Vec<LlmTool>,
    user_identity: Option<UserIdentity>,
    metadata: Option<RequestMetadata>,
    max_tool_calls: Option<u32>,
    api_params: Option<mini_chat_sdk::ModelApiParams>,
    anthropic_file_ids: std::collections::HashMap<String, String>,
    additional_params: Option<serde_json::Value>,
    raw_input_items: Vec<serde_json::Value>,
}

impl LlmRequestBuilder {
    /// Create a new builder with the required model identifier.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        LlmRequestBuilder {
            model: model.into(),
            messages: Vec::new(),
            system_instructions: None,
            max_output_tokens: None,
            tools: Vec::new(),
            user_identity: None,
            metadata: None,
            max_tool_calls: None,
            api_params: None,
            anthropic_file_ids: std::collections::HashMap::new(),
            additional_params: None,
            raw_input_items: Vec::new(),
        }
    }

    /// Add a single message to the conversation.
    #[must_use]
    pub fn message(mut self, message: LlmMessage) -> Self {
        self.messages.push(message);
        self
    }

    /// Set all messages at once.
    #[must_use]
    pub fn messages(mut self, messages: Vec<LlmMessage>) -> Self {
        self.messages = messages;
        self
    }

    /// Set system instructions.
    #[must_use]
    pub fn system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.system_instructions = Some(instructions.into());
        self
    }

    /// Set the hard token cap.
    #[must_use]
    pub fn max_output_tokens(mut self, max_tokens: u64) -> Self {
        self.max_output_tokens = Some(max_tokens);
        self
    }

    /// Add a single tool.
    #[must_use]
    pub fn tool(mut self, tool: LlmTool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set all tools at once.
    #[must_use]
    pub fn tools(mut self, tools: Vec<LlmTool>) -> Self {
        self.tools = tools;
        self
    }

    /// Set user identity for provider abuse detection.
    #[must_use]
    pub fn user_identity(
        mut self,
        tenant_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        self.user_identity = Some(UserIdentity {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        });
        self
    }

    /// Set observability metadata.
    #[must_use]
    pub fn metadata(mut self, metadata: RequestMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set the maximum tool calls per request.
    #[must_use]
    pub fn max_tool_calls(mut self, max: u32) -> Self {
        self.max_tool_calls = Some(max);
        self
    }

    /// Set typed model-policy inference params. Adapters consume only the
    /// fields their protocol supports.
    #[must_use]
    pub fn api_params(mut self, params: mini_chat_sdk::ModelApiParams) -> Self {
        self.api_params = Some(params);
        self
    }

    /// Set the `provider_file_id → anthropic_file_id` lookup map for chat
    /// attachments uploaded to Anthropic's Files API. Only consumed by the
    /// Anthropic adapter; ignored by other adapters.
    #[must_use]
    pub fn anthropic_file_ids(mut self, ids: std::collections::HashMap<String, String>) -> Self {
        self.anthropic_file_ids = ids;
        self
    }

    /// Set additional provider-specific parameters (escape hatch).
    ///
    /// Use [`Self::api_params`] for fields covered by `ModelApiParams`. This
    /// channel is reserved for adapter-specific extras outside the typed
    /// surface (e.g. Anthropic's `thinking` block).
    #[must_use]
    pub fn additional_params(mut self, params: serde_json::Value) -> Self {
        self.additional_params = Some(params);
        self
    }

    /// Append raw provider-format input items (for agentic loop replay).
    ///
    /// These are appended to the `input` array after the normal messages.
    /// Used to inject `function_call` and `function_call_output` items.
    ///
    /// Successive calls accumulate — the builder owns prior items and adds
    /// the new batch to the tail. Pass `clear_raw_input_items` (or rebuild
    /// the builder) to start fresh.
    #[must_use]
    pub fn raw_input_items(mut self, items: Vec<serde_json::Value>) -> Self {
        self.raw_input_items.extend(items);
        self
    }

    fn build_inner<Mode>(self) -> LlmRequest<Mode> {
        LlmRequest {
            model: self.model,
            messages: self.messages,
            system_instructions: self.system_instructions,
            max_output_tokens: self.max_output_tokens,
            tools: self.tools,
            user_identity: self.user_identity,
            metadata: self.metadata,
            max_tool_calls: self.max_tool_calls,
            api_params: self.api_params,
            anthropic_file_ids: self.anthropic_file_ids,
            additional_params: self.additional_params,
            raw_input_items: self.raw_input_items,
            _mode: PhantomData,
        }
    }

    /// Build a streaming request.
    #[must_use]
    pub fn build_streaming(self) -> LlmRequest<Streaming> {
        self.build_inner()
    }

    /// Build a non-streaming request.
    #[must_use]
    pub fn build_non_streaming(self) -> LlmRequest<NonStreaming> {
        self.build_inner()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod user_identity_tests {
    use super::UserIdentity;

    #[test]
    fn provider_user_fits_the_64_char_limit_for_uuids() {
        let identity = UserIdentity {
            tenant_id: "00000000-df51-5b42-9538-d2b56b7ee953".to_owned(),
            user_id: "11111111-6a88-4768-9dfc-6bcd5187d9ed".to_owned(),
        };
        let value = identity.provider_user();
        assert_eq!(value.len(), 64);
        assert_eq!(
            value,
            "00000000df515b429538d2b56b7ee953111111116a8847689dfc6bcd5187d9ed"
        );
    }

    #[test]
    fn provider_user_falls_back_for_non_uuid_ids() {
        let identity = UserIdentity {
            tenant_id: "abc".to_owned(),
            user_id: "def".to_owned(),
        };
        assert_eq!(identity.provider_user(), "abc:def");
    }
}
