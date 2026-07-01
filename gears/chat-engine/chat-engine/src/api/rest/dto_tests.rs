use super::*;
use serde_json::json;

#[test]
fn text_part_input_string_is_canonicalized_to_object() {
    // A bare-string text part is wrapped so it matches the persisted /
    // streamed assistant shape `{ "text": ... }`.
    let dto = MessagePartInputDto {
        part_type: "text".into(),
        content: json!("hello"),
    };
    let sdk = SdkMessagePartInput::from(dto);
    assert_eq!(sdk.part_type, MessagePartType::Text);
    assert_eq!(sdk.content, json!({ "text": "hello" }));
}

#[test]
fn text_part_input_object_passes_through() {
    let dto = MessagePartInputDto {
        part_type: "text".into(),
        content: json!({ "text": "hi" }),
    };
    assert_eq!(
        SdkMessagePartInput::from(dto).content,
        json!({ "text": "hi" })
    );
}

#[test]
fn non_text_part_content_is_untouched() {
    let dto = MessagePartInputDto {
        part_type: "links".into(),
        content: json!({ "links": [{ "url": "https://e.com" }] }),
    };
    let sdk = SdkMessagePartInput::from(dto);
    assert_eq!(sdk.part_type, MessagePartType::Links);
    assert_eq!(
        sdk.content,
        json!({ "links": [{ "url": "https://e.com" }] })
    );
}

#[test]
fn streaming_event_dto_serializes_as_tagged_union() {
    let evt = StreamingEventDto::Start(StreamingStartDto {
        message_id: Uuid::nil(),
    });
    let s = serde_json::to_string(&evt).unwrap();
    assert!(s.contains("\"type\":\"start\""));
}

#[test]
fn streaming_chunk_dto_uses_flat_string_chunk() {
    let evt = StreamingEventDto::Chunk(StreamingChunkDto {
        message_id: Uuid::nil(),
        chunk: "hi".into(),
    });
    let s = serde_json::to_string(&evt).unwrap();
    assert!(s.contains("\"type\":\"chunk\""));
    assert!(s.contains("\"chunk\":\"hi\""));
}

#[test]
fn streaming_error_dto_uses_single_string_error_field() {
    let evt = StreamingEventDto::Error(StreamingErrorDto {
        message_id: Uuid::nil(),
        error: "context_overflow: too many tokens".into(),
    });
    let s = serde_json::to_string(&evt).unwrap();
    assert!(s.contains("\"type\":\"error\""));
    assert!(s.contains("\"error\":\"context_overflow: too many tokens\""));
}

#[test]
fn streaming_complete_dto_omits_metadata_when_none() {
    let evt = StreamingEventDto::Complete(StreamingCompleteDto {
        message_id: Uuid::nil(),
        metadata: None,
    });
    let value: serde_json::Value = serde_json::to_value(&evt).unwrap();
    assert!(
        value.get("metadata").is_none(),
        "metadata must be omitted when None"
    );
}

#[test]
fn streaming_complete_dto_keeps_metadata_when_present() {
    let evt = StreamingEventDto::Complete(StreamingCompleteDto {
        message_id: Uuid::nil(),
        metadata: Some(json!({"model": "gpt-x"})),
    });
    let s = serde_json::to_string(&evt).unwrap();
    assert!(s.contains("\"metadata\""));
    assert!(s.contains("\"model\":\"gpt-x\""));
}

#[test]
fn session_dto_redacts_share_token() {
    let dto_json = serde_json::to_string(&SessionDto {
        session_id: Uuid::nil(),
        tenant_id: "t".into(),
        user_id: "u".into(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: "active".into(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
    })
    .unwrap();
    assert!(
        !dto_json.contains("share_token"),
        "share_token must never leak via SessionDto"
    );
}
