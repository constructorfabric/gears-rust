//! Axum handlers for message-variant and branching endpoints.
//!
//! Routes (full registration in Phase 14):
//!
//! | Route                                                              | Method | Handler                  | Service method                  |
//! |--------------------------------------------------------------------|--------|--------------------------|---------------------------------|
//! | `/sessions/{s}/messages/{m}/recreate`                              | POST   | [`recreate_variant`]     | `VariantService::recreate_variant` |
//! | `/sessions/{s}/messages/{m}/branch`                                | POST   | [`branch_message`]       | `VariantService::branch_message`   |
//! | `/sessions/{s}/messages/{m}/variants`                              | GET    | [`list_variants`]        | `VariantService::list_variants`    |
//! | `/sessions/{s}/active-variant`                                     | PATCH  | [`set_active_variant`]   | `VariantService::set_active_variant` |
//! | `/sessions/{s}/messages/{m}/variants/active`                       | PUT    | [`set_active_variant_compat`] | (compat alias of [`set_active_variant`]) |
//! | `/sessions/{s}/type`                                               | PATCH  | [`switch_session_type`]  | `VariantService::switch_session_type` |
//! | `/sessions/{s}/session-type`                                       | PATCH  | [`switch_session_type_compat`] | (compat alias of [`switch_session_type`]) |
//!
//! The recreate handler reuses Phase 5's streaming pipeline through
//! [`VariantService::recreate_variant`] — no chunk-forwarding logic is
//! duplicated here.
//
// @cpt-cf-chat-engine-api-rest-variants-handler:p6
// @cpt-cf-chat-engine-adr-message-variants:p6
// @cpt-cf-chat-engine-adr-message-recreation:p6
// @cpt-cf-chat-engine-adr-branching-strategy:p6
// @cpt-cf-chat-engine-adr-session-switching:p6

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::response::{Json, Response};
use chat_engine_sdk::models::{CapabilityValue, MessagePart, MessagePartInput, VariantInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;
use tracing::field::Empty;
use uuid::Uuid;

use toolkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::service::variant_service::{VariantEntry, VariantListing, VariantService};

// ============================================================================
//  Request / response DTOs
// ============================================================================

/// `POST /sessions/{s}/messages/{m}/recreate`.
#[derive(Debug, Deserialize, Default)]
pub struct RecreateBody {
    /// Optional capability overrides for the recreate call.
    #[serde(default)]
    pub enabled_capabilities: Option<Vec<CapabilityValue>>,

    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `POST /sessions/{s}/messages/{m}/branch`.
#[derive(Debug, Deserialize)]
pub struct BranchBody {
    #[serde(default)]
    pub parts: Vec<MessagePartInput>,
    #[serde(default)]
    pub file_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub enabled_capabilities: Option<Vec<CapabilityValue>>,

    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `PATCH /sessions/{s}/active-variant`.
///
/// Identifies the target variant by `message_id` (canonical, per brief).
/// `variant_index` may optionally be supplied for client diagnostics but
/// the service authoritatively reads it from the stored row.
#[derive(Debug, Deserialize)]
pub struct ActiveVariantBody {
    pub message_id: Uuid,
    #[serde(default)]
    pub variant_index: Option<u32>,

    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `PUT /sessions/{s}/messages/{m}/variants/active` (compat). The
/// target variant is identified by `variant_index` in the body and the
/// `m` path parameter; we resolve it to a `message_id` by looking up
/// the sibling list.
#[derive(Debug, Deserialize)]
pub struct ActiveVariantCompatBody {
    pub variant_index: u32,

    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `PATCH /sessions/{s}/type` / `PATCH /sessions/{s}/session-type`.
#[derive(Debug, Deserialize)]
pub struct SwitchSessionTypeBody {
    pub session_type_id: Uuid,

    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `GET /sessions/{s}/messages/{m}/variants` response envelope.
#[derive(Debug, Serialize)]
pub struct ListVariantsResponse {
    pub variants: Vec<ListVariantsEntry>,
    pub current_index: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ListVariantsEntry {
    pub message_id: Uuid,
    pub variant_index: u32,
    pub total_variants: u32,
    pub is_active: bool,
    pub is_complete: bool,
    pub parts: Vec<MessagePart>,
    pub metadata: Option<JsonValue>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

impl From<VariantListing> for ListVariantsResponse {
    fn from(value: VariantListing) -> Self {
        Self {
            current_index: value.current_index,
            variants: value
                .variants
                .into_iter()
                .map(|VariantEntry { message, info }| ListVariantsEntry {
                    message_id: info.message_id,
                    variant_index: info.variant_index,
                    total_variants: info.total_variants,
                    is_active: info.is_active,
                    is_complete: message.is_complete,
                    parts: message.parts,
                    metadata: message.metadata,
                    created_at: message.created_at,
                })
                .collect(),
        }
    }
}

// ============================================================================
//  Handlers
// ============================================================================

/// `POST /sessions/{session_id}/messages/{message_id}/recreate`.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(
        request_id = Empty,
        session_id = %session_id,
        message_id = %message_id,
    ),
)]
pub async fn recreate_variant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RecreateBody>,
) -> Result<Response> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;

    let cancel = CancellationToken::new();
    let stream = svc
        .recreate_variant(
            &identity,
            session_id,
            message_id,
            body.enabled_capabilities,
            cancel,
        )
        .await?;

    stream_to_sse_response(stream)
}

/// `POST /sessions/{session_id}/messages/{message_id}/branch`.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(
        session_id = %session_id,
        branch_point_message_id = %message_id,
    ),
)]
pub async fn branch_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<BranchBody>,
) -> Result<Response> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;

    let cancel = CancellationToken::new();
    let stream = svc
        .branch_message(
            &identity,
            session_id,
            message_id,
            body.parts,
            body.file_ids,
            body.enabled_capabilities,
            cancel,
        )
        .await?;

    stream_to_sse_response(stream)
}

