//! Message reaction service (Phase 9).
//!
//! Orchestrates the `POST /sessions/{s}/messages/{m}/reaction` and
//! `GET /sessions/{s}/messages/{m}/reactions` surfaces. The reaction itself
//! is persisted by [`ReactionRepo`]; this service applies the
//! ADR-0020-mandated validation chain *before* persistence:
//!
//! 1. **Session ownership** — load the session via
//!    [`SessionRepo::find_by_id`] scoped to the JWT-derived
//!    `(tenant_id, user_id)`. A miss collapses to
//!    [`ChatEngineError::NotFound`] mapped to HTTP 404 (per ADR-0021's
//!    anti-enumeration policy: cross-tenant access and "doesn't exist"
//!    look identical to the caller).
//! 2. **Message ownership** — confirm the target `message_id` actually
//!    belongs to the session via
//!    [`MessageRepo::find_message_in_session`]; 404 on miss.
//! 3. **Assistant-only target** — reactions are only meaningful on
//!    assistant responses (feature spec §1.2). Attempts to react to a
//!    `user` or `system` message return [`ChatEngineError::BadRequest`]
//!    mapped to HTTP 400.
//! 4. **Capability gate** — the session's
//!    `enabled_capabilities` JSONB array MUST advertise a capability named
//!    `"feedback"`. Otherwise the service returns
//!    [`ChatEngineError::Conflict`] mapped to HTTP 409 (per Phase 9
//!    brief). The gate is *write-only*: read endpoints intentionally
//!    bypass it so a UI can render historical reactions even after a
//!    session-type switch turns the feature off.
//! 5. **UPSERT or DELETE** — routes by `reaction_type`:
//!    - `Like` / `Dislike` → [`ReactionRepo::upsert`] returning the new
//!      stored row plus `previous_reaction_type`.
//!    - `None` → [`ReactionRepo::delete`] which is idempotent (200 with
//!      `applied: false` when no prior row existed).
//!
//! After the response is built, the service spawns a fire-and-forget task
//! that resolves the backend plugin and emits a `message.reaction` event.
//! Per ADR-0020 the event MUST NOT block the client response and MUST NOT
//! propagate errors; the task logs at warning level on failure. The SDK
//! plugin trait does not yet declare an `on_message_reaction` method, so
//! the task currently emits a structured `info!` event payload that
//! Phase 14 will route through the live webhook outbox once that surface
//! lands.
//
// @cpt-cf-chat-engine-reaction-service:p9
// @cpt-cf-chat-engine-adr-message-reactions:p9

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value as JsonValue;
use tokio::task::JoinHandle;
use toolkit_macros::domain_model;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::MessageRole;
use crate::domain::ports::MessageRepo;
use crate::domain::ports::ReactionRepo;
use crate::domain::ports::SessionRepo;
use crate::domain::ports::SessionTypeRepo;
use crate::domain::reaction::{MessageReaction, MessageReactionEvent, ReactionType};
use crate::domain::service::plugin_service::PluginService;
use crate::domain::service::session_service::Identity;
use crate::domain::session::Session;

/// Capability name that gates writes to message reactions. Matches the
/// `feedback` token referenced in the Phase 9 brief and the
/// `cpt-cf-chat-engine-fr-message-feedback` requirement.
pub const CAPABILITY_FEEDBACK: &str = "feedback";

/// Response shape returned by [`ReactionService::set_reaction`]. Mirrors
/// `schemas/message/MessageReactionResponse.json` (`{message_id,
/// reaction_type, applied}`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetReactionResponse {
    pub message_id: Uuid,
    /// Echoes the request's `reaction_type`. For deletes this is
    /// [`ReactionType::None`] regardless of the prior value.
    pub reaction_type: ReactionType,
    /// True on successful create / update, true on a successful delete,
    /// false on a delete that found no prior row (idempotent no-op).
    pub applied: bool,
}

/// Listing returned by [`ReactionService::list_reactions`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct ReactionsListing {
    pub message_id: Uuid,
    pub reactions: Vec<MessageReaction>,
}

/// Reaction orchestration service.
///
/// Cheap to clone (all internal fields are `Arc`s).
#[domain_model]
#[derive(Clone)]
pub struct ReactionService {
    sessions: Arc<dyn SessionRepo>,
    session_types: Arc<dyn SessionTypeRepo>,
    messages: Arc<dyn MessageRepo>,
    reactions: Arc<dyn ReactionRepo>,
    plugins: PluginService,
}

