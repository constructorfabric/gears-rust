//! Axum handlers for the Session Intelligence surface (Phase 8).
//!
//! Routes (full router registration in Phase 14):
//!
//! | Route                                          | Method | Handler                          | Service method                         | Status codes |
//! |------------------------------------------------|--------|----------------------------------|----------------------------------------|--------------|
//! | `/sessions/{id}/summarize`                     | POST   | [`summarize_session`]            | `summarize_session`                    | 200, 401, 403, 404, 409, 422, 502 |
//! | `/sessions/{id}/retention-policy`              | GET    | [`get_retention_policy`]         | `get_effective_retention_policy`       | 200, 401, 403, 404 |
//! | `/sessions/{id}/retention-policy`              | PATCH  | [`patch_retention_policy`]       | `update_session_retention_policy`      | 200, 400, 401, 403, 404, 409 |
//!
//! `tenant_id` / `user_id` are extracted from the bearer JWT and never
//! accepted from the request body (per the standard handler convention
//! used across Phases 4–7).
//
// @cpt-cf-chat-engine-api-rest-intelligence-handler:p8
// @cpt-cf-chat-engine-flow-session-intelligence-generate-summary:p8
// @cpt-cf-chat-engine-flow-session-intelligence-get-retention:p8
// @cpt-cf-chat-engine-flow-session-intelligence-update-retention:p8

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path};
use axum::response::Response;
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;
use tracing::field::Empty;
use uuid::Uuid;

use toolkit_security::SecurityContext;

use crate::api::rest::handlers::sessions::{identity_from_ctx, reject_body_identity};
#[cfg(test)]
use crate::domain::error::ChatEngineError;
use crate::domain::error::Result;
use crate::domain::retention::RetentionPolicy;
use crate::domain::service::intelligence_service::IntelligenceService;

/// Wire body for `PATCH /sessions/{id}/retention-policy`. The
/// `RetentionPolicy` enum (re-exported from the SDK) is internally
/// tagged on `"type"`; serde rejects unknown discriminators with a parse
/// error which the API layer maps to `400 Bad Request`.
#[derive(Debug, serde::Deserialize)]
pub struct PatchRetentionPolicyBody {
    /// Internally-tagged `RetentionPolicy` payload. Required.
    #[serde(flatten)]
    pub policy: RetentionPolicy,
    // ---- anti-spoof fields (PRD §7; rejected if present) ----
    pub tenant_id: Option<JsonValue>,
    pub user_id: Option<JsonValue>,
}

/// `POST /sessions/{id}/summarize` — invokes the backend plugin's
/// `on_session_summary` hook and streams the result as NDJSON.
///
/// Response:
/// - `200 OK` with `content-type: application/x-ndjson` and a chunked
///   body of `Start → Chunk* → (Complete | Error)\n`-delimited JSON
///   when the plugin supports summarization and the session is active.
/// - Pre-stream failures surface as `Err(ChatEngineError)` mapped to
///   HTTP status by the scaffold [`IntoResponse`] impl: 401/403 for
///   identity, 404 for missing session, 409 for non-active lifecycle,
///   422 for unbound plugin / unsupported summarization (mapped from
///   `BackendUnavailable` without `retry_after`), 502 for plugin
///   errors.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        request_id = Empty,
        session_id = %session_id,
    ),
)]
pub async fn summarize_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<IntelligenceService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Response> {
    let identity = identity_from_ctx(&ctx)?;

    // Cancellation token threaded into the summary driver. The summary stream
    // is not part of the resumable message buffer, but it shares the SSE delta
    // builder; a client disconnect no longer force-cancels here, so an
    // on-demand summary still completes and persists.
    let cancel = CancellationToken::new();

    let event_stream = svc.summarize_session(&identity, session_id, cancel).await?;

    // Project the summary stream into the shared SSE delta protocol (FR-024).
    Ok(crate::api::rest::sse_delta_stream_response(event_stream))
}

/// `GET /sessions/{id}/retention-policy` — returns the effective
/// retention policy (per-session override, else session-type default,
/// else [`RetentionPolicy::None`]).
#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn get_retention_policy(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<IntelligenceService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<RetentionPolicy>> {
    let identity = identity_from_ctx(&ctx)?;
    let policy = svc
        .get_effective_retention_policy(&identity, session_id)
        .await?;
    Ok(Json(policy))
}

/// `PATCH /sessions/{id}/retention-policy` — updates the per-session
/// retention policy. Validates the variant + numeric bounds; rejects
/// invalid payloads with `400 Bad Request`.
#[tracing::instrument(skip(svc, ctx, body), fields(session_id = %session_id))]
pub async fn patch_retention_policy(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<IntelligenceService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<PatchRetentionPolicyBody>,
) -> Result<Json<RetentionPolicy>> {
    reject_body_identity(&body.tenant_id, &body.user_id)?;
    let identity = identity_from_ctx(&ctx)?;
    let updated = svc
        .update_session_retention_policy(&identity, session_id, body.policy)
        .await?;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_body_deserializes_none_policy() {
        let body: PatchRetentionPolicyBody =
            serde_json::from_value(json!({"type": "none"})).unwrap();
        assert!(matches!(body.policy, RetentionPolicy::None));
    }

    #[test]
    fn patch_body_deserializes_age_based() {
        let body: PatchRetentionPolicyBody =
            serde_json::from_value(json!({"type": "age_based", "max_age_days": 7})).unwrap();
        assert!(matches!(
            body.policy,
            RetentionPolicy::AgeBased { max_age_days: 7 }
        ));
    }

    #[test]
    fn patch_body_deserializes_count_based() {
        let body: PatchRetentionPolicyBody =
            serde_json::from_value(json!({"type": "count_based", "max_message_count": 100}))
                .unwrap();
        assert!(matches!(
            body.policy,
            RetentionPolicy::CountBased {
                max_message_count: 100
            }
        ));
    }

    #[test]
    fn patch_body_rejects_unknown_type() {
        let res: std::result::Result<PatchRetentionPolicyBody, _> =
            serde_json::from_value(json!({"type": "no_such"}));
        assert!(res.is_err(), "unknown discriminator must be rejected");
    }

    #[test]
    fn patch_body_rejects_anti_spoof_fields() {
        // The body deserializes fine; the handler-level guard then
        // rejects the request with BadRequest. Mirror the Phase 4 guard
        // shape so a regression of the deserializer doesn't accidentally
        // gain tenant_id/user_id setters.
        let body: PatchRetentionPolicyBody = serde_json::from_value(json!({
            "type": "none",
            "tenant_id": "spoof",
            "user_id": "spoof",
        }))
        .unwrap();
        assert!(body.tenant_id.is_some());
        assert!(body.user_id.is_some());
        let err = reject_body_identity(&body.tenant_id, &body.user_id).unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }
}
