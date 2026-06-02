//! Message search service (Phase 11).
//!
//! Owns the `GET /sessions/{id}/search` (within-session) and `GET /search`
//! (cross-session) flows. The service is dual-backend: the PostgreSQL path
//! uses `to_tsvector`/`plainto_tsquery`/`ts_rank_cd` with a GIN index (added
//! by the Phase 11 deferred migration), and the SQLite/dev-test path falls
//! back to `LOWER(content) LIKE LOWER(?)` with no ranking. Both backend
//! impls ship as concrete structs (`crate::infra::search::PgSearchBackend`,
//! `crate::infra::search::SqliteSearchBackend`) that compile
//! unconditionally — the `modkit-db` workspace dependency enables BOTH
//! the `pg` and `sqlite` cargo features, and Phase 15 owns all per-crate
//! feature wiring. Selection between the two backends happens at
//! module-wiring time (Phase 15) based on the materialised
//! `DatabaseConnection::get_database_backend()` discriminant. The service
//! itself stays backend-agnostic via the [`SearchBackend`] trait.
//!
//! Tenant + user scoping is enforced for every read by routing through the
//! existing `SessionRepo::find_by_id` (single-session search) or
//! `SessionRepo::list_paginated` filter (cross-session search). The
//! underlying message read is performed via [`SearchBackend::search`] —
//! the only Phase 11 surface that touches the `DatabaseConnection`. This
//! lets the unit tests swap an in-memory backend without touching SeaORM.
//!
//! ### Hidden messages
//!
//! Rows with `is_hidden_from_user = true` are filtered out by every
//! backend before pagination is applied so summary anchors (Phase 8) and
//! plugin-generated hidden context never leak into the response. The
//! context-window loader applies the same filter.
//!
//! ### Cursor semantics
//!
//! When a cursor is supplied the service drops the `$skip` parameter (the
//! two are mutually exclusive in keyset pagination). The cursor encodes the
//! last-seen `(rank, message_id)` pair so subsequent pages skip already-
//! returned rows even when intervening writes shift the global ordering.
//
// @cpt-cf-chat-engine-search-service:p11
// @cpt-cf-chat-engine-adr-search-strategy:p11

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use modkit_macros::domain_model;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::{Message, MessageRole};
use crate::domain::search::{
    Cursor, MAX_QUERY_LENGTH, MessageRef, SearchError, SearchPage, SearchQuery, SearchResult,
    SessionMeta, extract_searchable_text, make_snippet, sanitize_for_tsquery,
};
use crate::domain::service::session_service::Identity;
use crate::infra::db::repo::message_repo::MessageRepo;
use crate::infra::db::repo::session_repo::SessionRepo;

/// Scope label used by the `search_duration_seconds` metric / structured log.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Session,
    CrossSession,
}

impl SearchScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::CrossSession => "cross_session",
        }
    }
}

/// Parsed search input — sanitised and validated.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Original (length-checked) query string for ILIKE matching.
    pub raw: String,
    /// Sanitised payload safe for `plainto_tsquery` / `phraseto_tsquery`.
    pub tsquery: String,
}

/// Parse + validate a raw query string. Returns a [`SearchError`] for empty
/// or oversized input.
///
/// The PostgreSQL `tsquery` path consumes `parsed.tsquery`; the SQLite path
/// consumes `parsed.raw` (after [`escape_like_pattern`]).
pub fn parse_search_query(raw: &str) -> std::result::Result<ParsedQuery, SearchError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SearchError::QueryRequired);
    }
    if trimmed.chars().count() > MAX_QUERY_LENGTH {
        return Err(SearchError::QueryTooLong);
    }
    let tsquery = sanitize_for_tsquery(trimmed);
    if tsquery.is_empty() {
        // All characters stripped → effectively empty query.
        return Err(SearchError::QueryRequired);
    }
    Ok(ParsedQuery {
        raw: trimmed.to_string(),
        tsquery,
    })
}

/// Result row carried back from the search backend. The service enriches
/// each hit with context messages + the parent chain before returning to
/// the handler.
#[domain_model]
#[derive(Debug, Clone)]
pub struct BackendHit {
    pub message_id: Uuid,
    pub session_id: Uuid,
    pub parent_message_id: Option<Uuid>,
    pub role: MessageRole,
    pub content: serde_json::Value,
    pub created_at: time::OffsetDateTime,
    /// Relevance score. SQLite backend returns `0.0`.
    pub rank: f32,
}