impl ReactionService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        session_types: Arc<dyn SessionTypeRepo>,
        messages: Arc<dyn MessageRepo>,
        reactions: Arc<dyn ReactionRepo>,
        plugins: PluginService,
    ) -> Self {
        Self {
            sessions,
            session_types,
            messages,
            reactions,
            plugins,
        }
    }

    /// Apply a reaction (add / change / remove) to an assistant message.
    ///
    /// Returns the wire-shape response. The caller (REST handler) writes
    /// the response to the wire BEFORE awaiting the plugin notification
    /// task — see [`Self::spawn_plugin_notification`].
    #[instrument(skip(self), fields(
        session_id = %session_id,
        message_id = %message_id,
        reaction = reaction_type.as_str(),
    ))]
    pub async fn set_reaction(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
        reaction_type: ReactionType,
    ) -> Result<(SetReactionResponse, ReactionMutation)> {
        let started = Instant::now();

        let (session, _message) = self
            .validate_access_for_reaction_target(identity, session_id, message_id)
            .await?;

        // Capability gate is applied to WRITES only. The brief is
        // explicit: reads return an empty list when the feature is off,
        // so historical reactions remain visible after a session-type
        // switch.
        ensure_feedback_capability(&session)?;

        let (response, mutation) = match reaction_type {
            ReactionType::Like | ReactionType::Dislike => {
                let outcome = self
                    .reactions
                    .upsert(message_id, &identity.user_id, reaction_type)
                    .await?;
                let duration_ms = started.elapsed().as_millis() as u64;
                info!(
                    target: "chat_engine::reaction",
                    session_id = %session_id,
                    message_id = %message_id,
                    user_id = %identity.user_id,
                    reaction = reaction_type.as_str(),
                    previous = ?outcome.previous_reaction_type.as_ref().map(ReactionType::as_str),
                    duration_ms,
                    "reaction upserted"
                );
                (
                    SetReactionResponse {
                        message_id,
                        reaction_type,
                        applied: true,
                    },
                    ReactionMutation {
                        session_id,
                        message_id,
                        user_id: identity.user_id.clone(),
                        reaction_type,
                        previous_reaction_type: outcome.previous_reaction_type,
                        session_type_id: session.session_type_id,
                    },
                )
            }
            ReactionType::None => {
                let outcome = self.reactions.delete(message_id, &identity.user_id).await?;
                let duration_ms = started.elapsed().as_millis() as u64;
                info!(
                    target: "chat_engine::reaction",
                    session_id = %session_id,
                    message_id = %message_id,
                    user_id = %identity.user_id,
                    reaction = "none",
                    applied = outcome.applied,
                    previous = ?outcome.previous_reaction_type.as_ref().map(ReactionType::as_str),
                    duration_ms,
                    "reaction removed"
                );
                (
                    SetReactionResponse {
                        message_id,
                        reaction_type: ReactionType::None,
                        applied: outcome.applied,
                    },
                    ReactionMutation {
                        session_id,
                        message_id,
                        user_id: identity.user_id.clone(),
                        reaction_type: ReactionType::None,
                        previous_reaction_type: outcome.previous_reaction_type,
                        session_type_id: session.session_type_id,
                    },
                )
            }
        };

        Ok((response, mutation))
    }

    /// List every reaction on a message. The capability gate is NOT
    /// applied here — once a reaction exists, the owner can always read
    /// it back.
    #[instrument(skip(self), fields(
        session_id = %session_id,
        message_id = %message_id,
    ))]
    pub async fn list_reactions(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<ReactionsListing> {
        let _ = self
            .validate_access_for_reaction_target(identity, session_id, message_id)
            .await?;
        let reactions = self.reactions.list_by_message(message_id).await?;
        Ok(ReactionsListing {
            message_id,
            reactions,
        })
    }

    /// Fire the `message.reaction` event to the backend plugin.
    ///
    /// Spawned by the REST handler AFTER the HTTP response is built; the
    /// returned [`JoinHandle`] is intentionally dropped so the task is
    /// detached. Failures are logged at warning level (with `trace_id`,
    /// `session_id`, `message_id`, `reaction_type`) and never propagate.
    ///
    /// The SDK plugin trait does not yet declare an
    /// `on_message_reaction` method (no method exists in
    /// `chat_engine_sdk::plugin::ChatEngineBackendPlugin`); the task
    /// therefore resolves the plugin only to verify registration, then
    /// emits a structured `info!` event payload. Phase 14 may route the
    /// event through the live outbox once that surface lands.
    pub fn spawn_plugin_notification(&self, mutation: ReactionMutation) -> JoinHandle<()> {
        let session_types = Arc::clone(&self.session_types);
        let plugins = self.plugins.clone();

        tokio::spawn(async move {
            let event = MessageReactionEvent::new(
                mutation.session_id,
                mutation.message_id,
                mutation.user_id.clone(),
                mutation.reaction_type,
                mutation.previous_reaction_type,
            );

            // Resolve the plugin via session_type → plugin_instance_id.
            let Some(session_type_id) = mutation.session_type_id else {
                info!(
                    target: "chat_engine::reaction::notify",
                    session_id = %mutation.session_id,
                    message_id = %mutation.message_id,
                    "no session_type bound; skipping fire-and-forget reaction event"
                );
                return;
            };

            let plugin_instance_id = match session_types.find_by_id(session_type_id).await {
                Ok(Some(st)) => st.plugin_instance_id,
                Ok(None) => None,
                Err(err) => {
                    warn!(
                        target: "chat_engine::reaction::notify",
                        session_id = %mutation.session_id,
                        message_id = %mutation.message_id,
                        error = %err,
                        "failed to resolve session_type for plugin notification (swallowed)"
                    );
                    return;
                }
            };

            let Some(plugin_instance_id) = plugin_instance_id else {
                info!(
                    target: "chat_engine::reaction::notify",
                    session_id = %mutation.session_id,
                    message_id = %mutation.message_id,
                    "session_type has no plugin_instance_id; skipping reaction event"
                );
                return;
            };

            // Resolve the plugin only to confirm it is registered. The
            // actual `on_message_reaction` SDK method does not exist yet
            // (Phase 14 / future SDK bump), so deliver the event via a
            // structured log line — failures (plugin unregistered) are
            // logged at warning level per ADR-0020.
            match plugins.resolve(&plugin_instance_id) {
                Ok(_plugin) => {
                    let payload = serde_json::to_value(&event).unwrap_or(JsonValue::Null);
                    info!(
                        target: "chat_engine::reaction::notify",
                        plugin_instance_id = %plugin_instance_id,
                        session_id = %mutation.session_id,
                        message_id = %mutation.message_id,
                        reaction = mutation.reaction_type.as_str(),
                        event = MessageReactionEvent::EVENT_KIND,
                        payload = %payload,
                        "fire-and-forget reaction event ready (plugin resolved)"
                    );
                }
                Err(err) => {
                    warn!(
                        target: "chat_engine::reaction::notify",
                        plugin_instance_id = %plugin_instance_id,
                        session_id = %mutation.session_id,
                        message_id = %mutation.message_id,
                        reaction = mutation.reaction_type.as_str(),
                        error = %err,
                        "failed to resolve plugin for reaction event (swallowed)"
                    );
                }
            }
        })
    }

    /// Combined ownership + assistant-target validation. Returns the
    /// session row and the message domain object. Cross-tenant /
    /// missing-session / wrong-tenant collapse to
    /// [`ChatEngineError::NotFound { resource: "session", .. }`]; an
    /// unrelated message id collapses to
    /// [`ChatEngineError::NotFound { resource: "message", .. }`]. The
    /// 404-on-cross-tenant rule mirrors ADR-0021 anti-enumeration.
    async fn validate_access_for_reaction_target(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<(Session, crate::domain::message::Message)> {
        let session_row = self
            .sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        let session = session_row;

        let message = self
            .messages
            .find_message_in_session(session_id, message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", message_id))?;

        if !matches!(message.role, MessageRole::Assistant) {
            return Err(ChatEngineError::bad_request(
                "reactions are only allowed on assistant messages",
            ));
        }

        Ok((session, message))
    }
}

/// Mutation payload returned alongside the wire response so the REST
/// handler can hand it to [`ReactionService::spawn_plugin_notification`]
/// AFTER the response is built.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ReactionMutation {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub user_id: String,
    pub reaction_type: ReactionType,
    pub previous_reaction_type: Option<ReactionType>,
    pub session_type_id: Option<Uuid>,
}

/// Capability gate. Inspects `session.enabled_capabilities` (JSONB array
/// of `{name, value}` objects, per the Phase 4 capability writer) for a
/// capability named `"feedback"`. Absence is mapped to
/// [`ChatEngineError::Conflict`] which the handler renders as HTTP 409
/// with body `{"error": "capability_disabled", "capability": "feedback"}`.
fn ensure_feedback_capability(session: &Session) -> Result<()> {
    let JsonValue::Array(arr) = session
        .enabled_capabilities
        .as_ref()
        .unwrap_or(&JsonValue::Null)
    else {
        return Err(ChatEngineError::conflict(
            "feature 'feedback' is disabled for this session type",
        ));
    };

    let has_feedback = arr.iter().any(|entry| {
        entry
            .get("name")
            .and_then(JsonValue::as_str)
            .is_some_and(|n| n == CAPABILITY_FEEDBACK)
    });

    if has_feedback {
        Ok(())
    } else {
        Err(ChatEngineError::conflict(
            "feature 'feedback' is disabled for this session type",
        ))
    }
}

#[cfg(test)]
#[path = "reaction_service_tests.rs"]
mod reaction_service_tests;
