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

/// Observability metadata attached to provider requests.
#[derive(Debug, Clone, Serialize)]
pub struct RequestMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub chat_id: String,
    pub request_type: RequestType,
    pub feature: Feature,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Chat,
    Summary,
    DocSummary,
}

#[derive(Debug, Clone, Serialize)]
pub enum Feature {
    #[serde(rename = "file_search")]
    FileSearch,
    #[serde(rename = "web_search")]
    WebSearch,
    #[serde(rename = "file_search+web_search")]
    FileSearchAndWebSearch,
    #[serde(rename = "code_interpreter")]
    CodeInterpreter,
    #[serde(rename = "none")]
    None,
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
    pub(crate) additional_params: Option<serde_json::Value>,
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
    additional_params: Option<serde_json::Value>,
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
            additional_params: None,
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

    /// Set additional provider-specific parameters (escape hatch).
    #[must_use]
    pub fn additional_params(mut self, params: serde_json::Value) -> Self {
        self.additional_params = Some(params);
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
            additional_params: self.additional_params,
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
