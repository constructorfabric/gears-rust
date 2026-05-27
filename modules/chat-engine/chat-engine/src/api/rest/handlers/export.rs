//! Axum handlers for the session export & sharing surface (Phase 10).
//!
//! Routes (mounted on the live router in Phase 14):
//!
//! | Route                                  | Method | Auth | Handler             |
//! |----------------------------------------|--------|------|---------------------|
//! | `/sessions/{id}/export`                | POST   | JWT  | [`export_session`]  |
//! | `/sessions/{id}/share`                 | POST   | JWT  | [`create_share`]    |
//! | `/sessions/{id}/share`                 | DELETE | JWT  | [`revoke_share`]    |
//! | `/share/{token}`                       | GET    | NONE | [`access_shared`]   |
//!
//! `GET /share/{token}` is intentionally unauthenticated — Phase 14
//! routes it behind an anonymous-friendly middleware stack while the
//! other three sit behind the bearer-token guard.
//!
//! Error mapping (consumed by Phase 14's RFC-9457 wrapper):
//! - `NotFound` → 404
//! - `Forbidden` → 403
//! - `BadRequest` → 400
//! - `Conflict { reason: "share token expired" }` → **410 Gone**
//! - other `Conflict` → 409
//! - `BackendUnavailable` → 502 (storage failure)
//! - `Internal` → 500
//
// @cpt-cf-chat-engine-api-rest-export-handler:p10
// @cpt-cf-chat-engine-adr-session-sharing:p10

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::field::Empty;
use uuid::Uuid;

use modkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::export::{ExportFormat, ExportedSession, ShareTokenIssue, SharedSessionView};
use crate::domain::service::export_service::{ExportService, is_share_token_expired};

/// Query parameters for `POST /sessions/{id}/export`.
#[derive(Debug, Deserialize, Default)]
pub struct ExportSessionQuery {
    /// `json` (default) or `markdown` / `md`. Unknown values are
    /// rejected with HTTP 400.
    pub format: Option<String>,
    /// When `true`, the rendered export retains plugin-injected per-message
    /// metadata (model, finish_reason, request_id, …). Default `false`.
    #[serde(default)]
    pub include_plugin_metadata: Option<bool>,
}

/// Body for `POST /sessions/{id}/share`. The `expires_in_hours` field is
/// optional — omitting it issues a token with no auto-expiration.
#[derive(Debug, Deserialize, Default)]
pub struct CreateShareBody {
    pub expires_in_hours: Option<u32>,

    // ---- anti-spoof fields (rejected if present) ----
    #[serde(default)]
    pub tenant_id: Option<JsonValue>,
    #[serde(default)]
    pub user_id: Option<JsonValue>,
}

/// Wire shape for `POST /sessions/{id}/export`. Mirrors
/// [`ExportedSession`] verbatim; carried through a re-export so callers
/// don't need to depend on `domain::export` directly.
pub type ExportSessionResponse = ExportedSession;

/// Wire shape for `POST /sessions/{id}/share`. Mirrors
/// [`ShareTokenIssue`] verbatim.
pub type CreateShareResponse = ShareTokenIssue;

/// Wire shape for `GET /share/{token}`. Mirrors [`SharedSessionView`]
/// verbatim — intentionally has NO `user_id`, `tenant_id`, or `share_token`.
pub type SharedSessionResponse = SharedSessionView;

/// Empty body returned by a successful `DELETE /sessions/{id}/share`.
/// Serialized as `{}` so clients can parse a JSON envelope even on the
/// no-op revocation path.
#[derive(Debug, Serialize)]
pub struct RevokeShareResponse {}

/// `POST /sessions/{id}/export` — render the active message path as JSON
/// or Markdown and upload via [`crate::domain::export::ExportStorage`].
#[tracing::instrument(
    skip(svc, ctx),
    fields(session_id = %session_id, format = Empty, message_count = Empty),
)]
pub async fn export_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ExportService>>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<ExportSessionQuery>,
) -> Result<Json<ExportSessionResponse>> {
    let identity = identity_from_ctx(&ctx)?;
    let format = parse_format(query.format.as_deref())?;
    tracing::Span::current().record("format", tracing::field::display(format.as_str()));

    let exported = svc
        .export(
            &identity,
            session_id,
            format,
            query.include_plugin_metadata.unwrap_or(false),
        )
        .await?;
    tracing::Span::current().record("message_count", exported.message_count);
    Ok(Json(exported))
}

