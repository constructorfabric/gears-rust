//! Axum handlers for `/sessions`.
//!
//! Routes (full registration in Phase 14):
//!
//! | Route                                | Method | Handler              | Service method             | Status codes |
//! |--------------------------------------|--------|----------------------|----------------------------|--------------|
//! | `/sessions`                          | POST   | [`create_session`]   | `create_session`           | 201, 400, 401, 404, 502 |
//! | `/sessions`                          | GET    | [`list_sessions`]    | `list_sessions`            | 200, 401 |
//! | `/sessions/{id}`                     | GET    | [`get_session`]      | `get_session`              | 200, 401, 404 |
//! | `/sessions/{id}`                     | PATCH  | [`patch_session`]    | `update_metadata` + `update_capabilities` | 200, 400, 401, 404, 409, 502 |
//! | `/sessions/{id}`                     | DELETE | [`delete_session`]   | `delete_session`           | 200, 204, 401, 404, 422 |
//! | `/sessions/{id}/archive`             | POST   | [`archive_session`]  | `archive_session`          | 200, 401, 404, 422 |
//! | `/sessions/{id}/restore`             | POST   | [`restore_session`]  | `restore_session`          | 200, 401, 404, 409, 422 |
//!
//! `tenant_id` / `user_id` are extracted from the bearer JWT
//! (`SecurityContext`) and never accepted from the request body — handlers
//! reject any wire payload field named `tenant_id` or `user_id` via the
//! `BodyTenantUserGuard` rejection.
//
// @cpt-cf-chat-engine-api-rest-sessions-handler:p4
// @cpt-cf-chat-engine-adr-session-deletion-strategy:p4

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::field::Empty;
use uuid::Uuid;

use modkit_security::SecurityContext;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::service::session_service::{
    CreateSessionRequest, Identity, PaginatedSessions, SessionDeleteOutcome, SessionService,
};

/// Body for `POST /sessions`.
#[derive(Debug, Deserialize)]
pub struct CreateSessionBody {
    pub session_type_id: Option<Uuid>,
    pub metadata: Option<JsonValue>,
    // ---- anti-spoof fields ----
    /// Attempted client-supplied tenant_id — always rejected.
    pub tenant_id: Option<JsonValue>,
    /// Attempted client-supplied user_id — always rejected.
    pub user_id: Option<JsonValue>,
}

/// Body for `PATCH /sessions/{id}`. At least one of `metadata` /
/// `enabled_capabilities` must be supplied; supplying both applies both in
/// the order metadata → capabilities.
#[derive(Debug, Deserialize)]
pub struct PatchSessionBody {
    pub metadata: Option<JsonValue>,
    pub enabled_capabilities: Option<Vec<chat_engine_sdk::models::CapabilityValue>>,
    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// Query parameters for `GET /sessions`.
#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Query parameters for `DELETE /sessions/{id}`.
#[derive(Debug, Deserialize, Default)]
pub struct DeleteSessionQuery {
    /// `?hard=true` → hard delete (physical row removal + cascade).
    /// Default (`?hard=false` or absent) → soft delete.
    #[serde(default)]
    pub hard: Option<bool>,
}

/// Wire envelope for `GET /sessions`. Phase 14 may rewrap into the canonical
/// `Page<T>` representation; the field shape here is the contract for
/// downstream phases.
#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    pub items: Vec<chat_engine_sdk::models::Session>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[tracing::instrument(
    skip(svc, ctx, body),
    fields(request_id = Empty, session_id = Empty),
)]
pub async fn create_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Json(body): Json<CreateSessionBody>,
) -> Result<impl IntoResponse> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;

    let session = svc
        .create_session(
            &identity,
            CreateSessionRequest {
                session_type_id: body.session_type_id,
                metadata: body.metadata,
            },
        )
        .await?;

    Ok((StatusCode::CREATED, Json(session)))
}

#[tracing::instrument(skip(svc, ctx), fields(request_id = Empty))]
pub async fn list_sessions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<ListSessionsResponse>> {
    let identity = identity_from_ctx(&ctx)?;
    let PaginatedSessions {
        items,
        next_cursor,
        has_more,
    } = svc.list_sessions(&identity, query.cursor, query.limit).await?;
    Ok(Json(ListSessionsResponse {
        items,
        next_cursor,
        has_more,
    }))
}

#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn get_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    let identity = identity_from_ctx(&ctx)?;
    let session = svc.get_session(&identity, session_id).await?;
    Ok(Json(session))
}

#[tracing::instrument(skip(svc, ctx, body), fields(session_id = %session_id))]
pub async fn patch_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<PatchSessionBody>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    if body.metadata.is_none() && body.enabled_capabilities.is_none() {
        return Err(ChatEngineError::bad_request(
            "request must supply at least one of `metadata` or `enabled_capabilities`",
        ));
    }
    let identity = identity_from_ctx(&ctx)?;

    let mut latest: Option<chat_engine_sdk::models::Session> = None;
    if let Some(metadata) = body.metadata {
        latest = Some(svc.update_metadata(&identity, session_id, metadata).await?);
    }
    if let Some(caps) = body.enabled_capabilities {
        latest = Some(svc.update_capabilities(&identity, session_id, caps).await?);
    }

    Ok(Json(latest.expect("at least one branch ran")))
}

#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn delete_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<DeleteSessionQuery>,
) -> Result<axum::response::Response> {
    let identity = identity_from_ctx(&ctx)?;
    let hard = query.hard.unwrap_or(false);
    let outcome = svc.delete_session(&identity, session_id, hard).await?;
    match outcome {
        SessionDeleteOutcome::Soft { session } => Ok((StatusCode::OK, Json(session)).into_response()),
        SessionDeleteOutcome::Hard => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn archive_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    let identity = identity_from_ctx(&ctx)?;
    let session = svc.archive_session(&identity, session_id).await?;
    Ok(Json(session))
}

#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn restore_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<chat_engine_sdk::models::Session>> {
    let identity = identity_from_ctx(&ctx)?;
    let session = svc.restore_session(&identity, session_id).await?;
    Ok(Json(session))
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

/// Build a service [`Identity`] from the JWT-derived [`SecurityContext`].
/// Tenant + user are extracted from the token only — never from the request
/// body. Anonymous / unauthenticated contexts are rejected with a
/// `Forbidden` error (mapped to HTTP 403; Phase 14 will refine to 401 for
/// missing-token vs 403 for missing-claims).
pub(crate) fn identity_from_ctx(ctx: &SecurityContext) -> Result<Identity> {
    let tenant = ctx.subject_tenant_id();
    let user = ctx.subject_id();
    if tenant.is_nil() || user.is_nil() {
        return Err(ChatEngineError::forbidden(
            "authenticated identity required (tenant_id and user_id must be present in the bearer token)",
        ));
    }
    Identity::new(tenant.to_string(), user.to_string(), None)
}

/// Reject any client attempt to populate `tenant_id` or `user_id` in the
/// request body — those values are server-side-only (per PRD §7, anti-
/// enumeration + anti-spoof).
pub(crate) fn reject_body_identity(
    tenant_id: &Option<JsonValue>,
    user_id: &Option<JsonValue>,
) -> Result<()> {
    if tenant_id.is_some() || user_id.is_some() {
        return Err(ChatEngineError::bad_request(
            "tenant_id / user_id are derived from the auth token and must not appear in the request body",
        ));
    }
    Ok(())
}
