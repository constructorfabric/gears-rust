//! Glue handlers bridging the spec's `message_id`-only / path-`session_id`
//! routes to the session-scoped domain services.
//!
//! The HTTP spec keys several routes on `message_id` alone
//! (`GET /messages/{id}`, recreate, reactions, variants) while the domain
//! services are session-scoped. These handlers resolve the owning
//! `session_id` via [`MessageService::resolve_owned_message`] (which also
//! performs the tenant/user ownership check, folding cross-tenant misses to
//! 404) and then delegate to the appropriate service.
//
// @cpt-cf-chat-engine-api-rest-handlers-glue:p14

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderName};
use axum::response::Response;
use axum::{Extension, Json};
use chat_engine_sdk::models::CapabilityValue;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    MessageDto, MessageListDto, ReactionDto, ReactionListDto, ReactionRequestDto,
    RecreateMessageRequestDto, SearchRequestDto, SearchResultsDto, SendMessageRequestDto,
    VariantInfoDto, VariantListDto, parts_into_sdk,
};
use crate::api::rest::handlers::sessions::identity_from_ctx;
use crate::api::rest::sse_delta_stream_response;
use crate::api::rest::stream_reader::sse_buffer_reader_response;
use crate::domain::error::{ChatEngineError, Result};
use crate::domain::reaction::ReactionType;
use crate::domain::search::SearchQuery;
use crate::domain::service::message_service::SendMessageRequest;
use crate::domain::service::{
    IntelligenceService, MessageService, ReactionService, SearchService, VariantService,
};
use crate::infra::db::repo::stream_event_repo::StreamEventBuffer;

/// Query parameters for `GET /chat-engine/v1/sessions/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct ListMessagesQuery {
    #[serde(default)]
    pub parent_message_id: Option<Uuid>,
}

/// Map optional wire capability values onto the SDK type.
fn capabilities_into_sdk(
    caps: Option<Vec<crate::api::rest::dto::CapabilityValueDto>>,
) -> Option<Vec<CapabilityValue>> {
    caps.map(|c| c.into_iter().map(CapabilityValue::from).collect())
}

/// `POST /chat-engine/v1/sessions/{id}/messages` — send a user message and
/// stream the assistant response as NDJSON.
#[tracing::instrument(skip(svc, ctx, body), fields(session_id = %session_id))]
pub async fn send_message_in_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<SendMessageRequestDto>,
) -> Result<Response> {
    let identity = identity_from_ctx(&ctx)?;
    let req = SendMessageRequest {
        session_id,
        parts: parts_into_sdk(body.parts),
        file_ids: body.file_ids.unwrap_or_default(),
        parent_message_id: body.parent_message_id,
        capabilities: capabilities_into_sdk(body.capabilities),
    };
    let cancel = CancellationToken::new();
    let stream = svc.send_message(req, identity, cancel).await?;
    Ok(sse_delta_stream_response(stream))
}

/// `GET /chat-engine/v1/sessions/{id}/messages` — list the active path.
#[tracing::instrument(skip(svc, ctx, query), fields(session_id = %session_id))]
pub async fn list_messages(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<MessageListDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let messages = svc
        .list_active_messages(&identity, session_id, query.parent_message_id)
        .await?;
    Ok(Json(MessageListDto {
        items: messages.into_iter().map(MessageDto::from).collect(),
    }))
}

/// `GET /chat-engine/v1/messages/{id}` — fetch a single owned message.
#[tracing::instrument(skip(svc, ctx), fields(message_id = %message_id))]
pub async fn get_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<MessageDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let message = svc.resolve_owned_message(&identity, message_id).await?;
    Ok(Json(MessageDto::from(message)))
}