/// Pagination + scoping passed to a [`SearchBackend`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct SearchScopeFilter {
    pub tenant_id: String,
    pub user_id: String,
    /// When `Some`, restricts the search to the given session. `None` →
    /// search across all sessions owned by `(tenant_id, user_id)`.
    pub session_id: Option<Uuid>,
}

/// Backend-agnostic search surface. Two concrete impls live in
/// `crate::infra::search::backend` (`PgSearchBackend` and
/// `SqliteSearchBackend`) — selection happens at module-wiring time
/// (Phase 15) based on the live `DatabaseBackend`.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Execute a paginated search. Backends MUST:
    /// - Exclude rows with `is_hidden_from_user = true`.
    /// - Exclude rows in hard-deleted sessions (cross-session path).
    /// - Apply the scope filter's `tenant_id` + `user_id` via the
    ///   `sessions` join (or `session_id` filter for the session-scoped
    ///   path).
    /// - Honour `cursor` (when set) by skipping rows ordered before/equal
    ///   to the cursor's `(rank, message_id)` keyset.
    /// - Return at most `limit` rows + a flag indicating whether more
    ///   rows are available (caller materialises `next_cursor`).
    async fn search(
        &self,
        scope: &SearchScopeFilter,
        query: &ParsedQuery,
        cursor: Option<&Cursor>,
        skip: u32,
        limit: u32,
    ) -> std::result::Result<(Vec<BackendHit>, u64), ChatEngineError>;
}

/// In-memory backend used by unit tests and the SQLite/ILIKE path's
/// fallback. The backend stores a flat list of `(scope_session_id, msg)`
/// pairs and applies the filter at query time.
#[domain_model]
#[derive(Debug, Default)]
pub struct InMemorySearchBackend {
    rows: Vec<(SearchScopeFilter, Message)>,
}

impl InMemorySearchBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a `(scope, message)` pair into the in-memory store.
    pub fn push(&mut self, scope: SearchScopeFilter, message: Message) {
        self.rows.push((scope, message));
    }
}

#[async_trait]
impl SearchBackend for InMemorySearchBackend {
    async fn search(
        &self,
        scope: &SearchScopeFilter,
        query: &ParsedQuery,
        cursor: Option<&Cursor>,
        skip: u32,
        limit: u32,
    ) -> std::result::Result<(Vec<BackendHit>, u64), ChatEngineError> {
        let needle = query.raw.to_lowercase();
        let mut matches: Vec<BackendHit> = self
            .rows
            .iter()
            .filter(|(s, _)| {
                s.tenant_id == scope.tenant_id
                    && s.user_id == scope.user_id
                    && match scope.session_id {
                        Some(sid) => s.session_id == Some(sid),
                        None => true,
                    }
            })
            .filter(|(_, m)| !m.is_hidden_from_user)
            .filter(|(_, m)| {
                let text = extract_searchable_text(&m.content);
                text.to_lowercase().contains(&needle)
            })
            .map(|(_, m)| BackendHit {
                message_id: m.message_id,
                session_id: m.session_id,
                parent_message_id: m.parent_message_id,
                role: m.role.clone(),
                content: m.content.clone(),
                created_at: m.created_at,
                rank: 0.0,
            })
            .collect();

        // Order by created_at DESC, message_id DESC (deterministic tiebreak).
        // This MUST be the same key the cursor encodes — see the
        // `apply_cursor_desc` filter below.
        matches.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });

        let total = matches.len() as u64;

        // Apply cursor: drop EVERY hit ordered at-or-before the cursor
        // under the sort key, not just the row whose id matches. The
        // previous `retain(|h| h.message_id != c.message_id)` removed
        // exactly one row, so page 2 returned page 1 (minus a row)
        // instead of advancing past it.
        let matches = apply_cursor_desc(matches, cursor);

        let skip = skip as usize;
        let limit = limit as usize;
        if skip >= matches.len() {
            return Ok((Vec::new(), total));
        }
        let end = (skip + limit).min(matches.len());
        Ok((matches[skip..end].to_vec(), total))
    }
}

