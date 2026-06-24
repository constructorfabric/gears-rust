//! Client-facing **delta streaming** wire model and the projector that turns a
//! backend plugin's [`StreamingEvent`] stream into it (FR-024).
//!
//! The plugin contract stays chunk-based (`Start` → `Chunk*` → `Complete`/`Error`);
//! Chat Engine *projects* that into the SSE delta protocol the client consumes:
//! a `start` opens an (empty) message document, `delta` events mutate it by
//! `(op, path, value)`, and `complete`/`error` terminate it. Every wire event
//! carries a per-message monotonic `seq` (mirrored in the SSE `id:` line) for
//! ordering, de-duplication, and resume.
//!
//! See DESIGN `cpt-cf-chat-engine-design-streaming-protocol` and
//! `cpt-cf-chat-engine-adr-sse-delta-streaming`.
//
// @cpt-cf-chat-engine-design-streaming-protocol:p1
// @cpt-cf-chat-engine-adr-sse-delta-streaming:p1

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::domain::message::StreamingEvent;

/// Mutation operation carried by a [`WireStreamEvent::Delta`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaOp {
    /// Set the value at `path` (create a part, set a field).
    Add,
    /// Append `value` to the existing value at `path` (text fragment onto a
    /// text body, element onto a citation array).
    Append,
    /// Replace a scalar/field at `path`.
    Patch,
    /// Remove the value at `path`.
    Remove,
}

/// One event of the client-facing **typed** delta stream. Each variant
/// serializes with a specific `"type"` discriminator — `message.start`,
/// `message.part.add`, `message.text.delta`, `message.file_citation.add`,
/// `message.link_citation.add`, `message.reference.add`, `message.complete`,
/// `message.error` — mirrored in the SSE `event:` line. The delta-family
/// events keep the `(op, path, value)` patch fields so the client applies each
/// to the message document by `path`. `seq` mirrors the SSE `id:` line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WireStreamEvent {
    /// Opens the assistant message document (empty; no parts yet).
    #[serde(rename = "message.start")]
    Start { message_id: Uuid, seq: u64 },
    /// Opens a new message part (`op: add`, `path: parts/{n}`).
    #[serde(rename = "message.part.add")]
    PartAdd {
        message_id: Uuid,
        seq: u64,
        op: DeltaOp,
        path: String,
        value: JsonValue,
    },
    /// Appends a text fragment to a part body
    /// (`op: append`, `path: parts/{n}/content/text`).
    #[serde(rename = "message.text.delta")]
    TextDelta {
        message_id: Uuid,
        seq: u64,
        op: DeltaOp,
        path: String,
        value: JsonValue,
    },
    /// Appends file citations to a part
    /// (`op: append`, `path: parts/{n}/file_citations`).
    #[serde(rename = "message.file_citation.add")]
    FileCitationAdd {
        message_id: Uuid,
        seq: u64,
        op: DeltaOp,
        path: String,
        value: JsonValue,
    },
    /// Appends link citations to a part.
    #[serde(rename = "message.link_citation.add")]
    LinkCitationAdd {
        message_id: Uuid,
        seq: u64,
        op: DeltaOp,
        path: String,
        value: JsonValue,
    },
    /// Appends URL references to a part.
    #[serde(rename = "message.reference.add")]
    ReferenceAdd {
        message_id: Uuid,
        seq: u64,
        op: DeltaOp,
        path: String,
        value: JsonValue,
    },
    /// Successful end; carries optional plugin metadata. Terminal.
    #[serde(rename = "message.complete")]
    Complete {
        message_id: Uuid,
        seq: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<JsonValue>,
    },
    /// Terminal error.
    #[serde(rename = "message.error")]
    Error {
        message_id: Uuid,
        seq: u64,
        error: String,
    },
}