/// `POST /sessions/{id}/share` — generate and persist a CSPRNG share
/// token. Returns 201 with the raw token (the only sanctioned surface
/// for the token string).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id),
)]
pub async fn create_share(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ExportService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<CreateShareBody>,
) -> Result<(StatusCode, Json<CreateShareResponse>)> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;
    let issue = svc
        .create_share(&identity, session_id, body.expires_in_hours)
        .await?;
    Ok((StatusCode::CREATED, Json(issue)))
}

/// `DELETE /sessions/{id}/share` — clear `share_token` and remove
/// `share_expires_at` from metadata. Idempotent: clearing an
/// already-revoked token returns 200 OK.
#[tracing::instrument(
    skip(svc, ctx),
    fields(session_id = %session_id),
)]
pub async fn revoke_share(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ExportService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<RevokeShareResponse>> {
    let identity = identity_from_ctx(&ctx)?;
    svc.revoke_share(&identity, session_id).await?;
    Ok(Json(RevokeShareResponse {}))
}

/// `GET /share/{token}` — unauthenticated read-only view of a session.
/// The token path parameter MUST NOT be logged or surfaced in error
/// responses (per Phase 10 Rules §Share Token Security). The instrument
/// macro intentionally redacts the token field.
#[tracing::instrument(
    skip(svc, token),
    fields(token = "***redacted***"),
)]
pub async fn access_shared(
    Extension(svc): Extension<Arc<ExportService>>,
    Path(token): Path<String>,
) -> Response {
    match svc.access_shared(&token).await {
        Ok(view) => Json::<SharedSessionResponse>(view).into_response(),
        Err(err) => map_share_error(err),
    }
}

// --- helpers ---------------------------------------------------------------

fn parse_format(raw: Option<&str>) -> Result<ExportFormat> {
    use std::str::FromStr;
    match raw {
        None => Ok(ExportFormat::Json),
        Some(s) => ExportFormat::from_str(s),
    }
}

/// Map the share/expiry-specific `Conflict` carrier to HTTP 410 Gone.
/// Every other error variant is delegated to the Phase 4 scaffold
/// `IntoResponse for ChatEngineError`.
fn map_share_error(err: ChatEngineError) -> Response {
    if is_share_token_expired(&err) {
        return (
            StatusCode::GONE,
            Json(serde_json::json!({"error": "share_token_expired"})),
        )
            .into_response();
    }
    err.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_format_defaults_to_json() {
        assert_eq!(parse_format(None).unwrap(), ExportFormat::Json);
    }

    #[test]
    fn parse_format_accepts_known_values() {
        assert_eq!(parse_format(Some("json")).unwrap(), ExportFormat::Json);
        assert_eq!(
            parse_format(Some("markdown")).unwrap(),
            ExportFormat::Markdown
        );
        assert_eq!(parse_format(Some("md")).unwrap(), ExportFormat::Markdown);
    }

    #[test]
    fn parse_format_rejects_unknown_value() {
        let err = parse_format(Some("yaml")).unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[test]
    fn map_share_error_emits_410_for_expired_conflict() {
        let err = ChatEngineError::Conflict {
            reason: "share token expired".into(),
        };
        let response = map_share_error(err);
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[test]
    fn map_share_error_passes_through_unrelated_conflicts() {
        let err = ChatEngineError::Conflict {
            reason: "invalid lifecycle transition".into(),
        };
        let response = map_share_error(err);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn revoke_share_response_serializes_as_empty_object() {
        let body = serde_json::to_string(&RevokeShareResponse {}).unwrap();
        assert_eq!(body, "{}");
    }

    #[test]
    fn create_share_body_anti_spoof_fields_default_none() {
        let body: CreateShareBody = serde_json::from_str("{}").unwrap();
        assert!(body.expires_in_hours.is_none());
        assert!(body.tenant_id.is_none());
        assert!(body.user_id.is_none());
    }

    #[test]
    fn create_share_body_rejects_spoofed_identity() {
        let body: CreateShareBody = serde_json::from_str(r#"{"tenant_id": "x"}"#).unwrap();
        let result = reject_body_identity(&body.tenant_id, &body.user_id);
        assert!(matches!(result, Err(ChatEngineError::BadRequest { .. })));
    }

    #[test]
    fn export_session_query_defaults() {
        let q: ExportSessionQuery = serde_json::from_str("{}").unwrap();
        assert!(q.format.is_none());
        assert!(q.include_plugin_metadata.is_none());
    }
}