/// Drop every hit ordered at-or-before `cursor` under the
/// `(created_at DESC, message_id DESC)` sort key — the sole keyset
/// pagination primitive used by [`InMemorySearchBackend`].
///
/// Cursor variants:
/// - `Some(created_at)` (current format) → strict `<` filter on the
///   `(created_at, message_id)` tuple. This is the canonical keyset skip.
/// - `None` (legacy cursor minted before the `:t:<unix_ns>` tail) → fall
///   back to position-based slicing: find the cursor row in `matches`
///   and keep only rows strictly after it. Misses if the row is no
///   longer in the candidate set, which is the unavoidable limitation
///   of a legacy cursor that did not carry the sort key.
fn apply_cursor_desc(matches: Vec<BackendHit>, cursor: Option<&Cursor>) -> Vec<BackendHit> {
    let Some(c) = cursor else {
        return matches;
    };
    if let Some(c_ts) = c.created_at {
        return matches
            .into_iter()
            .filter(|h| {
                // Under DESC ordering, "after the cursor" means a
                // smaller (created_at, message_id) tuple.
                h.created_at < c_ts || (h.created_at == c_ts && h.message_id < c.message_id)
            })
            .collect();
    }
    // Legacy cursor — best-effort position-based skip. matches is
    // already sorted DESC, so the cursor row (if present) appears once
    // and everything after it in the slice is the next page.
    match matches.iter().position(|h| h.message_id == c.message_id) {
        Some(idx) => matches.into_iter().skip(idx + 1).collect(),
        None => matches,
    }
}

/// Orchestrates the two search endpoints. Generic over the backend so
/// production wiring (Phase 15) plugs in the SeaORM-backed implementation
/// while unit tests use [`InMemorySearchBackend`].
#[domain_model]
#[derive(Clone)]
pub struct SearchService {
    sessions: Arc<dyn SessionRepo>,
    messages: Arc<dyn MessageRepo>,
    backend: Arc<dyn SearchBackend>,
}

