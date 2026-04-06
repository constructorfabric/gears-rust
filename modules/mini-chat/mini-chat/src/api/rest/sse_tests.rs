use super::*;
use crate::domain::stream_events::{DoneData, ErrorData};
use crate::infra::llm::Usage;

// ── SSE serialization tests ──

#[test]
fn ping_converts_to_sse_event() {
    assert!(StreamEvent::Ping.into_sse_event().is_ok());
}

#[test]
fn delta_converts_to_sse_event() {
    assert!(
        StreamEvent::Delta(DeltaData {
            r#type: "text",
            content: "hello".into(),
        })
        .into_sse_event()
        .is_ok()
    );
}

#[test]
fn delta_data_serializes_correctly() {
    let data = DeltaData {
        r#type: "text",
        content: "hello".into(),
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"type\":\"text\""));
    assert!(json.contains("\"content\":\"hello\""));
}

#[test]
fn done_serializes_without_optional_fields() {
    let data = DoneData {
        usage: None,
        effective_model: "gpt-4o".into(),
        selected_model: "gpt-4o".into(),
        quota_decision: "allow".into(),
        downgrade_from: None,
        downgrade_reason: None,
        quota_warnings: None,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"effective_model\":\"gpt-4o\""));
    assert!(!json.contains("downgrade_from"));
    assert!(!json.contains("downgrade_reason"));
}

#[test]
fn done_serializes_with_downgrade() {
    let data = DoneData {
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
        }),
        effective_model: "gpt-4o-mini".into(),
        selected_model: "gpt-4o".into(),
        quota_decision: "downgrade".into(),
        downgrade_from: Some("gpt-4o".into()),
        downgrade_reason: Some("premium_quota_exhausted".into()),
        quota_warnings: None,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"downgrade_reason\":\"premium_quota_exhausted\""));
    assert!(json.contains("\"downgrade_from\":\"gpt-4o\""));
}

#[test]
fn done_converts_to_sse_event() {
    assert!(
        StreamEvent::Done(Box::new(DoneData {
            usage: None,
            effective_model: "gpt-4o".into(),
            selected_model: "gpt-4o".into(),
            quota_decision: "allow".into(),
            downgrade_from: None,
            downgrade_reason: None,
            quota_warnings: None,
        }))
        .into_sse_event()
        .is_ok()
    );
}

#[test]
fn done_serializes_with_quota_warnings() {
    use crate::domain::stream_events::QuotaWarning;
    let data = DoneData {
        usage: Some(Usage {
            input_tokens: 50,
            output_tokens: 20,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
        }),
        effective_model: "gpt-5.2".into(),
        selected_model: "gpt-5.2".into(),
        quota_decision: "allow".into(),
        downgrade_from: None,
        downgrade_reason: None,
        quota_warnings: Some(vec![QuotaWarning {
            tier: crate::domain::stream_events::QuotaTier::Premium,
            period: crate::domain::stream_events::QuotaPeriod::Daily,
            remaining_percentage: 20,
            warning: true,
            exhausted: false,
            next_reset: Some(time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()),
        }]),
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"quota_warnings\""));
    assert!(json.contains("\"remaining_percentage\":20"));
    assert!(json.contains("\"warning\":true"));
    assert!(json.contains("\"exhausted\":false"));
    assert!(json.contains("\"tier\":\"premium\""));
    assert!(json.contains("\"next_reset\""));
}

#[test]
fn done_omits_quota_warnings_when_none() {
    let data = DoneData {
        usage: None,
        effective_model: "gpt-4o".into(),
        selected_model: "gpt-4o".into(),
        quota_decision: "allow".into(),
        downgrade_from: None,
        downgrade_reason: None,
        quota_warnings: None,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(!json.contains("quota_warnings"));
}

#[test]
fn error_data_serializes_correctly() {
    let data = ErrorData {
        code: "provider_error".into(),
        message: "Something went wrong".into(),
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"code\":\"provider_error\""));
    assert!(json.contains("\"message\":\"Something went wrong\""));
}

#[test]
fn error_converts_to_sse_event() {
    assert!(
        StreamEvent::Error(ErrorData {
            code: "provider_error".into(),
            message: "Something went wrong".into(),
        })
        .into_sse_event()
        .is_ok()
    );
}

// ── StreamPhase tests ──

#[test]
fn phase_idle_rejects_non_start_events() {
    assert!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::Ping)
            .is_err()
    );
    assert!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::Delta)
            .is_err()
    );
    assert!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::Tool)
            .is_err()
    );
    assert!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::Citations)
            .is_err()
    );
}

#[test]
fn phase_idle_accepts_terminal() {
    assert_eq!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::Terminal)
            .unwrap(),
        StreamPhase::Terminal
    );
}

#[test]
fn phase_streaming_rejects_ping() {
    assert!(
        StreamPhase::Streaming
            .try_advance(StreamEventKind::Ping)
            .is_err()
    );
}

#[test]
fn phase_citations_rejects_non_terminal() {
    assert!(
        StreamPhase::Citations
            .try_advance(StreamEventKind::Ping)
            .is_err()
    );
    assert!(
        StreamPhase::Citations
            .try_advance(StreamEventKind::Delta)
            .is_err()
    );
    assert!(
        StreamPhase::Citations
            .try_advance(StreamEventKind::Tool)
            .is_err()
    );
    assert!(
        StreamPhase::Citations
            .try_advance(StreamEventKind::Citations)
            .is_err()
    );
}

