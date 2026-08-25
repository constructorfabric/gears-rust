//! Axum handlers for the message-search surface (Phase 11).
//!
//! Routes (mounted on the live router in Phase 14):
//!
//! | Route                                  | Method | Auth | Handler           |
//! |----------------------------------------|--------|------|-------------------|
//! | `/sessions/{id}/search`                | GET    | JWT  | [`get_search_session`] |
//! | `/search`                              | GET    | JWT  | [`get_search`]    |
//!
//! Error mapping (consumed by Phase 14's RFC-9457 wrapper):
//! - `BadRequest` → 400 (empty `q`, oversized `q`, malformed `cursor`)
//! - `Forbidden` → 403 (missing identity claims)
//! - `NotFound` → 404 (session not owned by caller — anti-enumeration)
//! - `Internal` → 500 (backend / DB failure)
//!
//! Both handlers emit the structured `chat_engine::search` log target with
//! `scope`, `query_length`, `result_count`, `duration_ms`, and (for the
//! session-scoped handler) `session_id`. They MUST NOT log the raw query
//! string — only its length is recorded.
//
// @cpt-cf-chat-engine-api-rest-search-handler:p11
// @cpt-cf-chat-engine-adr-search-strategy:p11

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query};
use tracing::field::Empty;
use uuid::Uuid;

use toolkit_security::SecurityContext;

use crate::domain::error::Result;
use crate::domain::search::{SearchPage, SearchQuery};
use crate::domain::service::search_service::SearchService;

/// `GET /sessions/{id}/search` — session-scoped full-text search.
///
/// Identity is sourced from the bearer JWT via [`SecurityContext`]; the
/// session ownership check happens inside [`SearchService::search_in_session`]
/// before the search runs (per Phase 11 §Scoping and Security).
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        session_id = %session_id,
        scope = "session",
        query_length = Empty,
        result_count = Empty,
    ),
)]
pub async fn get_search_session(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SearchService>>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchPage>> {
    let q_len = query.q.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    tracing::Span::current().record("query_length", q_len);

    let page = svc.search_in_session(&ctx, session_id, &query).await?;
    tracing::Span::current().record("result_count", page.items.len());
    Ok(Json(page))
}

/// `GET /search` — cross-session full-text search.
///
/// Returns results across every session owned by the caller's
/// `(tenant_id, user_id)` pair. Hard-deleted sessions and hidden messages
/// are excluded by the backend layer.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        scope = "cross_session",
        query_length = Empty,
        result_count = Empty,
    ),
)]
pub async fn get_search(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<SearchService>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchPage>> {
    let q_len = query.q.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    tracing::Span::current().record("query_length", q_len);

    let page = svc.search_across_sessions(&ctx, &query).await?;
    tracing::Span::current().record("result_count", page.items.len());
    Ok(Json(page))
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