impl SearchService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        messages: Arc<dyn MessageRepo>,
        backend: Arc<dyn SearchBackend>,
    ) -> Self {
        Self {
            sessions,
            messages,
            backend,
        }
    }

    /// Session-scoped search. Validates session ownership BEFORE running the
    /// search (per Phase 11 Rules §Scoping and Security).
    #[instrument(skip(self, identity, query), fields(session_id = %session_id))]
    pub async fn search_in_session(
        &self,
        identity: &Identity,
        session_id: Uuid,
        query: &SearchQuery,
    ) -> Result<SearchPage> {
        let started = Instant::now();
        let parsed = parse_search_query(query.q.as_deref().unwrap_or("")).map_err(
            ChatEngineError::from,
        )?;

        // Ownership validation: missing or cross-tenant session → 404.
        let owned = self
            .sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;

        let scope = SearchScopeFilter {
            tenant_id: identity.tenant_id.clone(),
            user_id: identity.user_id.clone(),
            session_id: Some(owned.session_id),
        };
        let page = self
            .run(&scope, &parsed, query, SearchScope::Session)
            .await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            target: "chat_engine::search",
            scope = SearchScope::Session.as_str(),
            session_id = %session_id,
            query_length = parsed.raw.chars().count(),
            result_count = page.items.len(),
            duration_ms,
            "search.completed"
        );
        Ok(page)
    }

    /// Cross-session search across every session owned by the caller.
    /// Hard-deleted sessions are excluded by the underlying backend
    /// implementation.
    #[instrument(skip(self, identity, query))]
    pub async fn search_across_sessions(
        &self,
        identity: &Identity,
        query: &SearchQuery,
    ) -> Result<SearchPage> {
        let started = Instant::now();
        let parsed = parse_search_query(query.q.as_deref().unwrap_or("")).map_err(
            ChatEngineError::from,
        )?;

        let scope = SearchScopeFilter {
            tenant_id: identity.tenant_id.clone(),
            user_id: identity.user_id.clone(),
            session_id: None,
        };
        // For cross-session results we need session titles → look them up
        // in batch after the backend returns the hits. Index by session id.
        let page = self
            .run(&scope, &parsed, query, SearchScope::CrossSession)
            .await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            target: "chat_engine::search",
            scope = SearchScope::CrossSession.as_str(),
            query_length = parsed.raw.chars().count(),
            result_count = page.items.len(),
            duration_ms,
            "search.completed"
        );
        Ok(page)
    }

    async fn run(
        &self,
        scope: &SearchScopeFilter,
        parsed: &ParsedQuery,
        query: &SearchQuery,
        kind: SearchScope,
    ) -> Result<SearchPage> {
        let limit = query.effective_top();
        let skip = if query.cursor.is_some() {
            0
        } else {
            query.effective_skip()
        };
        let context_radius = query.effective_context_radius();

        let cursor = match query.cursor.as_deref() {
            Some(raw) => Some(Cursor::decode(raw).map_err(ChatEngineError::from)?),
            None => None,
        };

        let (hits, total) = self
            .backend
            .search(scope, parsed, cursor.as_ref(), skip, limit + 1)
            .await?;

        // Detect whether another page exists.
        let mut hits = hits;
        let has_more = hits.len() as u32 > limit;
        if has_more {
            hits.truncate(limit as usize);
        }
        let next_cursor = if has_more {
            hits.last()
                // Cursor MUST carry the sort key (created_at) — without
                // it the backend cannot perform a real keyset skip and
                // page 2 would replay rows from page 1.
                .map(|h| Cursor::new(h.rank, h.message_id, h.created_at).encode())
        } else {
            None
        };

        // Enrich hits with context window + parent chain. For cross-session
        // results, also attach the session metadata.
        let mut items = Vec::with_capacity(hits.len());
        for hit in hits {
            let context_messages = self
                .load_context_window(hit.session_id, hit.created_at, context_radius)
                .await?;
            let parent_chain = self
                .load_parent_chain(hit.session_id, hit.parent_message_id)
                .await?;
            let snippet = make_snippet(&extract_searchable_text(&hit.content), &parsed.raw);
            let session_metadata = match kind {
                SearchScope::CrossSession => self.load_session_meta(scope, hit.session_id).await?,
                SearchScope::Session => None,
            };
            items.push(SearchResult {
                message_id: hit.message_id,
                session_id: hit.session_id,
                content_snippet: snippet,
                rank: hit.rank,
                context_messages,
                parent_chain,
                session_metadata,
            });
        }

        Ok(SearchPage {
            items,
            total_count: total,
            next_cursor,
            per_page: limit,
        })
    }

    /// Return the N messages immediately before and N after the matched
    /// message in chronological order. Hidden rows are dropped before
    /// trimming so the window does not silently shrink across hidden
    /// summaries.
    async fn load_context_window(
        &self,
        session_id: Uuid,
        anchor: time::OffsetDateTime,
        radius: u32,
    ) -> Result<Vec<MessageRef>> {
        if radius == 0 {
            return Ok(Vec::new());
        }
        let all = self.messages.list_active_path(session_id).await?;
        // Find anchor position.
        let mut before: Vec<&Message> = Vec::new();
        let mut after: Vec<&Message> = Vec::new();
        for m in &all {
            if m.is_hidden_from_user {
                continue;
            }
            if m.created_at < anchor {
                before.push(m);
            } else if m.created_at > anchor {
                after.push(m);
            }
        }
        // Keep last `radius` of `before` (closest to anchor) and first
        // `radius` of `after`.
        let before_skip = before.len().saturating_sub(radius as usize);
        let before_slice = &before[before_skip..];
        let after_take = (radius as usize).min(after.len());
        let after_slice = &after[..after_take];

        let mut out = Vec::with_capacity(before_slice.len() + after_slice.len());
        for m in before_slice.iter().chain(after_slice.iter()) {
            out.push(MessageRef {
                message_id: m.message_id,
                role: m.role.clone(),
                content: m.content.clone(),
                created_at: m.created_at,
            });
        }
        Ok(out)
    }

    /// Walk the parent chain from the matched message up to the session
    /// root in root-first order. Hidden ancestors are kept (the parent
    /// chain is structural; visibility is the caller's concern).
    async fn load_parent_chain(
        &self,
        session_id: Uuid,
        parent_message_id: Option<Uuid>,
    ) -> Result<Vec<MessageRef>> {
        let Some(mut cursor) = parent_message_id else {
            return Ok(Vec::new());
        };
        let all = self.messages.list_active_path(session_id).await?;
        let mut chain: Vec<MessageRef> = Vec::new();
        // Cap traversal depth to avoid pathological loops on corrupt data.
        let max_depth = 256;
        for _ in 0..max_depth {
            let Some(m) = all.iter().find(|m| m.message_id == cursor) else {
                break;
            };
            chain.push(MessageRef {
                message_id: m.message_id,
                role: m.role.clone(),
                content: m.content.clone(),
                created_at: m.created_at,
            });
            match m.parent_message_id {
                Some(p) => cursor = p,
                None => break,
            }
        }
        // Root-first order requested by the contract.
        chain.reverse();
        Ok(chain)
    }

    /// Build a [`SessionMeta`] for a cross-session hit. The session lookup
    /// is tenant + user scoped — if for any reason the session is not
    /// owned by the caller we silently omit the metadata (the row should
    /// have been filtered upstream; this is belt-and-braces).
    async fn load_session_meta(
        &self,
        scope: &SearchScopeFilter,
        session_id: Uuid,
    ) -> Result<Option<SessionMeta>> {
        let row = self
            .sessions
            .find_by_id(&scope.tenant_id, &scope.user_id, session_id)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let title = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("title"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let tags = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("tags"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Some(SessionMeta {
            session_id,
            title,
            tags,
        }))
    }
}

