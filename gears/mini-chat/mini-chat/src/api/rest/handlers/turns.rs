use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::KeepAlive;
use axum::response::{IntoResponse, Response, Sse};
use axum::{Extension, Json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use tracing::{Instrument, info, warn};

use super::messages::{SseRelay, run_to_completion};
use crate::api::rest::dto::{EditTurnRequest, TurnStatusResponse, TurnStatusState};
use crate::api::rest::error::MiniChatChatError;
use crate::domain::stream_events::StreamEvent;
use crate::gear::AppServices;
use crate::infra::db::entity::chat_turn::TurnState;

// ════════════════════════════════════════════════════════════════════════════
// GET turn status
// ════════════════════════════════════════════════════════════════════════════

fn map_turn_state(state: &TurnState) -> TurnStatusState {
    match state {
        TurnState::Running => TurnStatusState::Running,
        TurnState::Completed => TurnStatusState::Done,
        TurnState::Failed => TurnStatusState::Error,
        TurnState::Cancelled => TurnStatusState::Cancelled,
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
        .map_err(CanonicalError::from)?;

    Ok(Json(TurnStatusResponse {
        request_id: turn.request_id,
        state: map_turn_state(&turn.state),
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
        .map_err(CanonicalError::from)?;

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
    run_to_completion(
        async move { start_mutation_stream(&svc, ctx, chat_id, request_id, None).await },
    )
    .await
}

// ════════════════════════════════════════════════════════════════════════════
// PATCH edit turn
// ════════════════════════════════════════════════════════════════════════════

/// PATCH /mini-chat/v1/chats/{id}/turns/{request_id}
#[tracing::instrument(skip(svc, ctx, body), fields(chat_id = %chat_id, turn_request_id = %request_id))]
pub(crate) async fn edit_turn(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, request_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    extract::Json(body): extract::Json<EditTurnRequest>,
) -> Response {
    if body.content.trim().is_empty() {
        return MiniChatChatError::invalid_argument()
            .with_field_violation("content", "Edit content must not be empty", "EMPTY_CONTENT")
            .create()
            .into_response();
    }

    run_to_completion(async move {
        start_mutation_stream(&svc, ctx, chat_id, request_id, Some(body.content)).await
    })
    .await
}

// ════════════════════════════════════════════════════════════════════════════
// Shared helpers
// ════════════════════════════════════════════════════════════════════════════

/// Retry (`new_content = None`) or edit a turn and stream the new answer.
///
/// Order matters: the quota preflight runs before the mutation commits, so a
/// rejection returns a JSON error and leaves the previous turn untouched.
#[allow(clippy::cognitive_complexity)]
async fn start_mutation_stream(
    svc: &AppServices,
    ctx: SecurityContext,
    chat_id: uuid::Uuid,
    request_id: uuid::Uuid,
    new_content: Option<String>,
) -> Response {
    let preview = match svc
        .turns
        .preview_mutation(&ctx, chat_id, request_id, new_content.as_deref())
        .await
    {
        Ok(p) => p,
        Err(e) => return CanonicalError::from(e).into_response(),
    };

    let resolved = match svc
        .models
        .resolve_chat_model(ctx.subject_id(), &preview.chat_model)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, model = %preview.chat_model, "model resolution failed for mutation stream");
            return CanonicalError::from(e).into_response();
        }
    };

    let preflight = match svc
        .stream
        .preflight_mutation(
            &ctx,
            chat_id,
            preview.source_message_id,
            &preview.user_content,
            &resolved,
            preview.web_search_enabled,
        )
        .await
    {
        Ok(p) => p,
        Err(e) => return CanonicalError::from(e).into_response(),
    };

    // The preview already authorized this retry/edit; reuse its scope.
    let chat_scope = preview.chat_scope;
    let mutation = match new_content {
        Some(content) => {
            svc.turns
                .edit_in_scope(&ctx, chat_scope, chat_id, request_id, content)
                .await
        }
        None => {
            svc.turns
                .retry_in_scope(&ctx, chat_scope, chat_id, request_id)
                .await
        }
    };
    let mutation = match mutation {
        Ok(m) => m,
        Err(e) => return CanonicalError::from(e).into_response(),
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
            mutation.web_search_enabled,
            mutation.snapshot_boundary,
            preflight,
            cancel.clone(),
            tx,
        )
        .await
    {
        Ok(handle) => handle,
        Err(e) => return CanonicalError::from(e).into_response(),
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// The turn status endpoint reports `done` / `error` for the stored
    /// `completed` / `failed` states.
    #[test]
    fn turn_state_maps_to_status_state() {
        for (state, expected) in [
            (TurnState::Running, "running"),
            (TurnState::Completed, "done"),
            (TurnState::Failed, "error"),
            (TurnState::Cancelled, "cancelled"),
        ] {
            let value = serde_json::to_value(map_turn_state(&state)).unwrap();
            assert_eq!(value, expected);
        }
    }
}
