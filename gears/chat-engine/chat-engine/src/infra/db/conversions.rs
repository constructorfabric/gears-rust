//! Entity ↔ domain conversions.
//!
//! The domain models (`Session`, `Message`, `MessageReaction`) and the SDK
//! types they alias must stay free of infrastructure imports (DE0301). The
//! `From` impls that bridge them to the SeaORM `Model` / `ActiveModel` types
//! therefore live here in the infrastructure layer. Because these impls are
//! in the same crate as the (aliased) target types and the entity types are
//! local, the orphan rule is satisfied and callers use `.into()` as before.
//
// @cpt-cf-chat-engine-domain-message:p2
// @cpt-cf-chat-engine-domain-session:p2

use sea_orm::ActiveValue::{NotSet, Set};
use uuid::Uuid;

use chat_engine_sdk::models::{
    LifecycleState, Message, MessagePart, MessagePartType, MessageRole, Session, TenantId, UserId,
};

use crate::domain::reaction::{MessageReaction, ReactionType};
use crate::infra::db::entity::message as message_entity;
use crate::infra::db::entity::message_part as message_part_entity;
use crate::infra::db::entity::message_reaction as reaction_entity;
use crate::infra::db::entity::session as session_entity;

// ---------------------------------------------------------------------------
// session
// ---------------------------------------------------------------------------

impl From<session_entity::Model> for Session {
    fn from(model: session_entity::Model) -> Self {
        let lifecycle_state = LifecycleState::from_str_value(&model.lifecycle_state)
            .unwrap_or(LifecycleState::Active);
        Session {
            session_id: model.session_id,
            tenant_id: model.tenant_id.into(),
            user_id: model.user_id.into(),
            client_id: model.client_id,
            session_type_id: model.session_type_id,
            enabled_capabilities: model.enabled_capabilities,
            metadata: model.metadata,
            lifecycle_state,
            share_token: model.share_token.map(toolkit_utils::SecretString::new),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<Session> for session_entity::ActiveModel {
    fn from(s: Session) -> Self {
        session_entity::ActiveModel {
            session_id: Set(s.session_id),
            tenant_id: Set(s.tenant_id.into_inner()),
            user_id: Set(s.user_id.into_inner()),
            client_id: Set(s.client_id),
            session_type_id: Set(s.session_type_id),
            enabled_capabilities: Set(s.enabled_capabilities),
            metadata: Set(s.metadata),
            lifecycle_state: Set(s.lifecycle_state.as_str().to_string()),
            share_token: Set(s.share_token.map(toolkit_utils::SecretString::into_inner)),
            // `deleted_at` / `scheduled_hard_delete_at` are owned by the
            // soft-delete service (Phase 12) — leave untouched here so an
            // accidental `From` round-trip from a non-deleted session does
            // not wipe out the columns.
            deleted_at: NotSet,
            scheduled_hard_delete_at: NotSet,
            created_at: Set(s.created_at),
            updated_at: Set(s.updated_at),
        }
    }
}

// ---------------------------------------------------------------------------
// message
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// message parts
// ---------------------------------------------------------------------------

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
fn part_type_from_entity(t: &message_part_entity::MessagePartType) -> MessagePartType {
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

// ---------------------------------------------------------------------------
// message reaction
// ---------------------------------------------------------------------------

impl From<reaction_entity::Model> for MessageReaction {
    fn from(m: reaction_entity::Model) -> Self {
        // Unknown values fall back to `None`. The migration in Phase 1 has
        // no CHECK constraint, so guarding here keeps the bridge panic-free
        // even if a future write smuggles in a junk value.
        let reaction_type =
            ReactionType::from_str_value(&m.reaction_type).unwrap_or(ReactionType::None);
        Self {
            message_id: m.message_id,
            user_id: m.user_id,
            reaction_type,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[cfg(test)]
#[path = "conversions_tests.rs"]
mod conversions_tests;
