// Created: 2026-07-09 by Constructor Tech
//! Streaming event models — from `schemas/streaming/`.
//!
//! Server-sent events emitted while a response is streamed. Every event shares
//! a base (`type` + `sequence_number`); concrete events are `type`-discriminated
//! and map to one tagged enum, [`StreamingEvent`]. The stream terminates with a
//! `data: [DONE]` sentinel, which is not a JSON event and is not modeled here.
//!
//! Payload structs are shared across events with identical shapes (snapshot,
//! content-part, data, summary-part, and content-delta groups).

use crate::models::content::{Annotation, LogProb, OutputContentPart};
use crate::models::core::{ResponseError, ResponseResource};
use crate::models::extension::Extension;
use crate::models::items::{DataOutput, OutputItem, ReasoningSummaryPart};

// ---------------------------------------------------------------------------
// StreamingEvent
// ---------------------------------------------------------------------------

/// A streaming response event, discriminated by `type` (the SSE event name).
///
/// Any `type` the core does not own — a provider extension
/// (`{provider_slug}:{event_type}`) or third-party plugin event — is preserved
/// verbatim in [`StreamingEvent::Other`] and forwarded without interpretation.
// The core-owned variants carry a full response snapshot; the size gap versus
// `Other` is inherent to the protocol and not worth an allocation on the hot
// path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum StreamingEvent {
    /// The response was created.
    #[serde(rename = "response.created")]
    Created(ResponseSnapshotEvent),
    /// The response is in progress.
    #[serde(rename = "response.in_progress")]
    InProgress(ResponseSnapshotEvent),
    /// The response was queued.
    #[serde(rename = "response.queued")]
    Queued(ResponseSnapshotEvent),
    /// The response completed.
    #[serde(rename = "response.completed")]
    Completed(ResponseSnapshotEvent),
    /// The response ended incomplete.
    #[serde(rename = "response.incomplete")]
    Incomplete(ResponseSnapshotEvent),
    /// The response failed.
    #[serde(rename = "response.failed")]
    Failed(ResponseSnapshotEvent),

    /// An output item was added.
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded(OutputItemEvent),
    /// An output item was completed.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone(OutputItemEvent),

    /// A content part was added.
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded(ContentPartEvent),
    /// A content part was completed.
    #[serde(rename = "response.content_part.done")]
    ContentPartDone(ContentPartEvent),

    /// Output text was incrementally added.
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta(OutputTextEvent),
    /// Output text was completed.
    #[serde(rename = "response.output_text.done")]
    OutputTextDone(OutputTextEvent),
    /// An output-text annotation was added.
    #[serde(rename = "response.output_text.annotation.added")]
    OutputTextAnnotationAdded(OutputTextAnnotationAddedEvent),

    /// Refusal text was incrementally added.
    #[serde(rename = "response.refusal.delta")]
    RefusalDelta(ContentDeltaEvent),
    /// Refusal text was completed.
    #[serde(rename = "response.refusal.done")]
    RefusalDone(ContentDeltaEvent),

    /// Function-call arguments were incrementally added.
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta(FunctionCallArgumentsEvent),
    /// Function-call arguments were completed.
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone(FunctionCallArgumentsEvent),

    /// Reasoning text was incrementally added.
    #[serde(rename = "response.reasoning.delta")]
    ReasoningDelta(ContentDeltaEvent),
    /// Reasoning text was completed.
    #[serde(rename = "response.reasoning.done")]
    ReasoningDone(ContentDeltaEvent),

    /// A reasoning-summary part was added.
    #[serde(rename = "response.reasoning_summary_part.added")]
    ReasoningSummaryPartAdded(SummaryPartEvent),
    /// A reasoning-summary part was completed.
    #[serde(rename = "response.reasoning_summary_part.done")]
    ReasoningSummaryPartDone(SummaryPartEvent),
    /// Reasoning-summary text was incrementally added.
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta(ReasoningSummaryTextEvent),
    /// Reasoning-summary text was completed.
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryTextDone(ReasoningSummaryTextEvent),

    /// An error was emitted.
    #[serde(rename = "error")]
    Error(ErrorEvent),

    /// A binary data output started processing (Gears extension).
    #[serde(rename = "cf_gears:response.data.in_progress")]
    DataInProgress(DataEvent),
    /// A binary data output completed (Gears extension).
    #[serde(rename = "cf_gears:response.data.done")]
    DataDone(DataEvent),

    /// An event `type` the core does not own, preserved verbatim.
    #[serde(untagged)]
    Other(Extension),
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Lifecycle event carrying a full response snapshot (shared by the six
/// `response.*` lifecycle events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ResponseSnapshotEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// The response snapshot emitted with the event.
    pub response: ResponseResource,
}

/// An output item event — used for both `added` and `done` wire types.
/// The `item` is always present for `added` and optional for `done`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputItemEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Index of the output item.
    pub output_index: u32,
    /// The output item that was added or completed (absent for no-op done).
    #[serde(default)]
    pub item: Option<OutputItem>,
}

/// A content part was added or completed (shared by both content-part events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ContentPartEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The content part.
    pub part: OutputContentPart,
}

/// Output text event — used for both `delta` and `done` wire types.
/// For delta the `text` field carries the incremental append; for done the final text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputTextEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The text delta appended (delta) or the final text (done).
    pub text: String,
    /// Token log-probabilities emitted with the event, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<LogProb>>,
}

