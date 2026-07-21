// Created: 2026-07-09 by Constructor Tech
//! Item models — from `schemas/items/`.
//!
//! The schemas use a single `Item` base for both request input and response
//! output. Because two subtype pairs share a
//! discriminator (`message`, `reasoning`) but differ in shape by direction, the
//! Rust surface splits into two tagged enums: [`InputItem`] (request `input`)
//! and [`OutputItem`] (response `output`). Output items can still be fed back
//! as input by re-serializing them into the matching [`InputItem`] variant.

use crate::models::content::{InputContentPart, OutputContentPart};
use crate::models::core::Role;
use crate::models::extension::Extension;

// ---------------------------------------------------------------------------
// InputItem
// ---------------------------------------------------------------------------

/// A request input item, discriminated by `type`.
///
/// Any `type` the core does not own — a provider or plugin extension — is
/// preserved verbatim in [`InputItem::Other`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum InputItem {
    /// A message from a participant.
    #[serde(rename = "message")]
    Message(MessageItem),
    /// A function call (echoed back as context).
    #[serde(rename = "function_call")]
    FunctionCall(FunctionCallItem),
    /// The result of a function call executed by the consumer.
    #[serde(rename = "function_call_output")]
    FunctionCallOutput(FunctionCallOutputItem),
    /// A reference to an item from a previous response.
    #[serde(rename = "item_reference")]
    ItemReference(ItemReference),
    /// Reasoning context from a previous response.
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningItem),
    /// An item `type` the core does not own, preserved verbatim.
    #[serde(untagged)]
    Other(Extension),
}

// ---------------------------------------------------------------------------
// OutputItem
// ---------------------------------------------------------------------------

/// A response output item, discriminated by `type`.
///
/// Any `type` the core does not own — a provider or plugin extension (e.g.
/// `openai:web_search_call`) — is preserved verbatim in [`OutputItem::Other`]
/// and forwarded without interpretation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum OutputItem {
    /// The model's message response.
    #[serde(rename = "message")]
    Message(MessageOutput),
    /// A function call issued by the model.
    #[serde(rename = "function_call")]
    FunctionCall(FunctionCallItem),
    /// The model's reasoning trace.
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningOutput),
    /// Binary data output (Gears extension).
    #[serde(rename = "cf_gears:data")]
    Data(DataOutput),
    /// An item `type` the core does not own, preserved verbatim.
    #[serde(untagged)]
    Other(Extension),
}

// ---------------------------------------------------------------------------
// Item payloads
// ---------------------------------------------------------------------------

/// Input message item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MessageItem {
    /// Optional item identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Message author role.
    pub role: Role,
    /// Message content: a text string or an array of content parts.
    pub content: InputContent,
    /// Item lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
}

/// Function-call item.
///
/// Bidirectional: emitted as output when the model invokes a tool, and accepted
/// as input when replaying call context.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionCallItem {
    /// Item identifier (present in output, optional for input).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Item lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
    /// Call identifier correlating with a `function_call_output`.
    pub call_id: String,
    /// Function name.
    pub name: String,
    /// Function arguments as a JSON string (schema maximum 10 MiB).
    pub arguments: String,
}

/// Function-call output item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionCallOutputItem {
    /// Optional item identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Identifier of the `function_call` this output is for.
    pub call_id: String,
    /// Result: a text string or an array of content parts.
    pub output: InputContent,
    /// Item lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
}

/// Reference to an item from a previous response.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ItemReference {
    /// Identifier of the referenced item.
    pub id: String,
}

/// Input reasoning item.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReasoningItem {
    /// Optional item identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Reasoning summary parts.
    pub summary: Vec<ReasoningSummaryPart>,
    /// Raw reasoning text (null when the provider does not expose it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ReasoningContentPart>>,
    /// Encrypted reasoning content for providers that hide raw reasoning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

/// Output message item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MessageOutput {
    /// Unique item identifier.
    pub id: String,
    /// Item lifecycle status.
    pub status: ItemStatus,
    /// Message author role (always `assistant`).
    pub role: Role,
    /// Output content parts.
    pub content: Vec<OutputContentPart>,
}

/// Output reasoning item.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReasoningOutput {
    /// Unique item identifier.
    pub id: String,
    /// Human-readable reasoning summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<ReasoningSummaryPart>>,
    /// Raw reasoning text (may be null when the provider hides it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ReasoningContentPart>>,
    /// Encrypted reasoning content for round-tripping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

/// Binary data output item (Gears extension).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DataOutput {
    /// Unique item identifier.
    pub id: String,
    /// Item lifecycle status.
    pub status: ItemStatus,
    /// MIME type of the binary data.
    pub mime_type: String,
    /// Base64-encoded binary data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
    /// File-storage URL for the binary data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared item helpers
// ---------------------------------------------------------------------------

/// Content for input items: a text string or an array of content parts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum InputContent {
    /// A single text string.
    Text(String),
    /// An array of content parts.
    Parts(Vec<InputContentPart>),
}

/// Item lifecycle status.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    InProgress,
    Completed,
    Incomplete,
}

/// A reasoning summary part, discriminated by `type`.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummaryPart {
    /// Human-readable summary text.
    SummaryText {
        /// Summary text (schema maximum 10 MiB).
        text: String,
    },
}

/// A raw reasoning content part, discriminated by `type`.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContentPart {
    /// Raw reasoning text.
    ReasoningText {
        /// Reasoning text (schema maximum 10 MiB).
        text: String,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // A stand-in for a type a provider crate would define and own — the core
    // has no knowledge of it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct WebSearchCall {
        id: String,
        query: String,
    }

    #[test]
    fn known_output_item_roundtrips() {
        let wire = serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "status": "completed",
            "call_id": "c1",
            "name": "search",
            "arguments": "{}"
        });
        let item: OutputItem = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(item, OutputItem::FunctionCall(_)));
        assert_eq!(serde_json::to_value(&item).unwrap(), wire);
    }

    // The design's whole point: an out-of-tree provider type lands in `Other`,
    // is inspectable by `kind()`, and decodes into the provider's own struct —
    // with zero changes to this crate.
    #[test]
    fn extension_output_item_decodes_into_provider_type() {
        let wire = serde_json::json!({
            "type": "openai:web_search_call",
            "id": "ws_1",
            "query": "rust"
        });
        let item: OutputItem = serde_json::from_value(wire.clone()).unwrap();
        let OutputItem::Other(ext) = &item else {
            panic!("expected Other");
        };
        assert_eq!(ext.kind(), Some("openai:web_search_call"));
        let decoded: WebSearchCall = ext.decode().unwrap();
        assert_eq!(
            decoded,
            WebSearchCall {
                id: "ws_1".into(),
                query: "rust".into()
            }
        );
        // …and re-serializes verbatim for pass-through.
        assert_eq!(serde_json::to_value(&item).unwrap(), wire);
    }
}