// Trait helper: convert from SearchError → ChatEngineError used inline above.
impl SearchScopeFilter {
    /// Convenience constructor used by tests.
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        user_id: impl Into<String>,
        session_id: Option<Uuid>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            session_id,
        }
    }
}

// ----------------------------------------------------------------------------
// Backend selection (runtime — Phase 15 wires the concrete impl)
// ----------------------------------------------------------------------------
//
// The two concrete SeaORM-backed implementations live in
// `crate::infra::search::backend` (see `PgSearchBackend` and
// `SqliteSearchBackend`) — they carry `DatabaseConnection` so they
// belong in the infra layer per the `#[domain_model]` boundary.
// Selection happens at module-wiring time (Phase 15) based on the live
// `DatabaseBackend`.

// ----------------------------------------------------------------------------
// Unit tests — exercise the service over the in-memory backend (SQLite-ish).
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::entity::session as session_entity;
    use crate::infra::db::repo::session_repo::{SessionPage, SessionRepo};
    use async_trait::async_trait;
    use serde_json::Value as JsonValue;
    use time::{Duration, OffsetDateTime};

    // ---------- Mock SessionRepo ----------

    #[derive(Default)]
    struct MockSessionRepo {
        sessions: Vec<session_entity::Model>,
    }

    impl MockSessionRepo {
        fn with(model: session_entity::Model) -> Self {
            Self {
                sessions: vec![model],
            }
        }
    }

    #[async_trait]
    impl SessionRepo for MockSessionRepo {
        async fn insert(
            &self,
            _model: session_entity::ActiveModel,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            unimplemented!()
        }
        async fn find_by_id(
            &self,
            tenant_id: &str,
            user_id: &str,
            session_id: Uuid,
        ) -> std::result::Result<Option<session_entity::Model>, ChatEngineError> {
            Ok(self
                .sessions
                .iter()
                .find(|s| {
                    s.session_id == session_id
                        && s.tenant_id == tenant_id
                        && s.user_id == user_id
                })
                .cloned())
        }
        async fn list_paginated(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _cursor: Option<&str>,
            _limit: u32,
        ) -> std::result::Result<SessionPage, ChatEngineError> {
            unimplemented!()
        }
        async fn update_metadata(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _metadata: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            unimplemented!()
        }
        async fn update_capabilities(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _enabled_capabilities: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            unimplemented!()
        }
        async fn update_lifecycle_state(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _state: crate::domain::session::LifecycleState,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            unimplemented!()
        }
        async fn soft_delete(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _retention_days: i64,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            unimplemented!()
        }
        async fn hard_delete(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
        ) -> std::result::Result<bool, ChatEngineError> {
            unimplemented!()
        }
    }

    // ---------- Mock MessageRepo ----------

    #[derive(Default)]
    struct MockMessageRepo {
        messages: Vec<Message>,
    }

    impl MockMessageRepo {
        fn with(messages: Vec<Message>) -> Self {
            Self { messages }
        }
    }

    #[async_trait]
    impl MessageRepo for MockMessageRepo {
        async fn insert_user_and_assistant_stub(
            &self,
            _req: crate::infra::db::repo::message_repo::NewUserMessage,
        ) -> std::result::Result<
            crate::infra::db::repo::message_repo::InsertedPair,
            ChatEngineError,
        > {
            unimplemented!()
        }
        async fn finalize_assistant(
            &self,
            _session_id: Uuid,
            _assistant_message_id: Uuid,
            _outcome: crate::infra::db::repo::message_repo::FinalizeOutcome,
        ) -> std::result::Result<(), ChatEngineError> {
            unimplemented!()
        }
        async fn fetch_active_history(
            &self,
            session_id: Uuid,
            _depth: Option<u32>,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            Ok(self
                .messages
                .iter()
                .filter(|m| m.session_id == session_id)
                .cloned()
                .collect())
        }
        async fn find_message_in_session(
            &self,
            session_id: Uuid,
            message_id: Uuid,
        ) -> std::result::Result<Option<Message>, ChatEngineError> {
            Ok(self
                .messages
                .iter()
                .find(|m| m.session_id == session_id && m.message_id == message_id)
                .cloned())
        }
        async fn list_active_path(
            &self,
            session_id: Uuid,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            let mut out: Vec<Message> = self
                .messages
                .iter()
                .filter(|m| m.session_id == session_id)
                .cloned()
                .collect();
            out.sort_by_key(|m| m.created_at);
            Ok(out)
        }
    }

    // ---------- Fixtures ----------

    fn fixture_session(tenant: &str, user: &str, id: Uuid) -> session_entity::Model {
        session_entity::Model {
            session_id: id,
            tenant_id: tenant.to_string(),
            user_id: user.to_string(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: Some(serde_json::json!({
                "title": "Test Session",
                "tags": ["alpha", "beta"]
            })),
            lifecycle_state: "active".to_string(),
            share_token: None,
            deleted_at: None,
            scheduled_hard_delete_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn fixture_message(
        session_id: Uuid,
        role: MessageRole,
        text: &str,
        offset_secs: i64,
        hidden: bool,
    ) -> Message {
        Message {
            message_id: Uuid::new_v4(),
            session_id,
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role,
            content: serde_json::json!({"text": text}),
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: hidden,
            is_hidden_from_backend: false,
            created_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(offset_secs),
            updated_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(offset_secs),
        }
    }

    fn identity() -> Identity {
        Identity::new("tenant-a", "user-1", None).unwrap()
    }

    fn make_service(
        session: session_entity::Model,
        messages: Vec<Message>,
    ) -> SearchService {
        let sessions = Arc::new(MockSessionRepo::with(session.clone()));
        let message_repo = Arc::new(MockMessageRepo::with(messages.clone()));

        let mut backend = InMemorySearchBackend::new();
        for m in messages {
            backend.push(
                SearchScopeFilter::new(
                    session.tenant_id.clone(),
                    session.user_id.clone(),
                    Some(session.session_id),
                ),
                m,
            );
        }
        let backend = Arc::new(backend);
        SearchService::new(sessions, message_repo, backend)
    }

    // ---------- Tests ----------

    #[test]
    fn parse_query_empty_returns_query_required() {
        let err = parse_search_query("").unwrap_err();
        assert!(matches!(err, SearchError::QueryRequired));
        let err = parse_search_query("   ").unwrap_err();
        assert!(matches!(err, SearchError::QueryRequired));
    }

    #[test]
    fn parse_query_over_length_returns_query_too_long() {
        let raw: String = "a".repeat(MAX_QUERY_LENGTH + 1);
        let err = parse_search_query(&raw).unwrap_err();
        assert!(matches!(err, SearchError::QueryTooLong));
    }

    #[test]
    fn parse_query_only_operators_treated_as_empty() {
        let err = parse_search_query("&|!()").unwrap_err();
        assert!(matches!(err, SearchError::QueryRequired));
    }

    #[test]
    fn parse_query_accepts_normal_input() {
        let parsed = parse_search_query("Hello World").unwrap();
        assert_eq!(parsed.raw, "Hello World");
        assert_eq!(parsed.tsquery, "Hello World");
    }

    #[tokio::test]
    async fn empty_query_returns_400_via_chat_engine_error() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let svc = make_service(session, vec![]);
        let result = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(result, ChatEngineError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn over_length_query_returns_400() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let svc = make_service(session, vec![]);
        let q = "a".repeat(MAX_QUERY_LENGTH + 1);
        let result = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some(q),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        match result {
            ChatEngineError::BadRequest { reason } => {
                assert!(reason.contains("too long"), "got: {reason}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unowned_session_returns_404() {
        let session_id = Uuid::new_v4();
        // Session is owned by a different user.
        let session = fixture_session("tenant-a", "someone-else", session_id);
        let svc = make_service(session, vec![]);
        let result = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("hello".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(result, ChatEngineError::NotFound { .. }));
    }

    #[tokio::test]
    async fn hidden_messages_excluded_from_results() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let messages = vec![
            fixture_message(session_id, MessageRole::User, "find me hidden secret", 0, true),
            fixture_message(session_id, MessageRole::User, "find me", 1, false),
        ];
        let svc = make_service(session, messages);
        let page = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("find me".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total_count, 1);
    }

    #[tokio::test]
    async fn tenant_scoping_blocks_cross_tenant_results() {
        // Backend stores a message for tenant-b — caller is tenant-a.
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        // Mock storage to inject a cross-tenant row.
        let foreign_session = Uuid::new_v4();
        let foreign_msg = fixture_message(
            foreign_session,
            MessageRole::User,
            "find me everywhere",
            0,
            false,
        );
        let sessions = Arc::new(MockSessionRepo::with(session.clone()));
        let mr = Arc::new(MockMessageRepo::with(vec![foreign_msg.clone()]));

        let mut backend = InMemorySearchBackend::new();
        backend.push(
            SearchScopeFilter::new("tenant-b", "user-9", Some(foreign_session)),
            foreign_msg,
        );
        let backend = Arc::new(backend);
        let svc = SearchService::new(sessions, mr, backend);

        let page = svc
            .search_across_sessions(
                &identity(),
                &SearchQuery {
                    q: Some("find me".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 0);
        assert_eq!(page.total_count, 0);
    }

    #[tokio::test]
    async fn pagination_caps_per_page_at_max() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let mut messages = Vec::new();
        for i in 0..80 {
            messages.push(fixture_message(
                session_id,
                MessageRole::User,
                "needle haystack",
                i,
                false,
            ));
        }
        let svc = make_service(session, messages);
        let page = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    top: Some(1000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.per_page, crate::domain::search::MAX_PAGE_SIZE);
        assert_eq!(
            page.items.len(),
            crate::domain::search::MAX_PAGE_SIZE as usize
        );
        assert!(page.next_cursor.is_some());
    }

    #[tokio::test]
    async fn context_window_populated() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let messages = vec![
            fixture_message(session_id, MessageRole::User, "before-1", 0, false),
            fixture_message(session_id, MessageRole::Assistant, "needle here", 1, false),
            fixture_message(session_id, MessageRole::User, "after-1", 2, false),
        ];
        let svc = make_service(session, messages);
        let page = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    context_radius: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        let ctx = &page.items[0].context_messages;
        assert_eq!(ctx.len(), 2);
        // First context message is the "before-1" (chronologically earliest).
        assert!(extract_searchable_text(&ctx[0].content).contains("before-1"));
        assert!(extract_searchable_text(&ctx[1].content).contains("after-1"));
    }

    #[tokio::test]
    async fn cross_session_results_attach_session_metadata() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let messages = vec![fixture_message(
            session_id,
            MessageRole::User,
            "needle haystack",
            0,
            false,
        )];
        let svc = make_service(session, messages);
        let page = svc
            .search_across_sessions(
                &identity(),
                &SearchQuery {
                    q: Some("needle".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        let meta = page.items[0]
            .session_metadata
            .as_ref()
            .expect("cross-session result must carry session metadata");
        assert_eq!(meta.title.as_deref(), Some("Test Session"));
        assert_eq!(meta.tags, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn malformed_cursor_returns_400() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let svc = make_service(session, vec![]);
        let err = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    cursor: Some("not-a-cursor".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn cursor_pages_advance_strictly_past_prior_page() {
        // Pre-fix this test would have failed: the backend's cursor
        // application dropped only the single row whose id matched the
        // cursor, so page 2 returned the first page again (minus that
        // row). With the keyset fix, page 2 must be strictly older than
        // page 1's last row under the `(created_at DESC, message_id
        // DESC)` ordering.
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        // 5 distinct matches with monotonically increasing created_at.
        let messages: Vec<Message> = (0..5)
            .map(|i| {
                fixture_message(
                    session_id,
                    MessageRole::User,
                    &format!("needle row {i}"),
                    i64::from(i),
                    false,
                )
            })
            .collect();
        let svc = make_service(session, messages);

        // Page size 2 → expect three pages: 2 + 2 + 1.
        let page1 = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    top: Some(2),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page1.items.len(), 2, "page 1 size");
        let cursor1 = page1
            .next_cursor
            .clone()
            .expect("page 1 must surface a cursor when more rows exist");

        let page2 = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    top: Some(2),
                    cursor: Some(cursor1.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 2, "page 2 size");

        // Strict disjointness: page 2 must NOT replay any page 1 ids.
        let p1_ids: std::collections::HashSet<Uuid> =
            page1.items.iter().map(|r| r.message_id).collect();
        for r in &page2.items {
            assert!(
                !p1_ids.contains(&r.message_id),
                "page 2 leaked page-1 row {} — cursor skip is broken",
                r.message_id,
            );
        }

        // Sort-order invariant: every page-2 row's created_at must be
        // strictly older than page 1's last row (or equal with a
        // smaller message_id) — the DESC keyset advance condition.
        let p1_last = page1.items.last().expect("page 1 non-empty");
        let p1_last_msg = svc
            .messages
            .find_message_in_session(session_id, p1_last.message_id)
            .await
            .unwrap()
            .expect("page 1 last message present in repo");
        for r in &page2.items {
            let r_msg = svc
                .messages
                .find_message_in_session(session_id, r.message_id)
                .await
                .unwrap()
                .expect("page 2 row present in repo");
            assert!(
                r_msg.created_at < p1_last_msg.created_at
                    || (r_msg.created_at == p1_last_msg.created_at
                        && r_msg.message_id < p1_last_msg.message_id),
                "page 2 row {} is not strictly older than page 1's last row {} \
                 under DESC ordering",
                r_msg.message_id,
                p1_last_msg.message_id,
            );
        }

        // Third (final) page: one row left, no further cursor.
        let cursor2 = page2
            .next_cursor
            .clone()
            .expect("page 2 must surface a cursor when more rows exist");
        let page3 = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    top: Some(2),
                    cursor: Some(cursor2),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page3.items.len(), 1, "page 3 carries the final row");
        assert!(page3.next_cursor.is_none(), "no cursor past the final row");

        // Total ids across all three pages: every input row, exactly once.
        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        for r in page1.items.iter().chain(&page2.items).chain(&page3.items) {
            assert!(seen.insert(r.message_id), "id {} appeared twice", r.message_id);
        }
        assert_eq!(seen.len(), 5, "all 5 rows surfaced across the pages");
    }

    #[tokio::test]
    async fn legacy_cursor_without_created_at_still_advances() {
        // Cursors minted by the pre-fix encoder lack the `:t:<unix_ns>`
        // tail. The backend falls back to a position-based skip so
        // clients holding an in-flight legacy cursor at the cutover are
        // still able to advance instead of looping.
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let messages: Vec<Message> = (0..4)
            .map(|i| {
                fixture_message(
                    session_id,
                    MessageRole::User,
                    &format!("needle row {i}"),
                    i64::from(i),
                    false,
                )
            })
            .collect();
        // Snapshot ids in expected sort order (DESC).
        let mut snapshot = messages.clone();
        snapshot.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });
        let svc = make_service(session, messages);

        // Hand-craft a legacy cursor (no `:t:` tail) pointing at the
        // 2nd row in DESC order — page 2 should start from the 3rd.
        let cursor_target = snapshot[1].message_id;
        let legacy_cursor = format!("r:0:m:{cursor_target}");

        let page = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    top: Some(10),
                    cursor: Some(legacy_cursor),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // Expect the 3rd + 4th rows in DESC order.
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].message_id, snapshot[2].message_id);
        assert_eq!(page.items[1].message_id, snapshot[3].message_id);
    }

    #[tokio::test]
    async fn session_scoped_results_omit_session_metadata() {
        let session_id = Uuid::new_v4();
        let session = fixture_session("tenant-a", "user-1", session_id);
        let messages = vec![fixture_message(
            session_id,
            MessageRole::User,
            "needle",
            0,
            false,
        )];
        let svc = make_service(session, messages);
        let page = svc
            .search_in_session(
                &identity(),
                session_id,
                &SearchQuery {
                    q: Some("needle".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].session_metadata.is_none());
    }
}
