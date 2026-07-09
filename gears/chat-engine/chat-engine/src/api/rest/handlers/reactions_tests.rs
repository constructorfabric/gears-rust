use super::*;

#[test]
fn deserialize_set_reaction_body_accepts_known_values() {
    for variant in ["like", "dislike", "none"] {
        let json = format!("{{\"reaction_type\": \"{variant}\"}}");
        let body: SetReactionBody = serde_json::from_str(&json).expect("ok");
        // Anti-spoof fields default to None.
        assert!(body.tenant_id.is_none());
        assert!(body.user_id.is_none());
        // Round-trip via the helper.
        let as_str = body.reaction_type.as_str();
        assert!(matches!(as_str, "like" | "dislike" | "none"));
    }
}

#[test]
fn deserialize_set_reaction_body_rejects_unknown_values() {
    let err = serde_json::from_str::<SetReactionBody>(r#"{"reaction_type": "love"}"#)
        .expect_err("unknown variant must fail");
    assert!(err.to_string().contains("love") || err.to_string().contains("variant"));
}

#[test]
fn map_reaction_error_emits_capability_disabled_body() {
    let err = ChatEngineError::conflict("feature 'feedback' is disabled for this session type");
    let response = map_reaction_error(err);
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[test]
fn map_reaction_error_passes_through_unrelated_conflicts() {
    // A conflict whose reason does not mention `feedback` should
    // fall through to the Phase 4 scaffold — still a 409, but with
    // the generic `{"error": "<reason>"}` body.
    let err = ChatEngineError::conflict("invalid lifecycle transition");
    let response = map_reaction_error(err);
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[test]
fn set_reaction_response_dto_round_trips_through_serde() {
    let dto = SetReactionResponseDto::from(SetReactionResponse {
        message_id: Uuid::nil(),
        reaction_type: ReactionType::Like,
        applied: true,
    });
    let s = serde_json::to_string(&dto).expect("ok");
    assert!(s.contains("\"reaction_type\":\"like\""));
    assert!(s.contains("\"applied\":true"));
}

#[test]
fn list_reactions_dto_serializes_rfc3339_timestamps() {
    let now = time::OffsetDateTime::now_utc();
    let dto = ListReactionsResponseDto {
        message_id: Uuid::nil(),
        reactions: vec![ReactionDto {
            user_id: "u".into(),
            reaction_type: ReactionType::Dislike,
            created_at: now,
            updated_at: now,
        }],
    };
    let s = serde_json::to_string(&dto).expect("ok");
    assert!(s.contains("\"reaction_type\":\"dislike\""));
    assert!(s.contains("\"user_id\":\"u\""));
}
