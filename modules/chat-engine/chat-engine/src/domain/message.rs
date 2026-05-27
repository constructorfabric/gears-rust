//! Message domain primitives.
//!
//! Re-exports the SDK `Message`, `MessageRole`, `VariantInfo`, and the
//! NDJSON streaming-event types so callers have one canonical path. Adds
//! conversion impls between the SDK `Message` and the Phase 1 SeaORM
//! entity model.
//!
//! ### Schema drift between SDK and DB
//!
//! The SDK `Message` has both `created_at` and `updated_at`, but the Phase
//! 1 `messages` table only stores `created_at` (messages are immutable
//! tree nodes per ADR-0001 — `updated_at` exists in the SDK for streaming
//! placeholders that get filled in). On read we synthesize
//! `updated_at = created_at`; on write the `ActiveModel` simply doesn't
//! carry the field.
//!
//! ### `file_ids` shape
//!
//! Phase 1 stores `messages.file_ids` as JSONB array of UUID strings (see
//! `out/phase-01-db-contract.md`). The SDK `file_ids: Vec<Uuid>` is mapped
//! to / from that JSON via `serde_json`.
//
// @cpt-cf-chat-engine-domain-message:p2

pub use chat_engine_sdk::models::{
    Message, MessageRole, StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent,
    StreamingEvent, StreamingStartEvent, VariantInfo,
};

use sea_orm::ActiveValue::Set;
use uuid::Uuid;

use crate::infra::db::entity::message as message_entity;

impl From<message_entity::Model> for Message {
    fn from(m: message_entity::Model) -> Self {
        let role = parse_role(&m.role);
        let file_ids = m
            .file_ids
            .as_ref()
            .and_then(|v| serde_json::from_value::<Vec<Uuid>>(v.clone()).ok())
            .unwrap_or_default();

        // SDK `variant_index` is `u32`, table stores `i32`. Negative values
        // are impossible by construction (the variant_index helper only
        // returns max+1 starting at 0), but we clamp defensively rather
        // than panic at the conversion boundary.
        let variant_index = u32::try_from(m.variant_index).unwrap_or(0);

        Message {
            message_id: m.message_id,
            session_id: m.session_id,
            parent_message_id: m.parent_message_id,
            variant_index,
            is_active: m.is_active,
            role,
            content: m.content,
            file_ids,
            metadata: m.metadata,
            is_complete: m.is_complete,
            is_hidden_from_user: m.is_hidden_from_user,
            is_hidden_from_backend: m.is_hidden_from_backend,
            // Schema drift: table has no `updated_at`. SDK requires one,
            // so we surface `created_at`. Service code that mutates a
            // message must update this field at the SDK layer; the DB
            // layer never reads it back.
            created_at: m.created_at,
            updated_at: m.created_at,
        }
    }
}

impl From<Message> for message_entity::ActiveModel {
    fn from(m: Message) -> Self {
        let file_ids_json = if m.file_ids.is_empty() {
            None
        } else {
            serde_json::to_value(&m.file_ids).ok()
        };

        message_entity::ActiveModel {
            message_id: Set(m.message_id),
            session_id: Set(m.session_id),
            parent_message_id: Set(m.parent_message_id),
            role: Set(role_to_str(&m.role).to_string()),
            content: Set(m.content),
            file_ids: Set(file_ids_json),
            variant_index: Set(i32::try_from(m.variant_index).unwrap_or(i32::MAX)),
            is_active: Set(m.is_active),
            is_complete: Set(m.is_complete),
            is_hidden_from_user: Set(m.is_hidden_from_user),
            is_hidden_from_backend: Set(m.is_hidden_from_backend),
            metadata: Set(m.metadata),
            created_at: Set(m.created_at),
        }
    }
}

fn parse_role(s: &str) -> MessageRole {
    match s {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        // System / unknown both fold into `System` — Phase 2 only stores
        // the raw string, schema validation lives at write time in the
        // repositories (Phases 4+). Unknown values are surfaced as `System`
        // so deserialization can't crash if a future migration adds a role
        // before the corresponding code path lands.
        _ => MessageRole::System,
    }
}

fn role_to_str(r: &MessageRole) -> &'static str {
    match r {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ActiveValue;
    use time::OffsetDateTime;

    fn sample_model() -> message_entity::Model {
        message_entity::Model {
            message_id: Uuid::nil(),
            session_id: Uuid::nil(),
            parent_message_id: None,
            role: "assistant".into(),
            content: serde_json::json!({"text": "hi"}),
            file_ids: Some(serde_json::json!([
                "00000000-0000-0000-0000-000000000001"
            ])),
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
    fn message_to_active_model_encodes_role_and_file_ids() {
        let msg = Message {
            message_id: Uuid::nil(),
            session_id: Uuid::nil(),
            parent_message_id: None,
            variant_index: 3,
            is_active: false,
            role: MessageRole::User,
            content: serde_json::json!({"text": "hello"}),
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
            ActiveValue::Set(s) => assert_eq!(s, "user"),
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
            parent_message_id: None,
            variant_index: 0,
            is_active: false,
            role: MessageRole::System,
            content: serde_json::json!({}),
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
}
