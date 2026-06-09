//! Axum handlers for `/session-types`.
//!
//! Routes (full registration in Phase 14):
//!
//! | Route                | Method | Handler                        | Service method            | Status codes |
//! |----------------------|--------|--------------------------------|---------------------------|--------------|
//! | `/session-types`     | POST   | [`register_session_type`]      | `register_session_type`   | 201, 400, 401, 404 |
//! | `/session-types`     | GET    | [`list_session_types`]         | `list_session_types`      | 200, 401 |
//! | `/session-types/{id}` | GET   | [`get_session_type`]           | `get_session_type`        | 200, 401, 404 |
//!
//! Body fields named `tenant_id` / `user_id` are rejected: developer-scope
//! session-type registration is a server-side operation (per PRD §7).
//
// @cpt-cf-chat-engine-api-rest-session-types-handler:p4

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tracing::field::Empty;
use uuid::Uuid;

use modkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::service::session_service::{RegisterSessionTypeRequest, SessionService};

/// Body for `POST /session-types`.
#[derive(Debug, Deserialize)]
pub struct RegisterSessionTypeBody {
    pub name: String,
    pub plugin_instance_id: Option<String>,
    pub plugin_config: Option<JsonValue>,
    // ---- anti-spoof fields ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

#[tracing::instrument(
    skip(svc, ctx, body),
    fields(request_id = Empty, session_type_id = Empty),
)]
pub async fn register_session_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Json(body): Json<RegisterSessionTypeBody>,
) -> Result<impl IntoResponse> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    if body.name.trim().is_empty() {
        return Err(ChatEngineError::bad_request(
            "`name` is required for session-type registration",
        ));
    }
    let identity = identity_from_ctx(&ctx)?;
    let session_type = svc
        .register_session_type(
            &identity,
            RegisterSessionTypeRequest {
                name: body.name,
                plugin_instance_id: body.plugin_instance_id,
                plugin_config: body.plugin_config,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(session_type)))
}

#[tracing::instrument(skip(svc, ctx), fields(request_id = Empty))]
pub async fn list_session_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
) -> Result<Json<Vec<chat_engine_sdk::models::SessionType>>> {
    let identity = identity_from_ctx(&ctx)?;
    let types = svc.list_session_types(&identity).await?;
    Ok(Json(types))
}

#[tracing::instrument(skip(svc, ctx), fields(session_type_id = %session_type_id))]
pub async fn get_session_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SessionService>>,
    Path(session_type_id): Path<Uuid>,
) -> Result<Json<chat_engine_sdk::models::SessionType>> {
    let identity = identity_from_ctx(&ctx)?;
    let session_type = svc.get_session_type(&identity, session_type_id).await?;
    Ok(Json(session_type))
}
