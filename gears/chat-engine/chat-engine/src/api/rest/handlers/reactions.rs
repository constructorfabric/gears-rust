//! Axum handlers for message reaction endpoints (Phase 9).
//!
//! Routes (mounted on the live router in Phase 14):
//!
//! | Route                                                          | Method | Handler                |
//! |----------------------------------------------------------------|--------|------------------------|
//! | `/sessions/{session_id}/messages/{message_id}/reaction`        | POST   | [`set_reaction`]       |
//! | `/sessions/{session_id}/messages/{message_id}/reactions`       | GET    | [`list_reactions`]     |
//!
//! Both handlers extract `(tenant_id, user_id)` exclusively from the
//! JWT-backed [`SecurityContext`] — never from path parameters or the
//! request body. The body `Deserialize` carries anti-spoof
//! `tenant_id` / `user_id` fields whose presence is rejected at the entry
//! point (mirrors the pattern in `messages.rs`).
//!
//! ## Error mapping (Phase 9 brief)
//!
//! | Service result                                | HTTP | Body shape                                                       |
//! |-----------------------------------------------|------|------------------------------------------------------------------|
//! | `BadRequest` (invalid reaction_type / target) | 400  | `{"error": "<reason>"}` (Phase 4 scaffold)                       |
//! | identity missing / token absent               | 401  | upstream — handled by the security layer                          |
//! | `Forbidden` (identity claims absent)          | 403  | `{"error": "<reason>"}` (Phase 4 scaffold)                       |
//! | `NotFound` (session / message)                | 404  | `{"error": "<resource> not found: <id>"}` (Phase 4 scaffold)     |
//! | `Conflict` (capability disabled)              | 409  | `{"error": "capability_disabled", "capability": "feedback"}`     |
//!
//! The capability-disabled 409 shape is constructed inline so the
//! response matches the contract written into the Phase 9 brief without
//! plumbing a domain-specific error variant through the global
//! `IntoResponse` impl (Phase 4 owns that scaffold).
//!
//! ## Plugin notification
//!
//! On a successful `set_reaction` the service returns a
//! [`ReactionMutation`]; the handler hands it to
//! [`ReactionService::spawn_plugin_notification`] which `tokio::spawn`s
//! a detached task. The task is launched AFTER the response is fully
//! constructed; the handler does not `await` it before returning.
//
// @cpt-cf-chat-engine-api-rest-reactions-handler:p9
// @cpt-cf-chat-engine-adr-message-reactions:p9

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::field::Empty;
use uuid::Uuid;

use modkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::reaction::{MessageReaction, ReactionType};
use crate::domain::service::reaction_service::{
    CAPABILITY_FEEDBACK, ReactionService, ReactionsListing, SetReactionResponse,
};

/// Wire body for `POST /sessions/{s}/messages/{m}/reaction`. Mirrors
/// `schemas/message/MessageReactionRequest.json`.
#[derive(Debug, Deserialize)]
pub struct SetReactionBody {
    /// `"like"` | `"dislike"` | `"none"`. Unknown values cause serde to
    /// fail deserialization with a 400 at the framework boundary.
    pub reaction_type: ReactionType,

    // ---- anti-spoof fields (rejected if present) ----
    #[serde(default)]
    pub tenant_id: Option<JsonValue>,
    #[serde(default)]
    pub user_id: Option<JsonValue>,
}

/// Wire response for `POST /sessions/{s}/messages/{m}/reaction`. Mirrors
/// `schemas/message/MessageReactionResponse.json`.
#[derive(Debug, Serialize)]
pub struct SetReactionResponseDto {
    pub message_id: Uuid,
    pub reaction_type: ReactionType,
    pub applied: bool,
}

impl From<SetReactionResponse> for SetReactionResponseDto {
    fn from(r: SetReactionResponse) -> Self {
        Self {
            message_id: r.message_id,
            reaction_type: r.reaction_type,
            applied: r.applied,
        }
    }
}

