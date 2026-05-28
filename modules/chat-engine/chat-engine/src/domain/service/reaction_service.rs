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

use modkit_macros::domain_model;
use serde_json::Value as JsonValue;
use tokio::task::JoinHandle;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::MessageRole;
use crate::domain::reaction::{MessageReaction, MessageReactionEvent, ReactionType};
use crate::domain::service::plugin_service::PluginService;
use crate::domain::service::session_service::Identity;
use crate::domain::session::Session;
use crate::infra::db::repo::message_repo::MessageRepo;
use crate::infra::db::repo::reaction_repo::ReactionRepo;
use crate::infra::db::repo::session_repo::SessionRepo;
use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

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
        let session = Session::from(session_row);

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
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chat_engine_sdk::models::LifecycleState;
    use modkit::ClientHub;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use time::OffsetDateTime;

    use crate::domain::message::{Message, MessageRole};
    use crate::infra::db::entity::{
        session as session_entity, session_type as session_type_entity,
    };
    use crate::infra::db::repo::message_repo::{
        FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage,
    };
    use crate::infra::db::repo::plugin_config_repo::PluginConfigRepo;
    use crate::infra::db::repo::reaction_repo::{
        ReactionDeleteOutcome, ReactionRepo, ReactionUpsertOutcome,
    };
    use crate::infra::db::repo::session_repo::{SessionPage, SessionRepo};
    use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

    // ----------------------------- Stubs ----------------------------------

    struct StubSessionRepo {
        session: Mutex<session_entity::Model>,
    }

    impl StubSessionRepo {
        fn new(session: session_entity::Model) -> Arc<Self> {
            Arc::new(Self {
                session: Mutex::new(session),
            })
        }
    }

    #[async_trait]
    impl SessionRepo for StubSessionRepo {
        async fn insert(
            &self,
            _m: session_entity::ActiveModel,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Ok(self.session.lock().clone())
        }

        async fn find_by_id(
            &self,
            tenant_id: &str,
            user_id: &str,
            session_id: Uuid,
        ) -> std::result::Result<Option<session_entity::Model>, ChatEngineError> {
            let s = self.session.lock().clone();
            if s.tenant_id == tenant_id && s.user_id == user_id && s.session_id == session_id {
                Ok(Some(s))
            } else {
                Ok(None)
            }
        }

        async fn list_paginated(
            &self,
            _t: &str,
            _u: &str,
            _c: Option<&str>,
            _l: u32,
        ) -> std::result::Result<SessionPage, ChatEngineError> {
            Ok(SessionPage {
                items: vec![],
                next_cursor: None,
            })
        }

        async fn update_metadata(
            &self,
            _t: &str,
            _u: &str,
            _i: Uuid,
            _m: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Ok(self.session.lock().clone())
        }

        async fn update_capabilities(
            &self,
            _t: &str,
            _u: &str,
            _i: Uuid,
            _c: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Ok(self.session.lock().clone())
        }

        async fn update_lifecycle_state(
            &self,
            _t: &str,
            _u: &str,
            _i: Uuid,
            _s: LifecycleState,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Ok(self.session.lock().clone())
        }

        async fn soft_delete(
            &self,
            _t: &str,
            _u: &str,
            _i: Uuid,
            _d: i64,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Ok(self.session.lock().clone())
        }

        async fn hard_delete(
            &self,
            _t: &str,
            _u: &str,
            _i: Uuid,
        ) -> std::result::Result<bool, ChatEngineError> {
            Ok(true)
        }
    }

    struct StubSessionTypeRepo;

    #[async_trait]
    impl SessionTypeRepo for StubSessionTypeRepo {
        async fn insert(
            &self,
            _m: session_type_entity::ActiveModel,
        ) -> std::result::Result<session_type_entity::Model, ChatEngineError> {
            unreachable!()
        }

        async fn find_by_id(
            &self,
            _id: Uuid,
        ) -> std::result::Result<Option<session_type_entity::Model>, ChatEngineError> {
            Ok(None)
        }

        async fn list(
            &self,
        ) -> std::result::Result<Vec<session_type_entity::Model>, ChatEngineError> {
            Ok(vec![])
        }
    }

    struct StubMessageRepo {
        message: Mutex<Option<Message>>,
    }

    impl StubMessageRepo {
        fn assistant(session_id: Uuid, message_id: Uuid) -> Arc<Self> {
            let now = OffsetDateTime::now_utc();
            let msg = Message {
                message_id,
                session_id,
                parent_message_id: None,
                variant_index: 0,
                is_active: true,
                role: MessageRole::Assistant,
                content: serde_json::json!({"text": "hi"}),
                file_ids: vec![],
                metadata: None,
                is_complete: true,
                is_hidden_from_user: false,
                is_hidden_from_backend: false,
                created_at: now,
                updated_at: now,
            };
            Arc::new(Self {
                message: Mutex::new(Some(msg)),
            })
        }

        fn user(session_id: Uuid, message_id: Uuid) -> Arc<Self> {
            let now = OffsetDateTime::now_utc();
            let msg = Message {
                message_id,
                session_id,
                parent_message_id: None,
                variant_index: 0,
                is_active: true,
                role: MessageRole::User,
                content: serde_json::json!({"text": "hi"}),
                file_ids: vec![],
                metadata: None,
                is_complete: true,
                is_hidden_from_user: false,
                is_hidden_from_backend: false,
                created_at: now,
                updated_at: now,
            };
            Arc::new(Self {
                message: Mutex::new(Some(msg)),
            })
        }
    }

    #[async_trait]
    impl MessageRepo for StubMessageRepo {
        async fn insert_user_and_assistant_stub(
            &self,
            _req: NewUserMessage,
        ) -> std::result::Result<InsertedPair, ChatEngineError> {
            unreachable!()
        }

        async fn finalize_assistant(
            &self,
            _id: Uuid,
            _outcome: FinalizeOutcome,
        ) -> std::result::Result<(), ChatEngineError> {
            unreachable!()
        }

        async fn fetch_active_history(
            &self,
            _s: Uuid,
            _d: Option<u32>,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            Ok(vec![])
        }

        async fn find_message_in_session(
            &self,
            session_id: Uuid,
            message_id: Uuid,
        ) -> std::result::Result<Option<Message>, ChatEngineError> {
            let m = self.message.lock().clone();
            Ok(m.filter(|msg| msg.session_id == session_id && msg.message_id == message_id))
        }
    }

    #[derive(Default)]
    struct StubReactionRepo {
        upsert_calls: AtomicUsize,
        delete_calls: AtomicUsize,
        list_returns: Mutex<Vec<MessageReaction>>,
    }

    #[async_trait]
    impl ReactionRepo for StubReactionRepo {
        async fn get_by_pk(
            &self,
            _message_id: Uuid,
            _user_id: &str,
        ) -> std::result::Result<Option<MessageReaction>, ChatEngineError> {
            Ok(None)
        }

        async fn upsert(
            &self,
            message_id: Uuid,
            user_id: &str,
            reaction_type: ReactionType,
        ) -> std::result::Result<ReactionUpsertOutcome, ChatEngineError> {
            self.upsert_calls.fetch_add(1, Ordering::SeqCst);
            let now = OffsetDateTime::now_utc();
            Ok(ReactionUpsertOutcome {
                reaction: MessageReaction {
                    message_id,
                    user_id: user_id.to_owned(),
                    reaction_type,
                    created_at: now,
                    updated_at: now,
                },
                previous_reaction_type: None,
            })
        }

        async fn delete(
            &self,
            _message_id: Uuid,
            _user_id: &str,
        ) -> std::result::Result<ReactionDeleteOutcome, ChatEngineError> {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ReactionDeleteOutcome {
                applied: true,
                previous_reaction_type: Some(ReactionType::Like),
            })
        }

        async fn list_by_message(
            &self,
            _message_id: Uuid,
        ) -> std::result::Result<Vec<MessageReaction>, ChatEngineError> {
            Ok(self.list_returns.lock().clone())
        }
    }

    struct StubPluginConfigRepo;

    #[async_trait]
    impl PluginConfigRepo for StubPluginConfigRepo {
        async fn find(
            &self,
            _p: &str,
            _s: Uuid,
        ) -> std::result::Result<Option<JsonValue>, ChatEngineError> {
            Ok(None)
        }

        async fn upsert(
            &self,
            _p: &str,
            _s: Uuid,
            _c: JsonValue,
        ) -> std::result::Result<(), ChatEngineError> {
            Ok(())
        }

        async fn delete(
            &self,
            _p: &str,
            _s: Uuid,
        ) -> std::result::Result<(), ChatEngineError> {
            Ok(())
        }
    }

    fn make_session(
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        enabled_capabilities: Option<JsonValue>,
    ) -> session_entity::Model {
        let now = OffsetDateTime::now_utc();
        session_entity::Model {
            session_id,
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities,
            metadata: None,
            lifecycle_state: "active".into(),
            share_token: None,
            deleted_at: None,
            scheduled_hard_delete_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn plugin_service() -> PluginService {
        PluginService::new(Arc::new(ClientHub::new()), Arc::new(StubPluginConfigRepo))
    }

    fn make_service(
        sessions: Arc<dyn SessionRepo>,
        messages: Arc<dyn MessageRepo>,
        reactions: Arc<dyn ReactionRepo>,
    ) -> ReactionService {
        ReactionService::new(
            sessions,
            Arc::new(StubSessionTypeRepo),
            messages,
            reactions,
            plugin_service(),
        )
    }

    fn identity() -> Identity {
        Identity::new("t", "u", None).expect("identity")
    }

    // --------------------------- Unit tests -------------------------------

    #[tokio::test]
    async fn set_reaction_returns_409_when_feedback_capability_missing() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "model", "value": "gpt-4" }])),
        );
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, message_id),
            Arc::new(StubReactionRepo::default()),
        );

        let err = svc
            .set_reaction(&identity(), session_id, message_id, ReactionType::Like)
            .await
            .expect_err("capability gate must reject");
        match err {
            ChatEngineError::Conflict { reason } => {
                assert!(reason.contains("feedback"), "reason mentions capability");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_reaction_upserts_when_capability_enabled() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "feedback", "value": true }])),
        );
        let reactions = Arc::new(StubReactionRepo::default());
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, message_id),
            reactions.clone(),
        );

        let (resp, mutation) = svc
            .set_reaction(&identity(), session_id, message_id, ReactionType::Like)
            .await
            .expect("ok");
        assert_eq!(resp.message_id, message_id);
        assert_eq!(resp.reaction_type, ReactionType::Like);
        assert!(resp.applied);
        assert_eq!(reactions.upsert_calls.load(Ordering::SeqCst), 1);
        assert_eq!(mutation.reaction_type, ReactionType::Like);
    }

    #[tokio::test]
    async fn set_reaction_deletes_on_none_with_applied_true() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "feedback", "value": true }])),
        );
        let reactions = Arc::new(StubReactionRepo::default());
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, message_id),
            reactions.clone(),
        );

        let (resp, mutation) = svc
            .set_reaction(&identity(), session_id, message_id, ReactionType::None)
            .await
            .expect("ok");
        assert_eq!(resp.reaction_type, ReactionType::None);
        assert!(resp.applied);
        assert_eq!(reactions.delete_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mutation.previous_reaction_type,
            Some(ReactionType::Like)
        );
    }

    #[tokio::test]
    async fn set_reaction_returns_404_on_unknown_session() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        // Session repo holds a *different* tenant — find_by_id returns None.
        let session = make_session(
            "other-tenant",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "feedback", "value": true }])),
        );
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, message_id),
            Arc::new(StubReactionRepo::default()),
        );

        let err = svc
            .set_reaction(&identity(), session_id, message_id, ReactionType::Like)
            .await
            .expect_err("cross-tenant collapses to 404");
        assert!(matches!(
            err,
            ChatEngineError::NotFound { resource: "session", .. }
        ));
    }

    #[tokio::test]
    async fn set_reaction_returns_400_on_non_assistant_target() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "feedback", "value": true }])),
        );
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::user(session_id, message_id),
            Arc::new(StubReactionRepo::default()),
        );

        let err = svc
            .set_reaction(&identity(), session_id, message_id, ReactionType::Like)
            .await
            .expect_err("user-message target must be rejected");
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn list_reactions_bypasses_capability_gate() {
        let session_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        // No feedback capability — the read path must still succeed.
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "model", "value": "gpt-4" }])),
        );
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, message_id),
            Arc::new(StubReactionRepo::default()),
        );

        let listing = svc
            .list_reactions(&identity(), session_id, message_id)
            .await
            .expect("ok");
        assert_eq!(listing.message_id, message_id);
        assert!(listing.reactions.is_empty());
    }

    #[tokio::test]
    async fn list_reactions_404_on_missing_message() {
        let session_id = Uuid::new_v4();
        let session = make_session(
            "t",
            "u",
            session_id,
            Some(serde_json::json!([{ "name": "feedback", "value": true }])),
        );
        // Stub returns the session but `find_message_in_session` rejects
        // any UUID it didn't ingest at construction time.
        let svc = make_service(
            StubSessionRepo::new(session),
            StubMessageRepo::assistant(session_id, Uuid::new_v4()),
            Arc::new(StubReactionRepo::default()),
        );

        let err = svc
            .list_reactions(&identity(), session_id, Uuid::new_v4())
            .await
            .expect_err("unknown message must be 404");
        assert!(matches!(
            err,
            ChatEngineError::NotFound { resource: "message", .. }
        ));
    }

    #[test]
    fn ensure_feedback_capability_passes_when_present() {
        let now = OffsetDateTime::now_utc();
        let session = Session {
            session_id: Uuid::nil(),
            tenant_id: "t".to_string().into(),
            user_id: "u".to_string().into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: Some(serde_json::json!([
                { "name": "model", "value": "gpt-4" },
                { "name": "feedback", "value": true },
            ])),
            metadata: None,
            lifecycle_state: LifecycleState::Active,
            share_token: None,
            created_at: now,
            updated_at: now,
        };
        ensure_feedback_capability(&session).expect("passes");
    }

    #[test]
    fn ensure_feedback_capability_rejects_when_array_missing() {
        let now = OffsetDateTime::now_utc();
        let session = Session {
            session_id: Uuid::nil(),
            tenant_id: "t".to_string().into(),
            user_id: "u".to_string().into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: None,
            lifecycle_state: LifecycleState::Active,
            share_token: None,
            created_at: now,
            updated_at: now,
        };
        let err = ensure_feedback_capability(&session).unwrap_err();
        assert!(matches!(err, ChatEngineError::Conflict { .. }));
    }
}