/// `GET /chat-engine/v1/messages/{id}/stream` — (re)attach to an assistant
/// message's SSE delta stream (FR-024, true live-tail resume).
///
/// On reconnect the client sends `Last-Event-ID: <seq>` (the SSE `id:` of the
/// last event it applied); the server replays buffered events with
/// `seq > last` then live-tails the resume buffer until a terminal
/// (`complete`/`error`) event. Without the header the full buffered stream is
/// replayed from the start. The durable record remains `GET /messages/{id}`;
/// this only bridges the live-reconnect window
/// (`cpt-cf-chat-engine-design-stream-resume`).
///
/// Ownership is resolved first (cross-tenant/missing → 404) so no buffered
/// event is exposed without an access check.
#[tracing::instrument(skip(svc, buffer, ctx, headers), fields(message_id = %message_id))]
pub async fn resume_message_stream(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<MessageService>>,
    Extension(buffer): Extension<Arc<dyn StreamEventBuffer>>,
    Path(message_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response> {
    let identity = identity_from_ctx(&ctx)?;
    svc.resolve_owned_message(&identity, message_id).await?;

    let from_seq = parse_last_event_id(&headers);
    // Reader-only token: cancelling it stops polling on disconnect; the driver
    // keeps writing so a later reconnect resumes.
    let cancel = CancellationToken::new();
    Ok(sse_buffer_reader_response(
        buffer, message_id, from_seq, cancel,
    ))
}

/// Parse the SSE `Last-Event-ID` reconnect header into a `seq`. A missing or
/// malformed value resolves to `None` (replay from the start) rather than an
/// error — a client that lost its cursor still gets a coherent stream.
fn parse_last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(HeaderName::from_static("last-event-id"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// `POST /chat-engine/v1/messages/{id}/recreate` — regenerate an assistant
/// variant, streaming the new response as NDJSON.
#[tracing::instrument(skip(messages, variants, ctx, body), fields(message_id = %message_id))]
pub async fn recreate_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(messages): Extension<Arc<MessageService>>,
    Extension(variants): Extension<Arc<VariantService>>,
    Path(message_id): Path<Uuid>,
    Json(body): Json<RecreateMessageRequestDto>,
) -> Result<Response> {
    let identity = identity_from_ctx(&ctx)?;
    let session_id = messages
        .resolve_owned_message(&identity, message_id)
        .await?
        .session_id;
    let cancel = CancellationToken::new();
    let stream = variants
        .recreate_variant(
            &identity,
            session_id,
            message_id,
            capabilities_into_sdk(body.enabled_capabilities),
            cancel,
        )
        .await?;
    Ok(sse_delta_stream_response(stream))
}

/// `GET /chat-engine/v1/messages/{id}/variants` — list sibling variants.
#[tracing::instrument(skip(messages, variants, ctx), fields(message_id = %message_id))]
pub async fn list_variants(
    Extension(ctx): Extension<SecurityContext>,
    Extension(messages): Extension<Arc<MessageService>>,
    Extension(variants): Extension<Arc<VariantService>>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<VariantListDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let session_id = messages
        .resolve_owned_message(&identity, message_id)
        .await?
        .session_id;
    let listing = variants
        .list_variants(&identity, session_id, message_id)
        .await?;
    Ok(Json(VariantListDto {
        current_index: listing.current_index,
        variants: listing
            .variants
            .into_iter()
            .map(|e| VariantInfoDto::from(e.info))
            .collect(),
    }))
}

/// `POST /chat-engine/v1/messages/{id}/reactions` — set/update a reaction.
#[tracing::instrument(skip(messages, reactions, ctx, body), fields(message_id = %message_id))]
pub async fn set_reaction(
    Extension(ctx): Extension<SecurityContext>,
    Extension(messages): Extension<Arc<MessageService>>,
    Extension(reactions): Extension<Arc<ReactionService>>,
    Path(message_id): Path<Uuid>,
    Json(body): Json<ReactionRequestDto>,
) -> Result<Json<ReactionListDto>> {
    let identity = identity_from_ctx(&ctx)?;
    let reaction_type = ReactionType::from_str_value(&body.kind).ok_or_else(|| {
        ChatEngineError::bad_request(format!("unknown reaction kind: {}", body.kind))
    })?;
    let session_id = messages
        .resolve_owned_message(&identity, message_id)
        .await?
        .session_id;

    let (_response, mutation) = reactions
        .set_reaction(&identity, session_id, message_id, reaction_type)
        .await?;
    // Notify the backend plugin out-of-band; the detached task logs failures
    // and never blocks the HTTP response (see `spawn_plugin_notification`).
    drop(reactions.spawn_plugin_notification(mutation));

    let listing = reactions
        .list_reactions(&identity, session_id, message_id)
        .await?;
    Ok(Json(ReactionListDto {
        reactions: listing
            .reactions
            .into_iter()
            .map(|r| ReactionDto {
                kind: r.reaction_type.as_str().to_string(),
                value: None,
                user_id: r.user_id,
                created_at: r.created_at,
            })
            .collect(),
    }))
}

/// `POST /chat-engine/v1/sessions/{id}/summarize` — trigger an on-demand
/// session summary, streaming progress + the persisted summary as NDJSON.
#[tracing::instrument(skip(svc, ctx), fields(session_id = %session_id))]
pub async fn summarize_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<IntelligenceService>>,
    Path(session_id): Path<Uuid>,
) -> Result<Response> {
    let identity = identity_from_ctx(&ctx)?;
    let cancel = CancellationToken::new();
    let stream = svc.summarize_session(&identity, session_id, cancel).await?;
    Ok(sse_delta_stream_response(stream))
}

/// `POST /chat-engine/v1/sessions/{id}/search` — JSON-body variant that
/// delegates to [`SearchService::search_in_session`].
#[tracing::instrument(skip(svc, ctx, body), fields(session_id = %session_id, query_length))]
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

/// `POST /chat-engine/v1/sessions/search` — cross-session JSON-body variant.
#[tracing::instrument(skip(svc, ctx, body), fields(query_length))]
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