/// An output-text annotation was added.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputTextAnnotationAddedEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the output-text content.
    pub content_index: u32,
    /// Index of the annotation.
    pub annotation_index: u32,
    /// The annotation that was added, if any.
    pub annotation: Option<Annotation>,
}

/// Incremental text delta scoped to a content part (shared by the refusal and
/// reasoning delta events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ContentDeltaEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the content part.
    pub content_index: u32,
    /// The text delta appended.
    pub delta: String,
}

/// Function-call arguments event — used for both `delta` and `done` wire types.
/// For delta the `arguments` field carries the incremental append; for done the full arguments.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionCallArgumentsEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the tool-call item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// The arguments delta appended (delta) or the final arguments string (done).
    pub arguments: String,
}

/// A reasoning-summary part was added or completed (shared by both summary-part
/// events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SummaryPartEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the summary part.
    pub summary_index: u32,
    /// The summary content part.
    pub part: ReasoningSummaryPart,
}

/// Reasoning-summary text event — used for both `delta` and `done` wire types.
/// For delta the `text` field carries the incremental append; for done the final text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReasoningSummaryTextEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Identifier of the item that was updated.
    pub item_id: String,
    /// Index of the output item.
    pub output_index: u32,
    /// Index of the summary content.
    pub summary_index: u32,
    /// The summary text delta appended (delta) or the final summary text (done).
    pub text: String,
}

/// An error event.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ErrorEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// The error payload emitted.
    pub error: ResponseError,
}

/// Binary data output progress event (shared by the two Gears data events).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DataEvent {
    /// Monotonic ordering sequence number.
    pub sequence_number: u64,
    /// Index of the output item in the response output array.
    pub output_index: u32,
    /// The data output item.
    pub item: DataOutput,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::content::OutputText;
    use crate::models::core::Role;
    use crate::models::items::{ItemStatus, MessageOutput};

    #[test]
    fn output_item_added_roundtrips() {
        let event = StreamingEvent::OutputItemAdded(OutputItemEvent {
            sequence_number: 3,
            output_index: 0,
            item: Some(OutputItem::Message(MessageOutput {
                id: "m1".into(),
                status: ItemStatus::InProgress,
                role: Role::Assistant,
                content: vec![OutputContentPart::OutputText(OutputText {
                    text: "hi".into(),
                    annotations: vec![],
                    logprobs: None,
                })],
            })),
        });
        let value = serde_json::to_value(&event).unwrap();
        let back: StreamingEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn refusal_and_reasoning_share_delta_shape() {
        let refusal = StreamingEvent::RefusalDelta(ContentDeltaEvent {
            sequence_number: 1,
            item_id: "i1".into(),
            output_index: 0,
            content_index: 0,
            delta: "no".into(),
        });
        let reasoning: StreamingEvent = serde_json::from_value(serde_json::json!({
            "type": "response.reasoning.delta",
            "sequence_number": 2,
            "item_id": "i1",
            "output_index": 0,
            "content_index": 0,
            "delta": "thinking"
        }))
        .unwrap();
        assert!(matches!(refusal, StreamingEvent::RefusalDelta(_)));
        assert!(matches!(reasoning, StreamingEvent::ReasoningDelta(_)));
    }

    #[test]
    fn data_done_roundtrips_with_gears_discriminator() {
        let event = StreamingEvent::DataDone(DataEvent {
            sequence_number: 7,
            output_index: 0,
            item: DataOutput {
                id: "d1".into(),
                status: ItemStatus::Completed,
                mime_type: "image/png".into(),
                base64: Some("AAAA".into()),
                url: None,
            },
        });
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "cf_gears:response.data.done");
        let back: StreamingEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn output_text_delta_accepts_logprobs_without_bytes() {
        let event: StreamingEvent = serde_json::from_value(serde_json::json!({
            "type": "response.output_text.delta",
            "sequence_number": 4,
            "item_id": "i1",
            "output_index": 0,
            "content_index": 0,
            "text": "a",
            "logprobs": [
                { "token": "a", "logprob": -0.1, "top_logprobs": [{ "token": "b", "logprob": -1.0 }] }
            ]
        }))
        .unwrap();
        let StreamingEvent::OutputTextDelta(delta) = &event else {
            panic!("expected output_text.delta");
        };
        assert_eq!(delta.logprobs.as_ref().unwrap()[0].bytes, Vec::<u8>::new());
    }

    #[test]
    fn schema_includes_other_variant() {
        let schema = serde_json::to_value(schemars::schema_for!(StreamingEvent)).unwrap();
        let text = schema.to_string();
        // `Other` now appears as an untagged catch-all (any JSON object).
        assert!(
            text.contains("Other"),
            "Other must appear in schema: {text}"
        );
        // Payload field should be present in the schema defs.
        assert!(text.contains("sequence_number"), "{text}");
    }

    #[test]
    fn unknown_event_type_preserved_as_other() {
        let wire = serde_json::json!({
            "type": "openai:web_search_call.searching",
            "sequence_number": 9,
            "output_index": 0,
            "extra": { "query": "rust" }
        });
        let event: StreamingEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(event, StreamingEvent::Other(_)));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }
}