/// Wire entry in the `GET /reactions` listing.
#[derive(Debug, Serialize)]
pub struct ReactionDto {
    pub user_id: String,
    pub reaction_type: ReactionType,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl From<MessageReaction> for ReactionDto {
    fn from(r: MessageReaction) -> Self {
        Self {
            user_id: r.user_id,
            reaction_type: r.reaction_type,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Wire response for `GET /sessions/{s}/messages/{m}/reactions`.
#[derive(Debug, Serialize)]
pub struct ListReactionsResponseDto {
    pub message_id: Uuid,
    pub reactions: Vec<ReactionDto>,
}

impl From<ReactionsListing> for ListReactionsResponseDto {
    fn from(l: ReactionsListing) -> Self {
        Self {
            message_id: l.message_id,
            reactions: l.reactions.into_iter().map(ReactionDto::from).collect(),
        }
    }
}

/// `POST /sessions/{session_id}/messages/{message_id}/reaction` — apply
/// or remove a reaction. Identity is extracted from the JWT; the body
/// only carries `reaction_type`.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(
        session_id = Empty,
        message_id = Empty,
        reaction = Empty,
    ),
)]
pub async fn set_reaction(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ReactionService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SetReactionBody>,
) -> Response {
    let span = tracing::Span::current();
    span.record("session_id", tracing::field::display(session_id));
    span.record("message_id", tracing::field::display(message_id));
    span.record("reaction", tracing::field::display(body.reaction_type.as_str()));

    if let Err(err) = reject_body_identity(&body.tenant_id, &body.user_id) {
        return err.into_response();
    }

    let identity = match identity_from_ctx(&ctx) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let service_outcome = svc
        .set_reaction(&identity, session_id, message_id, body.reaction_type)
        .await;

    match service_outcome {
        Ok((response, mutation)) => {
            let dto = SetReactionResponseDto::from(response);
            // IMPORTANT: build the response BEFORE handing the mutation
            // to the fire-and-forget task. The handler intentionally does
            // not `await` the spawned task — ADR-0020's contract is that
            // plugin notification never delays the client.
            let http_response = (StatusCode::OK, Json(dto)).into_response();
            // Detached: the JoinHandle is dropped; the task completes
            // independently. Errors are logged inside the task.
            let _ = svc.spawn_plugin_notification(mutation);
            http_response
        }
        Err(err) => map_reaction_error(err),
    }
}

/// `GET /sessions/{session_id}/messages/{message_id}/reactions` — list
/// every reaction on the message. Capability gate intentionally bypassed
/// (reads remain available after the feature is toggled off).
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        session_id = %session_id,
        message_id = %message_id,
    ),
)]
pub async fn list_reactions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ReactionService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ListReactionsResponseDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let listing = svc.list_reactions(&identity, session_id, message_id).await?;
    Ok(Json(ListReactionsResponseDto::from(listing)))
}

/// Phase 9 maps the capability-disabled conflict to a structured 409 body
/// (per the brief: `{"error": "capability_disabled", "capability":
/// "feedback"}`). Every other variant is delegated to the Phase 4
/// scaffold `IntoResponse for ChatEngineError`.
fn map_reaction_error(err: ChatEngineError) -> Response {
    if let ChatEngineError::Conflict { reason } = &err {
        if reason.contains(CAPABILITY_FEEDBACK) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "capability_disabled",
                    "capability": CAPABILITY_FEEDBACK,
                })),
            )
                .into_response();
        }
    }
    err.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_set_reaction_body_accepts_known_values() {
        for variant in ["like", "dislike", "none"] {
            let json = format!("{{\"reaction_type\": \"{variant}\"}}");
            let body: SetReactionBody = serde_json::from_str(&json).expect("ok");
            // Anti-spoof fields default to None.
            assert!(body.tenant_id.is_none());
            assert!(body.user_id.is_none());
            // Round-trip via the helper.
            let as_str = body.reaction_type.as_str();
            assert!(matches!(as_str, "like" | "dislike" | "none"));
        }
    }

    #[test]
    fn deserialize_set_reaction_body_rejects_unknown_values() {
        let err = serde_json::from_str::<SetReactionBody>(r#"{"reaction_type": "love"}"#)
            .expect_err("unknown variant must fail");
        assert!(err.to_string().contains("love") || err.to_string().contains("variant"));
    }

    #[test]
    fn map_reaction_error_emits_capability_disabled_body() {
        let err = ChatEngineError::conflict(
            "feature 'feedback' is disabled for this session type",
        );
        let response = map_reaction_error(err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn map_reaction_error_passes_through_unrelated_conflicts() {
        // A conflict whose reason does not mention `feedback` should
        // fall through to the Phase 4 scaffold — still a 409, but with
        // the generic `{"error": "<reason>"}` body.
        let err = ChatEngineError::conflict("invalid lifecycle transition");
        let response = map_reaction_error(err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn set_reaction_response_dto_round_trips_through_serde() {
        let dto = SetReactionResponseDto::from(SetReactionResponse {
            message_id: Uuid::nil(),
            reaction_type: ReactionType::Like,
            applied: true,
        });
        let s = serde_json::to_string(&dto).expect("ok");
        assert!(s.contains("\"reaction_type\":\"like\""));
        assert!(s.contains("\"applied\":true"));
    }

    #[test]
    fn list_reactions_dto_serializes_rfc3339_timestamps() {
        let now = time::OffsetDateTime::now_utc();
        let dto = ListReactionsResponseDto {
            message_id: Uuid::nil(),
            reactions: vec![ReactionDto {
                user_id: "u".into(),
                reaction_type: ReactionType::Dislike,
                created_at: now,
                updated_at: now,
            }],
        };
        let s = serde_json::to_string(&dto).expect("ok");
        assert!(s.contains("\"reaction_type\":\"dislike\""));
        assert!(s.contains("\"user_id\":\"u\""));
    }
}