#[test]
fn phase_terminal_rejects_everything() {
    assert!(
        StreamPhase::Terminal
            .try_advance(StreamEventKind::Ping)
            .is_err()
    );
    assert!(
        StreamPhase::Terminal
            .try_advance(StreamEventKind::Terminal)
            .is_err()
    );
}

#[test]
fn phase_citations_accepts_terminal() {
    assert_eq!(
        StreamPhase::Citations
            .try_advance(StreamEventKind::Terminal)
            .unwrap(),
        StreamPhase::Terminal
    );
}

#[test]
fn normal_stream_sequence() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    assert_eq!(phase, StreamPhase::Started);
    phase = phase.try_advance(StreamEventKind::Ping).unwrap();
    assert_eq!(phase, StreamPhase::Pinging);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Terminal).unwrap();
    assert_eq!(phase, StreamPhase::Terminal);
}

#[test]
fn tool_stream_sequence() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Citations).unwrap();
    assert_eq!(phase, StreamPhase::Citations);
    phase = phase.try_advance(StreamEventKind::Terminal).unwrap();
    assert_eq!(phase, StreamPhase::Terminal);
}

// ── New interleaving tests ──

#[test]
fn interleaved_delta_tool_delta() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Terminal).unwrap();
    assert_eq!(phase, StreamPhase::Terminal);
}

#[test]
fn tool_then_delta_accepted() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
}

#[test]
fn ping_rejected_after_first_delta() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert!(phase.try_advance(StreamEventKind::Ping).is_err());
}

#[test]
fn ping_rejected_after_first_tool() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert!(phase.try_advance(StreamEventKind::Ping).is_err());
}

// ── StreamStarted / Started phase tests ──

#[test]
fn phase_idle_accepts_stream_started() {
    assert_eq!(
        StreamPhase::Idle
            .try_advance(StreamEventKind::StreamStarted)
            .unwrap(),
        StreamPhase::Started
    );
}

#[test]
fn phase_started_accepts_all_content_kinds() {
    assert_eq!(
        StreamPhase::Started
            .try_advance(StreamEventKind::Ping)
            .unwrap(),
        StreamPhase::Pinging
    );
    assert_eq!(
        StreamPhase::Started
            .try_advance(StreamEventKind::Delta)
            .unwrap(),
        StreamPhase::Streaming
    );
    assert_eq!(
        StreamPhase::Started
            .try_advance(StreamEventKind::Tool)
            .unwrap(),
        StreamPhase::Streaming
    );
    assert_eq!(
        StreamPhase::Started
            .try_advance(StreamEventKind::Citations)
            .unwrap(),
        StreamPhase::Citations
    );
    assert_eq!(
        StreamPhase::Started
            .try_advance(StreamEventKind::Terminal)
            .unwrap(),
        StreamPhase::Terminal
    );
}

#[test]
fn phase_started_rejects_stream_started() {
    assert!(
        StreamPhase::Started
            .try_advance(StreamEventKind::StreamStarted)
            .is_err()
    );
}

#[test]
fn stream_started_then_ping_then_deltas_then_done() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    assert_eq!(phase, StreamPhase::Started);
    phase = phase.try_advance(StreamEventKind::Ping).unwrap();
    assert_eq!(phase, StreamPhase::Pinging);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Terminal).unwrap();
    assert_eq!(phase, StreamPhase::Terminal);
}

#[test]
fn stream_started_then_tool_delta_citations_done() {
    let mut phase = StreamPhase::Idle;
    phase = phase.try_advance(StreamEventKind::StreamStarted).unwrap();
    assert_eq!(phase, StreamPhase::Started);
    phase = phase.try_advance(StreamEventKind::Tool).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Delta).unwrap();
    assert_eq!(phase, StreamPhase::Streaming);
    phase = phase.try_advance(StreamEventKind::Citations).unwrap();
    assert_eq!(phase, StreamPhase::Citations);
    phase = phase.try_advance(StreamEventKind::Terminal).unwrap();
    assert_eq!(phase, StreamPhase::Terminal);
}

#[test]
fn stream_started_converts_to_sse_event() {
    use crate::domain::stream_events::StreamStartedData;
    let rid = uuid::Uuid::new_v4();
    let mid = uuid::Uuid::new_v4();
    let data = StreamStartedData {
        request_id: rid,
        message_id: mid,
        is_new_turn: true,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains(&format!("\"request_id\":\"{rid}\"")));
    assert!(json.contains(&format!("\"message_id\":\"{mid}\"")));
    assert!(json.contains("\"is_new_turn\":true"));

    let event = StreamEvent::StreamStarted(data);
    assert!(event.into_sse_event().is_ok());
}

#[test]
fn stream_started_replay_serializes_correctly() {
    use crate::domain::stream_events::StreamStartedData;
    let data = StreamStartedData {
        request_id: uuid::Uuid::new_v4(),
        message_id: uuid::Uuid::new_v4(),
        is_new_turn: false,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"is_new_turn\":false"));
}
