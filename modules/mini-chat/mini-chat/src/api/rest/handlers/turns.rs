use std::sync::Arc;
use std::time::Duration;

use axum::extract::Path;
use axum::response::sse::KeepAlive;
use axum::response::{IntoResponse, Response, Sse};
use axum::{Extension, Json};
use modkit::api::prelude::*;
use modkit_security::SecurityContext;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, info, warn};
use utoipa::ToSchema;

use super::messages::SseRelay;
use crate::domain::service::{MutationError, StreamError};
use crate::domain::stream_events::StreamEvent;
use crate::infra::db::entity::chat_turn::TurnState;
use crate::module::AppServices;

// ════════════════════════════════════════════════════════════════════════════
// GET turn status
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct TurnStatusResponse {
    request_id: uuid::Uuid,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assistant_message_id: Option<uuid::Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: time::OffsetDateTime,
}

fn map_turn_state(state: &TurnState) -> &'static str {
    match state {
        TurnState::Running => "running",
        TurnState::Completed => "done",
        TurnState::Failed => "error",
        TurnState::Cancelled => "cancelled",
    }
}

/// GET /mini-chat/v1/chats/{id}/turns/{request_id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, turn_request_id = %request_id))]
pub(crate) async fn get_turn(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, request_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<Json<TurnStatusResponse>> {
    let turn = svc
        .turns
        .get(&ctx, chat_id, request_id)
        .await
        .map_err(|e| mutation_error_to_problem(&e))?;

    Ok(Json(TurnStatusResponse {
        request_id: turn.request_id,
        state: map_turn_state(&turn.state).to_owned(),
        error_code: turn.error_code.clone(),
        assistant_message_id: turn.assistant_message_id,
        updated_at: turn.updated_at,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// DELETE turn
// ════════════════════════════════════════════════════════════════════════════

/// DELETE /mini-chat/v1/chats/{id}/turns/{request_id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, turn_request_id = %request_id))]
pub(crate) async fn delete_turn(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, request_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.turns
        .delete(&ctx, chat_id, request_id)
        .await
        .map_err(|e| mutation_error_to_problem(&e))?;

    Ok(no_content().into_response())
}

// ════════════════════════════════════════════════════════════════════════════
// POST retry turn
// ════════════════════════════════════════════════════════════════════════════

/// POST /mini-chat/v1/chats/{id}/turns/{request_id}/retry
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, turn_request_id = %request_id))]
pub(crate) async fn retry_turn(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, request_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> Response {
    let mutation = match svc.turns.retry(&ctx, chat_id, request_id).await {
        Ok(m) => m,
        Err(e) => return mutation_error_to_problem(&e).into_response(),
    };

    start_mutation_stream(&svc, ctx, chat_id, mutation).await
}

// ════════════════════════════════════════════════════════════════════════════
// PATCH edit turn
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, ToSchema)]
pub struct EditTurnRequest {
    pub content: String,
}

impl modkit::api::api_dto::RequestApiDto for EditTurnRequest {}

/// PATCH /mini-chat/v1/chats/{id}/turns/{request_id}
#[tracing::instrument(skip(svc, ctx, body), fields(chat_id = %chat_id, turn_request_id = %request_id))]
pub(crate) async fn edit_turn(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, request_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    Json(body): Json<EditTurnRequest>,
) -> Response {
    if body.content.trim().is_empty() {
        return Problem::new(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "Edit content must not be empty",
        )
        .into_response();
    }

    let mutation = match svc
        .turns
        .edit(&ctx, chat_id, request_id, body.content)
        .await
    {
        Ok(m) => m,
        Err(e) => return mutation_error_to_problem(&e).into_response(),
    };

    start_mutation_stream(&svc, ctx, chat_id, mutation).await
}

// ════════════════════════════════════════════════════════════════════════════
// Shared helpers
// ════════════════════════════════════════════════════════════════════════════

#[allow(clippy::cognitive_complexity)]
async fn start_mutation_stream(
    svc: &AppServices,
    ctx: SecurityContext,
    chat_id: uuid::Uuid,
    mutation: crate::domain::service::MutationResult,
) -> Response {
    let chat_model = mutation.chat_model.clone();
    let resolved = match svc
        .models
        .resolve_model(ctx.subject_id(), Some(mutation.chat_model))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, model = %chat_model, "model resolution failed for mutation stream");
            return Problem::new(StatusCode::BAD_REQUEST, "Bad Request", e.to_string())
                .into_response();
        }
    };

    let capacity = svc.stream.channel_capacity();
    let ping_secs = svc.stream.ping_interval_secs();
    let (tx, rx) = mpsc::channel::<StreamEvent>(capacity);
    let cancel = CancellationToken::new();

    info!(
        chat_id = %chat_id,
        new_request_id = %mutation.new_request_id,
        model = %resolved.model_id,
        "starting mutation SSE stream"
    );

    let provider_handle = match svc
        .stream
        .run_stream_for_mutation(
            ctx,
            chat_id,
            mutation.new_request_id,
            mutation.new_turn_id,
            mutation.user_content,
            resolved,
            false, // web_search_enabled: retry/edit defaults to disabled
            mutation.snapshot_boundary,
            cancel.clone(),
            tx,
        )
        .await
    {
        Ok(handle) => handle,
        Err(e) => return stream_error_to_response(&e),
    };

    let monitor_span = tracing::Span::current();
    tokio::spawn(
        async move {
            if let Err(e) = provider_handle.await {
                tracing::error!(error = ?e, "provider task panicked");
            }
        }
        .instrument(monitor_span),
    );

    let relay = SseRelay::new(rx, cancel, ping_secs);
    Sse::new(relay)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

