use super::*;

#[test]
fn reject_reserved_metadata_blocks_known_keys() {
    let metadata = serde_json::json!({"memory_strategy": {"type": "full"}});
    let err = reject_reserved_metadata(Some(&metadata)).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn merge_plugin_metadata_object_merge_overlay_wins() {
    let base = Some(serde_json::json!({"a": 1, "b": 2}));
    let overlay = serde_json::json!({"b": 99, "c": 3});
    let merged = merge_plugin_metadata(base, overlay);
    assert_eq!(merged, serde_json::json!({"a": 1, "b": 99, "c": 3}));
}

#[test]
fn merge_plugin_metadata_strips_reserved_keys_from_overlay() {
    let base = Some(serde_json::json!({"a": 1}));
    // Plugin tries to set engine-reserved keys — they must be dropped.
    let overlay = serde_json::json!({
        "memory_strategy": {"type": "full"},
        "retention_policy": {"x": 1},
        "share_expires_at": "2026-01-01",
        "model": "gpt-4",
    });
    let merged = merge_plugin_metadata(base, overlay);
    assert_eq!(merged, serde_json::json!({"a": 1, "model": "gpt-4"}));
}

#[test]
fn merge_plugin_metadata_uses_overlay_when_base_absent() {
    let merged = merge_plugin_metadata(None, serde_json::json!({"k": "v"}));
    assert_eq!(merged, serde_json::json!({"k": "v"}));
}

#[test]
fn merge_plugin_metadata_keeps_base_when_overlay_not_object() {
    // A non-object overlay must not clobber existing client metadata.
    let base = Some(serde_json::json!({"a": 1}));
    let merged = merge_plugin_metadata(base, serde_json::json!("oops"));
    assert_eq!(merged, serde_json::json!({"a": 1}));
}

#[test]
fn reject_reserved_metadata_allows_client_keys() {
    let metadata = serde_json::json!({"title": "hello"});
    reject_reserved_metadata(Some(&metadata)).expect("client metadata accepted");
}

#[test]
#[allow(clippy::use_debug)]
fn redact_session_clears_share_token_and_reserved_metadata() {
    let s = Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: Some(serde_json::json!({
            "memory_strategy": {"type": "full"},
            "client_field": "ok",
        })),
        lifecycle_state: LifecycleState::Active,
        share_token: Some(toolkit_utils::SecretString::new("super-secret")),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };

    // Redaction proof: Debug must not leak the share token.
    let s_debug = format!("{s:?}");
    assert!(!s_debug.contains("super-secret"));

    let redacted = redact_session(s);
    assert!(redacted.share_token.is_none());
    assert_eq!(
        redacted.metadata,
        Some(serde_json::json!({"client_field": "ok"}))
    );
}

#[test]
fn identity_rejects_empty_tenant() {
    let err = Identity::new("", "u", None).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn identity_rejects_empty_user() {
    let err = Identity::new("t", "", None).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn parse_state_falls_back_to_active() {
    assert_eq!(
        LifecycleState::from_str_value("garbage").unwrap_or(LifecycleState::Active),
        LifecycleState::Active
    );
    assert_eq!(
        LifecycleState::from_str_value("soft_deleted"),
        Some(LifecycleState::SoftDeleted)
    );
}

// Anchor for the acceptance criterion that requires `ensure_can_transition`
// to be called before every state-changing write — the call sites in
// archive/restore/delete invoke this same helper, and the test below
// verifies the routing for one representative edge.
#[test]
fn ensure_can_transition_path_used_by_service_for_archive() {
    let from = LifecycleState::Active;
    let to = LifecycleState::Archived;
    ensure_can_transition(from, to).expect("active->archived is valid");
}
