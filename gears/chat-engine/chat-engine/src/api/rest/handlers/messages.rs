//! Axum handler for `POST /messages/send` — the NDJSON streaming endpoint.
//!
//! The handler is intentionally thin: it transforms the wire DTO into a
//! domain [`SendMessageRequest`], spawns the connection-close → cancel
//! bridge, calls [`MessageService::send_message`], and wraps the returned
//! [`StreamingEvent`] stream into an `application/x-ndjson` body.
//!
//! Pre-stream errors (validation, plugin missing, plugin
//! `Err(PluginError)`) bubble out as `Err(ChatEngineError)` — Phase 4's
//! scaffold `IntoResponse` impl maps them to a JSON `{"error": "…"}` body
//! with the correct HTTP status. Mid-stream errors stay on the wire as
//! `StreamingErrorEvent` (the HTTP response has already started; we MUST
//! NOT switch to a 5xx body at that point — see ADR-0006).
//
// @cpt-cf-chat-engine-api-rest-messages-handler:p5
// @cpt-cf-chat-engine-adr-streaming-architecture:p5
// @cpt-cf-chat-engine-adr-streaming-cancellation:p5

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use tracing::field::Empty;
use uuid::Uuid;

use chat_engine_sdk::models::{CapabilityValue, MessagePartInput};
use toolkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::service::message_service::{DeleteOutcome, MessageService, SendMessageRequest};

/// Wire body for `POST /messages/send`. Mirrors the JSON shape documented
/// in ADR-0006 §Streaming Operations.
#[derive(Debug, Deserialize)]
pub struct SendMessageBody {
    /// Target session. Must already exist and be owned by the JWT subject.
    pub session_id: Uuid,
    /// Ordered, typed body parts (FR-022). Each `{type, content}`; must be
    /// non-empty (validated in the service layer).
    #[serde(default)]
    pub parts: Vec<MessagePartInput>,
    /// Optional external file UUIDs forwarded opaquely to the plugin.
    #[serde(default)]
    pub file_ids: Option<Vec<Uuid>>,
    /// Optional parent in the message tree (must exist in `session_id`).
    #[serde(default)]
    pub parent_message_id: Option<Uuid>,
    /// Optional per-call capability values; must be a subset of the
    /// session's `enabled_capabilities`.
    #[serde(default)]
    pub capabilities: Option<Vec<CapabilityValue>>,

    // ---- anti-spoof fields (PRD §7; rejected if present) ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `POST /messages/send` — accepts a user message and streams the
/// assistant response back as NDJSON.
///
/// Response:
/// - On success: `200 OK` with
///   `content-type: application/x-ndjson` and a chunked body of
///   `Start → Chunk* → (Complete | Error)\n`-delimited JSON.
/// - On pre-stream failure: a JSON error body via Phase 4's scaffold
///   [`IntoResponse for ChatEngineError`]. Phase 14 replaces this with
///   full RFC-9457.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(
        request_id = Empty,
        session_id = Empty,
    ),
)]
pub async fn send_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Json(body): Json<SendMessageBody>,
) -> Result<Response> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;

    tracing::Span::current().record("session_id", tracing::field::display(body.session_id));

    // Cancellation token threaded into the driver. Under true live-tail it is
    // NOT cancelled by connection close — generation runs to completion so a
    // reconnect can resume. It remains the hook a future explicit "stop
    // generation" endpoint would cancel to abort the driver.
    let cancel = CancellationToken::new();

    let req = SendMessageRequest {
        session_id: body.session_id,
        parts: body.parts,
        file_ids: body.file_ids.unwrap_or_default(),
        parent_message_id: body.parent_message_id,
        capabilities: body.capabilities,
    };

    let event_stream = svc.send_message(req, identity, cancel).await?;

    // Project the plugin event stream into the client-facing SSE delta
    // protocol (FR-024). Per true live-tail, a client disconnect no longer
    // cancels the driver — it runs to completion and the client may resume the
    // stream via `Last-Event-ID`.
    Ok(crate::api::rest::sse_delta_stream_response(event_stream))
}

/// Wire response for `DELETE /sessions/{session_id}/messages/{message_id}`.
/// Field order mirrors the spec's `{message_id, deleted: true,
/// deleted_count, deleted_at}` envelope. `deleted_at` is serialised as
/// RFC-3339 UTC.
#[derive(Debug, Serialize)]
pub struct DeleteMessageResponse {
    pub message_id: Uuid,
    pub deleted: bool,
    pub deleted_count: u64,
    pub deleted_at: String,
}

impl DeleteMessageResponse {
    fn from_outcome(outcome: DeleteOutcome) -> Result<Self> {
        // Format the commit timestamp as RFC-3339 UTC. The `time` crate
        // emits a `Z` suffix because the input is already UTC.
        let deleted_at = outcome.deleted_at.format(&Rfc3339).map_err(|err| {
            ChatEngineError::internal(format!("failed to format deleted_at: {err}"))
        })?;
        Ok(Self {
            message_id: outcome.message_id,
            deleted: true,
            deleted_count: outcome.deleted_count,
            deleted_at,
        })
    }
}

/// `DELETE /sessions/{session_id}/messages/{message_id}` — atomic
/// cascade deletion of a message subtree.
///
/// Identity (`tenant_id`, `user_id`) is read EXCLUSIVELY from the JWT-
/// derived [`SecurityContext`]; the handler never inspects the path or
/// body for those values. Error status mapping follows the Phase 12
/// rules:
///
/// | Service error            | HTTP |
/// |--------------------------|------|
/// | `Forbidden` (tenant)     | 403  |
/// | `NotFound` (session/msg) | 404  |
/// | `Conflict` (root)        | 409  |
/// | `Forbidden` (no claims)  | 403  |
///
/// The Phase 4 scaffold [`IntoResponse for ChatEngineError`] performs the
/// final HTTP mapping; this handler only constructs the success body.
/// Phase 14 will refine missing-token (401) vs missing-claims (403).
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        session_id = Empty,
        message_id = %message_id,
        request_id = Empty,
    ),
)]
pub async fn delete_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<DeleteMessageResponse>> {
    let identity = identity_from_ctx(&ctx)?;
    // The spec keys this route on `message_id` only; resolve the owning
    // session (ownership-checked, cross-tenant → 404) before the cascade.
    let session_id = svc
        .resolve_owned_message(&identity, message_id)
        .await?
        .session_id;
    tracing::Span::current().record("session_id", tracing::field::display(session_id));
    let outcome = svc
        .delete_message_cascade(&identity, session_id, message_id)
        .await?;
    Ok(Json(DeleteMessageResponse::from_outcome(outcome)?))
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod messages_tests;
