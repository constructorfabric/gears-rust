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

/// Mutation operation carried in a delta event's `o` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaOp {
    /// Set the value at `p` (create a part, set a field).
    Add,
    /// Append `v` to the existing value at `p` (text fragment onto a text body,
    /// element onto a citation array).
    Append,
    /// Replace a scalar/field at `p`.
    Patch,
    /// Remove the value at `p`.
    Remove,
    /// Terminal completion marker carried by `message.complete`.
    Stop,
}

/// One event of the client-facing **typed** delta stream. Each variant
/// serializes with a specific `"type"` discriminator — `message.start`,
/// `message.part.add`, `message.text.delta`, `message.file_citation.add`,
/// `message.link_citation.add`, `message.reference.add`, `message.complete`,
/// `message.error` — mirrored in the SSE `event:` line. The delta-family
/// events carry terse `(o, p, v)` patch fields (operation / path / value) so
/// the client applies each to the message document by `p`. `seq` mirrors the
/// SSE `id:` line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WireStreamEvent {
    /// Opens the assistant message document (empty; no parts yet).
    #[serde(rename = "message.start")]
    Start { message_id: Uuid, seq: u64 },
    /// Opens a new message part (`o: add`, `p: parts/{n}`).
    #[serde(rename = "message.part.add")]
    PartAdd {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
        #[serde(rename = "p")]
        path: String,
        #[serde(rename = "v")]
        value: JsonValue,
    },
    /// Appends a text fragment to a part body
    /// (`o: append`, `p: parts/{n}/content/text`).
    #[serde(rename = "message.text.delta")]
    TextDelta {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
        #[serde(rename = "p")]
        path: String,
        #[serde(rename = "v")]
        value: JsonValue,
    },
    /// Appends file citations to a part
    /// (`o: append`, `p: parts/{n}/file_citations`).
    #[serde(rename = "message.file_citation.add")]
    FileCitationAdd {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
        #[serde(rename = "p")]
        path: String,
        #[serde(rename = "v")]
        value: JsonValue,
    },
    /// Appends link citations to a part.
    #[serde(rename = "message.link_citation.add")]
    LinkCitationAdd {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
        #[serde(rename = "p")]
        path: String,
        #[serde(rename = "v")]
        value: JsonValue,
    },
    /// Appends URL references to a part.
    #[serde(rename = "message.reference.add")]
    ReferenceAdd {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
        #[serde(rename = "p")]
        path: String,
        #[serde(rename = "v")]
        value: JsonValue,
    },
    /// Transient progress indicator (not a document mutation; carries bespoke
    /// `code`/`detail` rather than `op`/`path`/`value`).
    #[serde(rename = "message.status.changed")]
    StatusChanged {
        message_id: Uuid,
        seq: u64,
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Opaque assistant-message state patch (merged into message metadata).
    #[serde(rename = "message.state.changed")]
    StateChanged {
        message_id: Uuid,
        seq: u64,
        state: JsonValue,
    },
    /// Session-scoped metadata patch (merged into the owning session).
    #[serde(rename = "session.meta.updated")]
    SessionMetaUpdated {
        message_id: Uuid,
        seq: u64,
        patch: JsonValue,
    },
    /// Tool-invocation trace (recorded in message metadata).
    #[serde(rename = "message.tool")]
    Tool {
        message_id: Uuid,
        seq: u64,
        tool: String,
        payload: JsonValue,
    },
    /// Successful end; carries the terminal `o: stop` marker and optional
    /// plugin metadata. Terminal.
    #[serde(rename = "message.complete")]
    Complete {
        message_id: Uuid,
        seq: u64,
        #[serde(rename = "o")]
        op: DeltaOp,
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
            | WireStreamEvent::StatusChanged { seq, .. }
            | WireStreamEvent::StateChanged { seq, .. }
            | WireStreamEvent::SessionMetaUpdated { seq, .. }
            | WireStreamEvent::Tool { seq, .. }
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
            WireStreamEvent::StatusChanged { .. } => "message.status.changed",
            WireStreamEvent::StateChanged { .. } => "message.state.changed",
            WireStreamEvent::SessionMetaUpdated { .. } => "session.meta.updated",
            WireStreamEvent::Tool { .. } => "message.tool",
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

/// Stateful projector: feed it the plugin's [`StreamingEvent`]s in order and it
/// yields the client-facing [`WireStreamEvent`]s, assigning a monotonic `seq`.
///
/// Text accumulates into the primary `text` part (opened lazily on the first
/// chunk via `message.part.add`, then `message.text.delta` appends). Discrete
/// `Part` events open further parts at the next index. Out-of-band events
/// (status / state / session-meta / tool) project to their own typed events and
/// do not mutate the document.
pub struct DeltaProjector {
    message_id: Uuid,
    next_seq: u64,
    /// Index of the primary text part once opened (lazily on first `Chunk`).
    text_part: Option<usize>,
    /// Next part index to assign — both `Chunk` (text part) and `Part` events
    /// draw from this so part numbers stay gap-free and in arrival order.
    next_part: usize,
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
            text_part: None,
            next_part: 0,
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
                let idx = match self.text_part {
                    Some(i) => i,
                    None => {
                        let i = self.next_part;
                        self.next_part += 1;
                        self.text_part = Some(i);
                        out.push(WireStreamEvent::PartAdd {
                            message_id: self.message_id,
                            seq: self.take_seq(),
                            op: DeltaOp::Add,
                            path: format!("parts/{i}"),
                            value: json!({ "type": "text", "content": { "text": "" }, "number": i }),
                        });
                        i
                    }
                };
                out.push(WireStreamEvent::TextDelta {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    op: DeltaOp::Append,
                    path: format!("parts/{idx}/content/text"),
                    value: JsonValue::String(c.chunk),
                });
                out
            }
            StreamingEvent::Status(s) => {
                vec![WireStreamEvent::StatusChanged {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    code: s.code,
                    detail: s.detail,
                }]
            }
            StreamingEvent::Part(p) => {
                let idx = self.next_part;
                self.next_part += 1;
                // Stamp the document `number` so the client places the part.
                let mut value = serde_json::to_value(&p.part).unwrap_or(JsonValue::Null);
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("number".to_owned(), JsonValue::from(idx));
                }
                vec![WireStreamEvent::PartAdd {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    op: DeltaOp::Add,
                    path: format!("parts/{idx}"),
                    value,
                }]
            }
            StreamingEvent::Citation(c) => self.citation_deltas(
                c.part_number,
                &c.file_citations,
                &c.link_citations,
                &c.references,
            ),
            StreamingEvent::State(s) => {
                vec![WireStreamEvent::StateChanged {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    state: s.state,
                }]
            }
            StreamingEvent::SessionMeta(s) => {
                vec![WireStreamEvent::SessionMetaUpdated {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    patch: s.patch,
                }]
            }
            StreamingEvent::Tool(t) => {
                vec![WireStreamEvent::Tool {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    tool: t.tool,
                    payload: t.payload,
                }]
            }
            StreamingEvent::Complete(c) => {
                // Batch citations (FR-023) attach to the primary text part.
                let tp = i32::try_from(self.text_part.unwrap_or(0)).unwrap_or(0);
                let mut out =
                    self.citation_deltas(tp, &c.file_citations, &c.link_citations, &c.references);
                out.push(WireStreamEvent::Complete {
                    message_id: self.message_id,
                    seq: self.take_seq(),
                    op: DeltaOp::Stop,
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

    /// Project citation/reference lists targeting `part_number` into the
    /// matching typed wire events (one per non-empty list).
    fn citation_deltas(
        &mut self,
        part_number: i32,
        file_citations: &[chat_engine_sdk::models::FileCitation],
        link_citations: &[chat_engine_sdk::models::LinkCitation],
        references: &[chat_engine_sdk::models::LinkReference],
    ) -> Vec<WireStreamEvent> {
        let mut out = Vec::new();
        if !file_citations.is_empty() {
            let value = serde_json::to_value(file_citations).unwrap_or(JsonValue::Null);
            out.push(WireStreamEvent::FileCitationAdd {
                message_id: self.message_id,
                seq: self.take_seq(),
                op: DeltaOp::Append,
                path: format!("parts/{part_number}/file_citations"),
                value,
            });
        }
        if !link_citations.is_empty() {
            let value = serde_json::to_value(link_citations).unwrap_or(JsonValue::Null);
            out.push(WireStreamEvent::LinkCitationAdd {
                message_id: self.message_id,
                seq: self.take_seq(),
                op: DeltaOp::Append,
                path: format!("parts/{part_number}/link_citations"),
                value,
            });
        }
        if !references.is_empty() {
            let value = serde_json::to_value(references).unwrap_or(JsonValue::Null);
            out.push(WireStreamEvent::ReferenceAdd {
                message_id: self.message_id,
                seq: self.take_seq(),
                op: DeltaOp::Append,
                path: format!("parts/{part_number}/references"),
                value,
            });
        }
        out
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
        // Terse patch keys (o/p/v).
        assert_eq!(v["o"], "append");
        assert_eq!(v["p"], "parts/0/content/text");
        assert_eq!(v["v"], "hi");
        // The SSE event: name matches the serialized type.
        assert_eq!(ev.event_name(), "message.text.delta");
    }

    #[test]
    fn complete_carries_stop_op() {
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        let out = p.project(complete(vec![]));
        let last = serde_json::to_value(out.last().unwrap()).unwrap();
        assert_eq!(last["type"], "message.complete");
        assert_eq!(last["o"], "stop");
    }

    #[test]
    fn terminal_events_are_flagged() {
        assert!(
            WireStreamEvent::Complete {
                message_id: mid(),
                seq: 1,
                op: DeltaOp::Stop,
                metadata: None
            }
            .is_terminal()
        );
        assert!(
            WireStreamEvent::Error {
                message_id: mid(),
                seq: 1,
                error: "x".into()
            }
            .is_terminal()
        );
        assert!(
            !WireStreamEvent::Start {
                message_id: mid(),
                seq: 0
            }
            .is_terminal()
        );
    }

    #[test]
    fn status_projects_transient_typed_event() {
        use chat_engine_sdk::models::StreamingStatusEvent;
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        let out = p.project(StreamingEvent::Status(StreamingStatusEvent {
            message_id: mid(),
            code: "thinking".into(),
            detail: Some("reading docs".into()),
        }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event_name(), "message.status.changed");
        let v = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(v["type"], "message.status.changed");
        assert_eq!(v["code"], "thinking");
        assert!(!out[0].is_terminal());
    }

    #[test]
    fn part_events_open_gap_free_indexed_parts() {
        use chat_engine_sdk::models::{MessagePartInput, MessagePartType, StreamingPartEvent};
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        // A text chunk claims parts/0.
        let _ = p.project(StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: mid(),
            chunk: "hi".into(),
        }));
        // A Part event claims parts/1.
        let out = p.project(StreamingEvent::Part(StreamingPartEvent {
            message_id: mid(),
            part: MessagePartInput {
                part_type: MessagePartType::Links,
                content: json!({ "links": [{ "url": "https://x" }] }),
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            },
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            WireStreamEvent::PartAdd { op: DeltaOp::Add, path, .. } if path == "parts/1"
        ));
        let v = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(v["type"], "message.part.add");
        assert_eq!(v["v"]["number"], 1);
    }

    #[test]
    fn mid_stream_citation_targets_named_part() {
        use chat_engine_sdk::models::StreamingCitationEvent;
        let cite: chat_engine_sdk::models::FileCitation = serde_json::from_value(json!({
            "document_id": "doc-1", "document_name": "Doc", "index": 1
        }))
        .unwrap();
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        let out = p.project(StreamingEvent::Citation(StreamingCitationEvent {
            message_id: mid(),
            part_number: 2,
            file_citations: vec![cite],
            link_citations: vec![],
            references: vec![],
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            WireStreamEvent::FileCitationAdd { path, .. } if path == "parts/2/file_citations"
        ));
    }

    #[test]
    fn state_session_meta_and_tool_project_typed_events() {
        use chat_engine_sdk::models::{
            StreamingSessionMetaEvent, StreamingStateEvent, StreamingToolEvent,
        };
        let mut p = DeltaProjector::new();
        let _ = p.project(StreamingEvent::Start(StreamingStartEvent {
            message_id: mid(),
        }));
        let st = p.project(StreamingEvent::State(StreamingStateEvent {
            message_id: mid(),
            state: json!({ "phase": "draft" }),
        }));
        let sm = p.project(StreamingEvent::SessionMeta(StreamingSessionMetaEvent {
            message_id: mid(),
            patch: json!({ "title": "Renamed" }),
        }));
        let tl = p.project(StreamingEvent::Tool(StreamingToolEvent {
            message_id: mid(),
            tool: "file_search".into(),
            payload: json!({ "query": "q" }),
        }));
        assert_eq!(st[0].event_name(), "message.state.changed");
        assert_eq!(sm[0].event_name(), "session.meta.updated");
        assert_eq!(tl[0].event_name(), "message.tool");
        assert_eq!(serde_json::to_value(&tl[0]).unwrap()["tool"], "file_search");
    }
}
