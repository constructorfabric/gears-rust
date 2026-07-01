use super::*;
use chat_engine_sdk::models::{TenantId, UserId};
use uuid::Uuid;

fn sample() -> Session {
    Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn memory_strategy_roundtrip() {
    let mut s = sample();
    assert!(get_memory_strategy(&s).is_none());
    set_memory_strategy(&mut s, MemoryStrategy::SlidingWindow { window_size: 7 });
    assert!(matches!(
        get_memory_strategy(&s),
        Some(MemoryStrategy::SlidingWindow { window_size: 7 })
    ));
}

#[test]
fn retention_policy_roundtrip() {
    let mut s = sample();
    set_retention_policy(
        &mut s,
        RetentionPolicy::CountBased {
            max_message_count: 50,
        },
    );
    assert!(matches!(
        get_retention_policy(&s),
        Some(RetentionPolicy::CountBased {
            max_message_count: 50
        })
    ));
}

#[test]
fn share_expires_at_roundtrip() {
    let mut s = sample();
    let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    set_share_expires_at(&mut s, Some(ts));
    assert_eq!(get_share_expires_at(&s), Some(ts));
    set_share_expires_at(&mut s, None);
    assert!(get_share_expires_at(&s).is_none());
}

#[test]
fn public_metadata_strips_reserved_keys() {
    let mut s = sample();
    s.metadata = Some(serde_json::json!({
        "memory_strategy": {"type": "full"},
        "retention_policy": {"type": "none"},
        "share_expires_at": "1970-01-01T00:00:00Z",
        "client_field": "visible",
    }));
    let public = public_metadata(&s).expect("non-reserved field remains");
    assert_eq!(public, serde_json::json!({ "client_field": "visible" }));
}

#[test]
fn public_metadata_returns_none_when_only_reserved() {
    let mut s = sample();
    s.metadata = Some(serde_json::json!({
        "memory_strategy": {"type": "full"},
    }));
    assert!(public_metadata(&s).is_none());
}

#[test]
fn ensure_can_transition_accepts_valid() {
    assert!(ensure_can_transition(LifecycleState::Active, LifecycleState::Archived).is_ok());
}

#[test]
fn ensure_can_transition_rejects_invalid() {
    let err =
        ensure_can_transition(LifecycleState::HardDeleted, LifecycleState::Active).unwrap_err();
    assert!(matches!(err, ChatEngineError::Conflict { .. }));
}