/// `GET /sessions/{session_id}/messages/{message_id}/variants`.
#[tracing::instrument(
    skip(svc, ctx),
    fields(session_id = %session_id, message_id = %message_id),
)]
pub async fn list_variants(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ListVariantsResponse>> {
    let identity = identity_from_ctx(&ctx)?;
    let listing = svc.list_variants(&identity, session_id, message_id).await?;
    Ok(Json(listing.into()))
}

/// `PATCH /sessions/{session_id}/active-variant` — canonical handler.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id),
)]
pub async fn set_active_variant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<ActiveVariantBody>,
) -> Result<Json<VariantInfo>> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;
    let entry = svc
        .set_active_variant(&identity, session_id, body.message_id)
        .await?;
    Ok(Json(entry.info))
}

/// `PUT /sessions/{session_id}/messages/{message_id}/variants/active` —
/// compat alias of [`set_active_variant`]. Resolves
/// `(message_id_in_path, variant_index_in_body)` → target sibling, then
/// delegates to the canonical handler.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id, message_id = %message_id),
)]
pub async fn set_active_variant_compat(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path((session_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ActiveVariantCompatBody>,
) -> Result<Json<VariantInfo>> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;

    // Look up siblings + locate the entry at the requested variant_index.
    let listing = svc.list_variants(&identity, session_id, message_id).await?;
    let target = listing
        .variants
        .into_iter()
        .find(|e| e.info.variant_index == body.variant_index)
        .ok_or_else(|| {
            ChatEngineError::not_found(
                "variant",
                format!("{}:variant_index={}", message_id, body.variant_index),
            )
        })?;
    let entry = svc
        .set_active_variant(&identity, session_id, target.message.message_id)
        .await?;
    Ok(Json(entry.info))
}

/// `PATCH /sessions/{session_id}/type` — canonical handler.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id),
)]
pub async fn switch_session_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<SwitchSessionTypeBody>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;
    let session = svc
        .switch_session_type(&identity, session_id, body.session_type_id)
        .await?;
    Ok(Json(session))
}

/// `PATCH /sessions/{session_id}/session-type` — compat alias of
/// [`switch_session_type`].
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id),
)]
pub async fn switch_session_type_compat(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<SwitchSessionTypeBody>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    switch_session_type(Extension(ctx), Extension(svc), Path(session_id), Json(body)).await
}

// ============================================================================
//  Shared NDJSON response builder
// ============================================================================

fn stream_to_sse_response(
    stream: crate::domain::service::message_service::SendMessageStream,
) -> Result<Response> {
    // Delegates to the shared SSE delta-stream builder (FR-024): the plugin
    // event stream is projected into `start`/`delta`/`complete`/`error` SSE
    // frames. Per true live-tail, a client disconnect no longer cancels the
    // driver — it runs to completion and the client may resume via
    // `Last-Event-ID`.
    Ok(crate::api::rest::sse_delta_stream_response(stream))
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
#[path = "variants_tests.rs"]
mod variants_tests;
