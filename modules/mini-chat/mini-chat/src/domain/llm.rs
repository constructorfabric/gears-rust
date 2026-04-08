//! Domain-level LLM value types.
//!
//! Provider-agnostic types for LLM request construction. These are pure data
//! types with no infrastructure dependencies. Provider adapters in `infra::llm`
//! consume these types and map them to wire formats.

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ════════════════════════════════════════════════════════════════════════════
// Message types
// ════════════════════════════════════════════════════════════════════════════

/// A role in the conversation.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// A content part within a message.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { file_id: String },
}

/// A single message in the conversation.
#[domain_model]
#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

impl LlmMessage {
    /// Create a user message with text content.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        LlmMessage {
            role: Role::User,
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    /// Create an assistant message with text content.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        LlmMessage {
            role: Role::Assistant,
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    /// Create a user message with text and an image.
    #[must_use]
    pub fn user_with_image(text: impl Into<String>, file_id: impl Into<String>) -> Self {
        LlmMessage {
            role: Role::User,
            content: vec![
                ContentPart::Text { text: text.into() },
                ContentPart::Image {
                    file_id: file_id.into(),
                },
            ],
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tool types
// ════════════════════════════════════════════════════════════════════════════

// ════════════════════════════════════════════════════════════════════════════
// File search filter types
// ════════════════════════════════════════════════════════════════════════════

/// Recursive metadata filter for file search. Serialization to provider wire
/// format is handled in the infra layer only — do NOT derive `Serialize`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSearchFilter {
    /// Exact match: `key == value`.
    Eq { key: String, value: String },
    /// Set membership: `key IN values`.
    In { key: String, values: Vec<String> },
    /// Logical AND of sub-filters.
    And(Vec<FileSearchFilter>),
    /// Logical OR of sub-filters.
    Or(Vec<FileSearchFilter>),
}

impl FileSearchFilter {
    /// Convenience: filter for a single `attachment_id`.
    #[must_use]
    pub fn attachment_eq(id: uuid::Uuid) -> Self {
        Self::Eq {
            key: "attachment_id".to_owned(),
            value: id.to_string(),
        }
    }

    /// Convenience: filter for multiple `attachment_ids`.
    ///
    /// # Panics
    /// Panics if `ids` is empty — caller must check.
    #[must_use]
    pub fn attachment_in(ids: &[uuid::Uuid]) -> Self {
        assert!(!ids.is_empty(), "attachment_in called with empty ids");
        Self::In {
            key: "attachment_id".to_owned(),
            values: ids.iter().map(ToString::to_string).collect(),
        }
    }
}

/// A provider-agnostic tool descriptor.
///
/// Each adapter maps supported tools to its wire format and silently drops
/// unsupported ones with a `debug!` log.
#[domain_model]
#[derive(Debug, Clone)]
pub enum LlmTool {
    /// Server-side file search (provider manages execution).
    FileSearch {
        vector_store_ids: Vec<String>,
        filters: Option<FileSearchFilter>,
        max_num_results: Option<u32>,
    },
    /// Server-side web search (provider manages execution).
    WebSearch {
        search_context_size: WebSearchContextSize,
    },
    /// Server-side code interpreter (provider manages Python sandbox).
    CodeInterpreter { file_ids: Vec<String> },
    /// Generic function tool (for providers supporting function calling).
    Function {
        name: String,
        description: String,
        parameters: serde_json::Value,
    },
}

// ════════════════════════════════════════════════════════════════════════════
// Response / streaming value types
// ════════════════════════════════════════════════════════════════════════════

/// Token usage counters.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// A citation extracted from provider annotations.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Citation {
    pub source: CitationSource,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<TextSpan>,
}

/// How much context the web search tool should use.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchContextSize {
    #[default]
    Low,
    Medium,
    High,
}

/// Whether a citation came from a file or web search.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CitationSource {
    File,
    Web,
}

/// A character span within response text.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
}

/// Resolved attachment identity returned by [`build_provider_file_id_map`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct AttachmentRef {
    pub id: Uuid,
    pub filename: String,
}

/// Lifecycle phase of a tool invocation within a stream.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Start,
    Done,
}

// ════════════════════════════════════════════════════════════════════════════
// Context assembly input types
// ════════════════════════════════════════════════════════════════════════════

/// Minimal message representation for context assembly input.
///
/// Decouples context assembly from ORM entities — only carries the fields
/// needed for LLM prompt construction.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ContextMessage {
    pub role: Role,
    pub content: String,
}