impl WireStreamEvent {
    /// The per-message sequence number (also the SSE `id:`).
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            WireStreamEvent::Start { seq, .. }
            | WireStreamEvent::PartAdd { seq, .. }
            | WireStreamEvent::TextDelta { seq, .. }
            | WireStreamEvent::FileCitationAdd { seq, .. }
            | WireStreamEvent::LinkCitationAdd { seq, .. }
            | WireStreamEvent::ReferenceAdd { seq, .. }
            | WireStreamEvent::Complete { seq, .. }
            | WireStreamEvent::Error { seq, .. } => *seq,
        }
    }

    /// The SSE `event:` name for this event (identical to the serialized
    /// `"type"`).
    #[must_use]
    pub fn event_name(&self) -> &'static str {
        match self {
            WireStreamEvent::Start { .. } => "message.start",
            WireStreamEvent::PartAdd { .. } => "message.part.add",
            WireStreamEvent::TextDelta { .. } => "message.text.delta",
            WireStreamEvent::FileCitationAdd { .. } => "message.file_citation.add",
            WireStreamEvent::LinkCitationAdd { .. } => "message.link_citation.add",
            WireStreamEvent::ReferenceAdd { .. } => "message.reference.add",
            WireStreamEvent::Complete { .. } => "message.complete",
            WireStreamEvent::Error { .. } => "message.error",
        }
    }

    /// `true` for the terminal events that end a stream (`complete` / `error`).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WireStreamEvent::Complete { .. } | WireStreamEvent::Error { .. }
        )
    }
}

/// Path of the assistant's primary text part body (tokens append here).
const TEXT_BODY_PATH: &str = "parts/0/content/text";
/// Path of the assistant's primary text part (opened with `add`).
const TEXT_PART_PATH: &str = "parts/0";

/// Stateful projector: feed it the plugin's [`StreamingEvent`]s in order and it
/// yields the client-facing [`WireStreamEvent`]s, assigning a monotonic `seq`.
///
/// The assistant answer accumulates into a single `text` part at `parts/0`:
/// the first chunk opens the part (`add parts/0`), subsequent chunks append to
/// `parts/0/content/text`. Citations/references on `Complete` are appended to
/// the part's arrays as `delta`s before the terminal `complete`.
pub struct DeltaProjector {
    message_id: Uuid,
    next_seq: u64,
    text_opened: bool,
}

