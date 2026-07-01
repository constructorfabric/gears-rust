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
    Message, MessagePart, MessagePartInput, MessagePartType, MessageRole, StreamingChunkEvent,
    StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent, StreamingStartEvent, TenantId,
    UserId, VariantInfo,
};

use sea_orm::ActiveValue::Set;
use uuid::Uuid;

use crate::infra::db::entity::message as message_entity;
use crate::infra::db::entity::message_part as message_part_entity;

impl From<message_entity::Model> for Message {
    fn from(m: message_entity::Model) -> Self {
        let role = role_from_entity(&m.role);
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
            // Empty strings can't occur via the write path (newtypes reject
            // them) but we filter defensively rather than panic in
            // `TenantId`/`UserId::from` at this conversion boundary.
            tenant_id: m.tenant_id.filter(|s| !s.is_empty()).map(TenantId::from),
            user_id: m.user_id.filter(|s| !s.is_empty()).map(UserId::from),
            parent_message_id: m.parent_message_id,
            variant_index,
            is_active: m.is_active,
            role,
            // Parts live in their own table; `From<Model>` yields a message
            // with an empty `parts` list. The repo read methods attach the
            // ordered parts via `attach_parts` after this conversion.
            parts: Vec::new(),
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
            tenant_id: Set(m.tenant_id.map(|t| t.as_str().to_owned())),
            user_id: Set(m.user_id.map(|u| u.as_str().to_owned())),
            parent_message_id: Set(m.parent_message_id),
            role: Set(role_to_entity(&m.role)),
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

/// Map the persisted entity role enum to the SDK/domain role. Total and
/// exhaustive — the entity enum makes invalid roles unrepresentable, so the
/// old string-parse fallback to `System` is gone.
fn role_from_entity(role: &message_entity::MessageRole) -> MessageRole {
    match role {
        message_entity::MessageRole::User => MessageRole::User,
        message_entity::MessageRole::Assistant => MessageRole::Assistant,
        message_entity::MessageRole::System => MessageRole::System,
    }
}

/// Map the SDK/domain role to the persisted entity role enum.
fn role_to_entity(role: &MessageRole) -> message_entity::MessageRole {
    match role {
        MessageRole::User => message_entity::MessageRole::User,
        MessageRole::Assistant => message_entity::MessageRole::Assistant,
        MessageRole::System => message_entity::MessageRole::System,
    }
}

/// Concatenate the bodies of all `text`-typed parts of a message in `number`
/// order, joined by newlines. Non-text parts contribute nothing. This is the
/// canonical "plain text of a message" used by search matching, export
/// rendering, and any caller that needs a flat string view of the body.
#[must_use]
pub fn message_text(parts: &[MessagePart]) -> String {
    let mut texts = parts.iter().filter_map(|p| {
        if p.part_type == MessagePartType::Text {
            p.content.get("text").and_then(|v| v.as_str())
        } else {
            None
        }
    });
    let mut out = String::new();
    if let Some(first) = texts.next() {
        out.push_str(first);
    }
    for t in texts {
        out.push('\n');
        out.push_str(t);
    }
    out
}

impl From<message_part_entity::Model> for MessagePart {
    fn from(p: message_part_entity::Model) -> Self {
        MessagePart {
            id: p.id,
            message_id: p.message_id,
            part_type: part_type_from_entity(&p.r#type),
            content: p.content,
            // Stored `i32`, exposed as `u32`. Negative is impossible by
            // construction (`compute_next_part_number` starts at 0); clamp
            // defensively rather than panic at the boundary.
            number: u32::try_from(p.number).unwrap_or(0),
            // Citations live in their own child tables; `From<Model>` yields
            // empty lists and the repo attaches them on read (like parts).
            file_citations: Vec::new(),
            link_citations: Vec::new(),
            references: Vec::new(),
        }
    }
}

impl From<MessagePart> for message_part_entity::ActiveModel {
    fn from(p: MessagePart) -> Self {
        message_part_entity::ActiveModel {
            id: Set(p.id),
            message_id: Set(p.message_id),
            r#type: Set(part_type_to_entity(&p.part_type)),
            content: Set(p.content),
            number: Set(i32::try_from(p.number).unwrap_or(i32::MAX)),
        }
    }
}

/// Map the persisted entity part type to the SDK/domain type. Total and
/// exhaustive — the entity enum makes invalid types unrepresentable.
pub fn part_type_from_entity(t: &message_part_entity::MessagePartType) -> MessagePartType {
    match t {
        message_part_entity::MessagePartType::Text => MessagePartType::Text,
        message_part_entity::MessagePartType::Code => MessagePartType::Code,
        message_part_entity::MessagePartType::Images => MessagePartType::Images,
        message_part_entity::MessagePartType::Videos => MessagePartType::Videos,
        message_part_entity::MessagePartType::Links => MessagePartType::Links,
        message_part_entity::MessagePartType::Statuses => MessagePartType::Statuses,
    }
}

/// Map the SDK/domain part type to the persisted entity type.
pub fn part_type_to_entity(t: &MessagePartType) -> message_part_entity::MessagePartType {
    match t {
        MessagePartType::Text => message_part_entity::MessagePartType::Text,
        MessagePartType::Code => message_part_entity::MessagePartType::Code,
        MessagePartType::Images => message_part_entity::MessagePartType::Images,
        MessagePartType::Videos => message_part_entity::MessagePartType::Videos,
        MessagePartType::Links => message_part_entity::MessagePartType::Links,
        MessagePartType::Statuses => message_part_entity::MessagePartType::Statuses,
    }
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod message_tests;
