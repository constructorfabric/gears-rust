use super::*;

#[test]
fn reaction_type_roundtrips_known_values() {
    for v in [
        ReactionType::Like,
        ReactionType::Dislike,
        ReactionType::None,
    ] {
        let s = v.as_str();
        assert_eq!(ReactionType::from_str_value(s), Some(v));
    }
}

#[test]
fn reaction_type_rejects_unknown_string() {
    assert_eq!(ReactionType::from_str_value("love"), None);
    assert_eq!(ReactionType::from_str_value(""), None);
}

#[test]
fn reaction_type_is_persisted_marks_none_as_transient() {
    assert!(ReactionType::Like.is_persisted());
    assert!(ReactionType::Dislike.is_persisted());
    assert!(!ReactionType::None.is_persisted());
}

#[test]
fn reaction_type_serializes_snake_case() {
    let s = serde_json::to_string(&ReactionType::Like).unwrap();
    assert_eq!(s, "\"like\"");
    let s = serde_json::to_string(&ReactionType::Dislike).unwrap();
    assert_eq!(s, "\"dislike\"");
    let s = serde_json::to_string(&ReactionType::None).unwrap();
    assert_eq!(s, "\"none\"");
}

#[test]
fn event_carries_kind_and_now_timestamp() {
    let before = OffsetDateTime::now_utc();
    let event = MessageReactionEvent::new(
        Uuid::nil(),
        Uuid::nil(),
        "u".into(),
        ReactionType::Like,
        Some(ReactionType::Dislike),
    );
    let after = OffsetDateTime::now_utc();
    assert_eq!(event.event, "message.reaction");
    assert!(event.timestamp >= before && event.timestamp <= after);
    assert_eq!(event.previous_reaction_type, Some(ReactionType::Dislike));
}
