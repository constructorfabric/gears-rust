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