impl Default for DeltaProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaProjector {
    /// Create a projector. The assistant `message_id` is captured from the
    /// first `Start` event (the driver always emits `Start` first, stamped with
    /// the id Chat Engine assigned); all subsequent wire events carry it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            message_id: Uuid::nil(),
            next_seq: 0,
            text_opened: false,
        }
    }

    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Project one plugin event into zero or more typed wire events.
    pub fn project(&mut self, event: StreamingEvent) -> Vec<WireStreamEvent> {
        match event {
            StreamingEvent::Start(s) => {
                // Capture the assistant id Chat Engine assigned; stamp all
                // subsequent wire events with it.
                self.message_id = s.message_id;
                vec![WireStreamEvent::Start {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                }]
            }
            StreamingEvent::Chunk(c) => {
                let mut out = Vec::new();
                if !self.text_opened {
                    self.text_opened = true;
                    out.push(WireStreamEvent::PartAdd {
                        message_id: self.message_id,
                        seq: self.take_seq(),
                        op: DeltaOp::Add,
                        path: TEXT_PART_PATH.to_owned(),
                        value: json!({ "type": "text", "content": { "text": "" }, "number": 0 }),
                    });
                }
                out.push(WireStreamEvent::TextDelta {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    op: DeltaOp::Append,
                    path: TEXT_BODY_PATH.to_owned(),
                    value: JsonValue::String(c.chunk),
                });
                out
            }
            StreamingEvent::Complete(c) => {
                let mut out = Vec::new();
                if !c.file_citations.is_empty() {
                    let value = serde_json::to_value(&c.file_citations).unwrap_or(JsonValue::Null);
                    out.push(WireStreamEvent::FileCitationAdd {
                        message_id: self.message_id,
                        seq: self.take_seq(),
                        op: DeltaOp::Append,
                        path: "parts/0/file_citations".to_owned(),
                        value,
                    });
                }
                if !c.link_citations.is_empty() {
                    let value = serde_json::to_value(&c.link_citations).unwrap_or(JsonValue::Null);
                    out.push(WireStreamEvent::LinkCitationAdd {
                        message_id: self.message_id,
                        seq: self.take_seq(),
                        op: DeltaOp::Append,
                        path: "parts/0/link_citations".to_owned(),
                        value,
                    });
                }
                if !c.references.is_empty() {
                    let value = serde_json::to_value(&c.references).unwrap_or(JsonValue::Null);
                    out.push(WireStreamEvent::ReferenceAdd {
                        message_id: self.message_id,
                        seq: self.take_seq(),
                        op: DeltaOp::Append,
                        path: "parts/0/references".to_owned(),
                        value,
                    });
                }
                out.push(WireStreamEvent::Complete {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    metadata: c.metadata,
                });
                out
            }
            StreamingEvent::Error(e) => {
                vec![WireStreamEvent::Error {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    error: e.error,
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{
        StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent, StreamingStartEvent,
    };

    fn mid() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()
    }

    fn complete(file_citations: Vec<chat_engine_sdk::models::FileCitation>) -> StreamingEvent {
        StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: Uuid::nil(),
            metadata: Some(json!({ "finish_reason": "stop" })),
            file_citations,
            link_citations: vec![],
            references: vec![],
        })
    }

    #[test]
    fn happy_path_projects_start_text_deltas_and_complete() {
        let mut p = DeltaProjector::new();
        let mut events = Vec::new();
        events.extend(p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        })));
        events.extend(p.project(StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: Uuid::nil(),
            chunk: "Hel".into(),
        })));
        events.extend(p.project(StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: Uuid::nil(),
            chunk: "lo".into(),
        })));
        events.extend(p.project(complete(vec![])));

        // start, (part.add parts/0 + text.delta), text.delta, complete = 5 events
        assert_eq!(events.len(), 5);
        // seq is contiguous from 0 and every event carries our message_id.
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e.seq(), i as u64);
        }
        assert_eq!(events[0].event_name(), "message.start");
        // First chunk opens the text part then appends.
        assert!(matches!(
            &events[1],
            WireStreamEvent::PartAdd { op: DeltaOp::Add, path, .. } if path == "parts/0"
        ));
        assert!(matches!(
            &events[2],
            WireStreamEvent::TextDelta { op: DeltaOp::Append, path, value: JsonValue::String(s), .. }
                if path == "parts/0/content/text" && s == "Hel"
        ));
        // Second chunk only appends (part already open).
        assert!(matches!(
            &events[3],
            WireStreamEvent::TextDelta { op: DeltaOp::Append, path, .. } if path == "parts/0/content/text"
        ));
        assert_eq!(events[4].event_name(), "message.complete");
    }

    #[test]
    fn citations_on_complete_become_append_deltas_before_complete() {
        let cite: chat_engine_sdk::models::FileCitation = serde_json::from_value(json!({
            "document_id": "doc-1", "document_name": "Doc", "index": 1
        }))
        .unwrap();
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        let _ = p.project(StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: Uuid::nil(),
            chunk: "x".into(),
        }));
        let tail = p.project(complete(vec![cite]));
        // file_citation.add parts/0/file_citations, then complete
        assert_eq!(tail.len(), 2);
        assert!(matches!(
            &tail[0],
            WireStreamEvent::FileCitationAdd { op: DeltaOp::Append, path, .. } if path == "parts/0/file_citations"
        ));
        assert_eq!(tail[1].event_name(), "message.complete");
    }

    #[test]
    fn error_projects_single_terminal_error() {
        let mut p = DeltaProjector::new();
        let out = p.project(StreamingEvent::Error(StreamingErrorEvent {
            message_id: Uuid::nil(),
            error: "boom".into(),
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], WireStreamEvent::Error { error, .. } if error == "boom"));
    }

    #[test]
    fn wire_event_serializes_with_typed_discriminator_and_patch_fields() {
        let ev = WireStreamEvent::TextDelta {
            message_id: mid(),
            seq: 7,
            op: DeltaOp::Append,
            path: "parts/0/content/text".into(),
            value: json!("hi"),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "message.text.delta");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["op"], "append");
        assert_eq!(v["path"], "parts/0/content/text");
        assert_eq!(v["value"], "hi");
        // The SSE event: name matches the serialized type.
        assert_eq!(ev.event_name(), "message.text.delta");
    }

    #[test]
    fn terminal_events_are_flagged() {
        assert!(
            WireStreamEvent::Complete { message_id: mid(), seq: 1, metadata: None }.is_terminal()
        );
        assert!(
            WireStreamEvent::Error { message_id: mid(), seq: 1, error: "x".into() }.is_terminal()
        );
        assert!(!WireStreamEvent::Start { message_id: mid(), seq: 0 }.is_terminal());
    }
}
