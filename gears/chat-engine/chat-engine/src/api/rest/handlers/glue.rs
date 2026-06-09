//! Phase 14 glue handlers for routes whose underlying service surface
//! is not yet exposed by Phases 4-12.
//!
//! These handlers are deliberately minimal — they exist so the route
//! table declared in `api/rest/routes` is complete and the crate
//! compiles end-to-end. The full implementations land in Phase 15 (where
//! they can be wired through the production services with proper
//! `tenant_id` / `user_id` scoping) and the E2E suite in Phase 16
//! exercises them against the wire contract.
//
// @cpt-cf-chat-engine-api-rest-handlers-glue:p14

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::{Extension, Json};
use serde::Deserialize;
use uuid::Uuid;

use modkit_security::SecurityContext;

use axum::response::Response;
use futures::stream;

use crate::api::rest::dto::{
    MessageDto, MessageListDto, ReactionListDto, ReactionRequestDto, RecreateMessageRequestDto,
    SearchRequestDto, SearchResultsDto, StreamingEventDto, SummarizeAcceptedDto,
};
use crate::api::rest::handlers::sessions::identity_from_ctx;
use crate::api::rest::ndjson_response;
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::search::SearchQuery;
use crate::domain::service::{
    IntelligenceService, MessageService, ReactionService, SearchService, VariantService,
};

/// Query parameters for `GET /chat-engine/v1/sessions/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct ListMessagesQuery {
    #[serde(default)]
    pub parent_message_id: Option<Uuid>,
}

/// `GET /chat-engine/v1/sessions/{id}/messages` — Phase 14 stub.
///
/// Phase 15 wires this to a `MessageService::list_messages(session_id,
/// parent_message_id)` call once the corresponding repository method is
/// added. For Phase 14 the handler validates the auth context, returns
/// an empty envelope, and emits a `tracing` breadcrumb so the operator
/// can see the placeholder was hit.
#[tracing::instrument(
    skip(svc, ctx),
    fields(session_id = %session_id, parent_message_id),
)]
pub async fn list_messages(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<MessageListDto>> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = (svc, query);
    tracing::debug!(
        %session_id,
        "list_messages glue handler hit (Phase 15 will land the full implementation)",
    );
    Ok(Json(MessageListDto { items: vec![] }))
}

/// `GET /chat-engine/v1/messages/{id}` — Phase 14 stub.
///
/// Phase 15 wires this to a tenant-scoped lookup on the message repo.
#[tracing::instrument(
    skip(svc, ctx),
    fields(message_id = %message_id),
)]
pub async fn get_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<MessageDto>> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = svc;
    Err(ChatEngineError::not_found("message", message_id))
}

/// `POST /chat-engine/v1/sessions/{id}/search` — JSON-body variant that
/// delegates to [`SearchService::search_in_session`]. Bridges the
/// HTTP spec (POST with body) to the Phase 11 handler's GET-with-query
/// signature.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id, query_length),
)]
pub async fn search_in_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SearchService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<SearchRequestDto>,
) -> Result<Json<SearchResultsDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let query = SearchQuery {
        q: Some(body.query),
        top: body.limit,
        skip: body.offset,
        ..Default::default()
    };
    let page = svc.search_in_session(&identity, session_id, &query).await?;
    let results: Vec<serde_json::Value> = page
        .items
        .into_iter()
        .filter_map(|hit| serde_json::to_value(hit).ok())
        .collect();
    Ok(Json(SearchResultsDto { results }))
}

/// `POST /chat-engine/v1/sessions/search` — cross-session JSON-body
/// variant of the search endpoint.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(query_length),
)]
pub async fn search_across_sessions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SearchService>>,
    Json(body): Json<SearchRequestDto>,
) -> Result<Json<SearchResultsDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let query = SearchQuery {
        q: Some(body.query),
        top: body.limit,
        skip: body.offset,
        ..Default::default()
    };
    let page = svc.search_across_sessions(&identity, &query).await?;
    let results: Vec<serde_json::Value> = page
        .items
        .into_iter()
        .filter_map(|hit| serde_json::to_value(hit).ok())
        .collect();
    Ok(Json(SearchResultsDto { results }))
}

/// `POST /chat-engine/v1/sessions/{id}/messages` — Phase 14 stub for the
/// path-parameterised variant of [`super::messages::send_message`].
///
/// The Phase 5 handler accepts `session_id` from the request body; the API
/// spec sources it from the path. Phase 15 will collapse the two so this
/// stub goes away. For Phase 14 we close the stream cleanly so the wire
/// contract remains testable.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(session_id = %session_id),
)]
pub async fn send_message_in_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<crate::api::rest::dto::SendMessageRequestDto>,
) -> Result<Response> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = (svc, body, session_id);
    tracing::debug!(
        %session_id,
        "send_message_in_session glue handler hit (Phase 15 will land the full implementation)",
    );
    let empty = stream::empty::<std::result::Result<StreamingEventDto, ChatEngineError>>();
    Ok(ndjson_response(empty))
}

/// `POST /chat-engine/v1/messages/{id}/recreate` — Phase 14 stub.
///
/// The Phase 6 [`VariantService::recreate_variant`] handler is bound to a
/// `(session_id, message_id)` path; the API spec only carries `message_id`.
/// Phase 15 lands the service-side lookup. For Phase 14 this stub closes
/// the stream cleanly so the wire contract is testable end-to-end.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(message_id = %message_id),
)]
pub async fn recreate_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<VariantService>>,
    Path(message_id): Path<Uuid>,
    Json(body): Json<RecreateMessageRequestDto>,
) -> Result<Response> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = (svc, body, message_id);
    tracing::debug!(
        %message_id,
        "recreate_message glue handler hit (Phase 15 will land the full implementation)",
    );
    let empty = stream::empty::<std::result::Result<StreamingEventDto, ChatEngineError>>();
    Ok(ndjson_response(empty))
}

/// `POST /chat-engine/v1/messages/{id}/reactions` — Phase 14 stub.
///
/// The Phase 9 handler requires both `session_id` and `message_id` on the
/// path; the API spec only carries `message_id`. Phase 15 will land the
/// service-side lookup that resolves the parent `session_id` from
/// `message_id` so the handler can delegate to `ReactionService::set_reaction`
/// without a round-trip. For Phase 14 this stub validates the auth context
/// and returns an empty reaction list.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(message_id = %message_id),
)]
pub async fn set_reaction(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ReactionService>>,
    Path(message_id): Path<Uuid>,
    Json(body): Json<ReactionRequestDto>,
) -> Result<Json<ReactionListDto>> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = (svc, body);
    tracing::debug!(
        %message_id,
        "set_reaction glue handler hit (Phase 15 will land the session-id resolution)",
    );
    Ok(Json(ReactionListDto { reactions: vec![] }))
}

/// `POST /chat-engine/v1/sessions/{id}/summarize` — Phase 14 stub.
///
/// Phase 15 wires the synchronous accept path that schedules the
/// async summarization task and returns the polling URL.
#[tracing::instrument(
    skip(svc, ctx),
    fields(session_id = %session_id),
)]
pub async fn summarize_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<IntelligenceService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SummarizeAcceptedDto>> {
    let _identity = identity_from_ctx(&ctx)?;
    let _ = svc;
    Ok(Json(SummarizeAcceptedDto {
        session_id,
        status_url: format!("/chat-engine/v1/sessions/{session_id}/summarize/status"),
    }))
}
