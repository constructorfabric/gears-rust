use super::*;
use sea_orm::ActiveValue;
use time::OffsetDateTime;

fn sample_model() -> message_entity::Model {
    message_entity::Model {
        message_id: Uuid::nil(),
        session_id: Uuid::nil(),
        owner_tenant_id: Uuid::nil(),
        owner_id: Uuid::nil(),
        tenant_id: Some("tenant-1".to_string()),
        user_id: Some("user-1".to_string()),
        parent_message_id: None,
        role: message_entity::MessageRole::Assistant,
        file_ids: Some(serde_json::json!(["00000000-0000-0000-0000-000000000001"])),
        variant_index: 2,
        is_active: true,
        is_complete: false,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        metadata: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn model_to_message_decodes_role_and_file_ids() {
    let msg: Message = sample_model().into();
    assert_eq!(msg.role, MessageRole::Assistant);
    assert_eq!(msg.file_ids.len(), 1);
    assert_eq!(msg.variant_index, 2);
    assert_eq!(msg.created_at, msg.updated_at);
}

#[test]
fn tenant_and_user_round_trip_through_conversions() {
    let msg: Message = sample_model().into();
    assert_eq!(
        msg.tenant_id.as_ref().map(TenantId::as_str),
        Some("tenant-1")
    );
    assert_eq!(msg.user_id.as_ref().map(UserId::as_str), Some("user-1"));

    let am: message_entity::ActiveModel = msg.into();
    assert!(matches!(am.tenant_id, ActiveValue::Set(Some(ref t)) if t == "tenant-1"));
    assert!(matches!(am.user_id, ActiveValue::Set(Some(ref u)) if u == "user-1"));
}

#[test]
fn null_and_empty_string_tenant_user_decode_to_none() {
    // NULL columns decode to None; empty strings are filtered defensively
    // rather than panicking in TenantId/UserId::from.
    let mut model = sample_model();
    model.tenant_id = None;
    model.user_id = Some(String::new());
    let msg: Message = model.into();
    assert!(msg.tenant_id.is_none());
    assert!(msg.user_id.is_none());
}

#[test]
fn message_to_active_model_encodes_role_and_file_ids() {
    let msg = Message {
        message_id: Uuid::nil(),
        session_id: Uuid::nil(),
        tenant_id: Some(TenantId::from("tenant-1")),
        user_id: Some(UserId::from("user-1")),
        parent_message_id: None,
        variant_index: 3,
        is_active: false,
        role: MessageRole::User,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "hello")],
        file_ids: vec![Uuid::nil()],
        metadata: None,
        is_complete: true,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let am: message_entity::ActiveModel = msg.into();
    match am.role {
        ActiveValue::Set(r) => assert_eq!(r, message_entity::MessageRole::User),
        other => panic!("expected Set, got {other:?}"),
    }
    match am.variant_index {
        ActiveValue::Set(i) => assert_eq!(i, 3),
        other => panic!("expected Set, got {other:?}"),
    }
}

#[test]
fn empty_file_ids_round_trip_as_none() {
    let msg = Message {
        message_id: Uuid::nil(),
        session_id: Uuid::nil(),
        tenant_id: Some(TenantId::from("tenant-1")),
        user_id: Some(UserId::from("user-1")),
        parent_message_id: None,
        variant_index: 0,
        is_active: false,
        role: MessageRole::System,
        parts: vec![],
        file_ids: vec![],
        metadata: None,
        is_complete: true,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let am: message_entity::ActiveModel = msg.into();
    match am.file_ids {
        ActiveValue::Set(None) => {}
        other => panic!("expected Set(None), got {other:?}"),
    }
}

#[test]
fn reaction_model_to_domain_unknown_value_collapses_to_none() {
    let now = OffsetDateTime::now_utc();
    let model = reaction_entity::Model {
        message_id: Uuid::nil(),
        user_id: Uuid::nil(),
        reaction_type: "purple_heart".into(),
        created_at: now,
        updated_at: now,
    };
    let domain: MessageReaction = model.into();
    assert_eq!(domain.reaction_type, ReactionType::None);
}

#[test]
fn session_to_active_model_parses_uuid_owner_columns() {
    let tenant = Uuid::from_u128(0xAA);
    let user = Uuid::from_u128(0xBB);
    let s = Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new(tenant.to_string()),
        user_id: UserId::new(user.to_string()),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: Some(toolkit_utils::SecretString::new("tok")),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let am: session_entity::ActiveModel = s.into();
    assert_eq!(am.tenant_id, ActiveValue::Set(tenant));
    assert_eq!(am.user_id, ActiveValue::Set(user));
    assert_eq!(am.share_token, ActiveValue::Set(Some("tok".to_string())));
    // Soft-delete bookkeeping columns are owned elsewhere → untouched.
    assert_eq!(am.deleted_at, ActiveValue::NotSet);
    assert_eq!(am.scheduled_hard_delete_at, ActiveValue::NotSet);
}

#[test]
fn session_to_active_model_non_uuid_owner_defaults_to_nil() {
    // Impossible-by-contract path: a non-UUID owner string defaults to nil
    // rather than panicking at the conversion boundary.
    let s = Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new("not-a-uuid"),
        user_id: UserId::new("also-bad"),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let am: session_entity::ActiveModel = s.into();
    assert_eq!(am.tenant_id, ActiveValue::Set(Uuid::nil()));
    assert_eq!(am.user_id, ActiveValue::Set(Uuid::nil()));
    assert_eq!(am.share_token, ActiveValue::Set(None));
}

#[test]
fn message_part_to_active_model_maps_type_number_and_leaves_owner_unset() {
    let part = MessagePart::text(Uuid::nil(), Uuid::nil(), 3, "hi");
    let am: message_part_entity::ActiveModel = part.into();
    assert_eq!(am.number, ActiveValue::Set(3));
    // Owner columns are stamped by the repo write path, not this conversion.
    assert_eq!(am.owner_tenant_id, ActiveValue::NotSet);
    assert_eq!(am.owner_id, ActiveValue::NotSet);
}
