//! Message reaction domain primitives (Phase 9).
//!
//! The Chat Engine SDK does not yet ship a `MessageReaction` model, so the
//! service crate owns the in-process types. ADR-0020 (Message Reactions with
//! Simple Like/Dislike) constrains the wire format to a three-value enum:
//! `like`, `dislike`, `none`. Reactions are persisted independently from the
//! immutable message tree (`message_reactions` table from Phase 1 migration
//! 4) so storing/changing/removing a reaction never touches the parent
//! `messages` row.
//!
//! Types:
//! - [`ReactionType`] — wire enum (`like` / `dislike` / `none`), serialized
//!   in snake_case to match `schemas/common/ReactionType.json`.
//! - [`MessageReaction`] — domain view of a stored row.
//! - [`MessageReactionEvent`] — payload of the fire-and-forget
//!   `message.reaction` plugin notification (ADR-0020 §Webhook Event).
//
// @cpt-cf-chat-engine-domain-reaction:p9
// @cpt-cf-chat-engine-adr-message-reactions:p9

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::infra::db::entity::message_reaction as reaction_entity;

/// Wire-level reaction kind. Stored in `message_reactions.reaction_type` as
/// a lowercase string ("like" / "dislike"); a `None` value is never persisted
/// — it deletes the row.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionType {
    /// Thumbs-up; persisted as `"like"`.
    Like,
    /// Thumbs-down; persisted as `"dislike"`.
    Dislike,
    /// Marker requesting deletion of the existing reaction. NEVER persisted.
    None,
}

impl ReactionType {
    /// Canonical lowercase string (DB / wire format).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::None => "none",
        }
    }

    /// Parse from a wire string. Returns `None` for any value outside the
    /// three-element enum.
    #[must_use]
    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "like" => Some(Self::Like),
            "dislike" => Some(Self::Dislike),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// True when this value would be persisted (i.e. anything except
    /// [`ReactionType::None`]).
    #[must_use]
    pub fn is_persisted(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Stored reaction row as exposed by [`ReactionRepo`](
/// crate::infra::db::repo::reaction_repo::ReactionRepo) and the
/// [`ReactionService`](crate::domain::service::reaction_service::ReactionService).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReaction {
    pub message_id: Uuid,
    pub user_id: String,
    pub reaction_type: ReactionType,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

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

/// Fire-and-forget plugin event payload built by the reaction service after
/// every successful add / change / remove. Mirrors
/// `schemas/webhook/MessageReactionEvent.json`.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReactionEvent {
    /// Static event discriminator (`"message.reaction"`).
    pub event: &'static str,
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub user_id: String,
    pub reaction_type: ReactionType,
    /// `None` when this is the user's first reaction on the message.
    /// `Some(ReactionType::Like|Dislike)` when changing an existing reaction.
    /// `Some(ReactionType::Like|Dislike)` (the prior value) when removing.
    pub previous_reaction_type: Option<ReactionType>,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

impl MessageReactionEvent {
    /// Canonical event discriminator string used both in the JSON payload
    /// and the structured log field.
    pub const EVENT_KIND: &'static str = "message.reaction";

    /// Build a new event with `timestamp = OffsetDateTime::now_utc()` and the
    /// fixed `event` discriminator.
    #[must_use]
    pub fn new(
        session_id: Uuid,
        message_id: Uuid,
        user_id: String,
        reaction_type: ReactionType,
        previous_reaction_type: Option<ReactionType>,
    ) -> Self {
        Self {
            event: Self::EVENT_KIND,
            session_id,
            message_id,
            user_id,
            reaction_type,
            previous_reaction_type,
            timestamp: OffsetDateTime::now_utc(),
        }
    }
}

#[cfg(test)]
#[path = "reaction_tests.rs"]
mod reaction_tests;