/// Map `MutationError` to HTTP problem response.
///
/// Caller is expected to be within an instrumented span that carries
/// `chat_id` and `turn_request_id` fields.
fn mutation_error_to_problem(err: &MutationError) -> Problem {
    match err {
        MutationError::ChatNotFound { .. } => {
            Problem::new(StatusCode::NOT_FOUND, "chat_not_found", "Chat not found")
        }
        MutationError::TurnNotFound { .. } => {
            Problem::new(StatusCode::NOT_FOUND, "turn_not_found", "Turn not found")
        }
        MutationError::Forbidden => {
            warn!("access denied for turn mutation");
            Problem::new(StatusCode::FORBIDDEN, "forbidden", "Access denied")
        }
        MutationError::InvalidTurnState { state } => Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_turn_state",
            format!("Turn is in {state:?} state; only terminal turns can be mutated"),
        ),
        MutationError::NotLatestTurn => Problem::new(
            StatusCode::CONFLICT,
            "not_latest_turn",
            "Only the most recent turn can be mutated",
        ),
        MutationError::GenerationInProgress => Problem::new(
            StatusCode::CONFLICT,
            "generation_in_progress",
            "Another generation is already in progress for this chat",
        ),
        MutationError::Internal { message } => {
            warn!(error_message = %message, "turn mutation internal error");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                "An internal error occurred",
            )
        }
    }
}

/// Caller is expected to be within an instrumented span that carries
/// `chat_id` and `turn_request_id` fields.
#[allow(clippy::cognitive_complexity)]
fn stream_error_to_response(err: &StreamError) -> Response {
    match err {
        StreamError::QuotaExhausted {
            error_code,
            http_status,
            quota_scope,
        } => {
            info!(error_code = %error_code, http_status = *http_status, quota_scope = %quota_scope, "quota exhausted, mutation rejected");
            let status =
                StatusCode::from_u16(*http_status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
            // TODO(P2): include `quota_scope` in the response body so clients can
            // distinguish token vs web_search quota exhaustion (DESIGN.md §5.2).
            Problem::new(status, error_code, error_code).into_response()
        }
        StreamError::WebSearchDisabled => {
            info!(
                reason = "kill_switch",
                "web search disabled via kill switch, mutation rejected"
            );
            Problem::new(
                StatusCode::BAD_REQUEST,
                "web_search_disabled",
                "Web search is currently disabled",
            )
            .into_response()
        }
        StreamError::InvalidAttachment { code, message } => {
            info!(code = %code, message = %message, "invalid attachment in request");
            Problem::new(StatusCode::BAD_REQUEST, code, message).into_response()
        }
        StreamError::ContextBudgetExceeded {
            required_tokens,
            available_tokens,
        } => {
            info!(
                required_tokens,
                available_tokens, "context budget exceeded, request rejected"
            );
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "context_budget_exceeded",
                format!(
                    "Context requires {required_tokens} tokens but only {available_tokens} are available"
                ),
            )
            .into_response()
        }
        other => {
            warn!(error = ?other, "post-mutation stream error");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                "Failed to start streaming",
            )
            .into_response()
        }
    }
}
