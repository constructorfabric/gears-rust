//! Message search service (Phase 11).
//!
//! Owns the `GET /sessions/{id}/search` (within-session) and `GET /search`
//! (cross-session) flows. The service is dual-backend: the PostgreSQL path
//! uses `to_tsvector`/`plainto_tsquery`/`ts_rank_cd` with a GIN index (added
//! by the Phase 11 deferred migration), and the SQLite/dev-test path falls
//! back to `LOWER(content) LIKE LOWER(?)` with no ranking. Both backend
//! impls ship as concrete structs (`crate::infra::search::PgSearchBackend`,
//! `crate::infra::search::SqliteSearchBackend`) that compile
//! unconditionally — the `toolkit-db` workspace dependency enables BOTH
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
use toolkit_macros::domain_model;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::{Message, MessagePart, MessageRole, message_text};
use crate::domain::ports::MessageRepo;
use crate::domain::ports::SessionRepo;
use crate::domain::authz::{actions, resource_types};
use crate::domain::search::{
    Cursor, MAX_QUERY_LENGTH, MessageRef, SearchError, SearchPage, SearchQuery, SearchResult,
    SessionMeta, make_snippet, sanitize_for_tsquery,
};
use authz_resolver_sdk::pep::PolicyEnforcer;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};

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
    pub parts: Vec<MessagePart>,
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
                let text = message_text(&m.parts);
                text.to_lowercase().contains(&needle)
            })
            .map(|(_, m)| BackendHit {
                message_id: m.message_id,
                session_id: m.session_id,
                parent_message_id: m.parent_message_id,
                role: m.role.clone(),
                parts: m.parts.clone(),
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

/// Build the backend [`SearchScopeFilter`] from a PDP-compiled
/// [`AccessScope`]. Owner tenant/user are taken from the scope's compiled
/// `owner_tenant_id` / `owner_id` constraints so the backend filters exactly
/// the rows the PDP authorized. An unconstrained scope (allow-all — the
/// static-authz plugin only returns this in tests) falls back to the
/// authenticated subject's ids.
fn scope_filter_from_access(
    scope: &AccessScope,
    ctx: &SecurityContext,
    session_id: Option<Uuid>,
) -> SearchScopeFilter {
    if scope.is_unconstrained() {
        return SearchScopeFilter {
            tenant_id: ctx.subject_tenant_id().to_string(),
            user_id: ctx.subject_id().to_string(),
            session_id,
        };
    }
    let tenant_id = scope
        .all_uuid_values_for(pep_properties::OWNER_TENANT_ID)
        .first()
        .map(ToString::to_string)
        .unwrap_or_else(|| ctx.subject_tenant_id().to_string());
    let user_id = scope
        .all_uuid_values_for(pep_properties::OWNER_ID)
        .first()
        .map(ToString::to_string)
        .unwrap_or_else(|| ctx.subject_id().to_string());
    SearchScopeFilter {
        tenant_id,
        user_id,
        session_id,
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
    enforcer: PolicyEnforcer,
}

impl SearchService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        messages: Arc<dyn MessageRepo>,
        backend: Arc<dyn SearchBackend>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            sessions,
            messages,
            backend,
            enforcer,
        }
    }

    /// Session-scoped search. Validates session ownership BEFORE running the
    /// search (per Phase 11 Rules §Scoping and Security).
    #[instrument(skip(self, ctx, query), fields(session_id = %session_id))]
    pub async fn search_in_session(
        &self,
        ctx: &SecurityContext,
        session_id: Uuid,
        query: &SearchQuery,
    ) -> Result<SearchPage> {
        let started = Instant::now();
        let parsed =
            parse_search_query(query.q.as_deref().unwrap_or("")).map_err(ChatEngineError::from)?;

        // Within-session search is a MESSAGE list; the PDP returns owner
        // constraints and the backend applies them (plus the session_id filter).
        // @cpt-cf-chat-engine-seq-authz-list
        // @cpt-cf-chat-engine-interface-pep
        // @cpt-cf-chat-engine-constraint-fail-closed-authz
        let access = self
            .enforcer
            .access_scope(ctx, &resource_types::MESSAGE, actions::LIST, None)
            .await?;
        let scope = scope_filter_from_access(&access, ctx, Some(session_id));
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
    #[instrument(skip(self, ctx, query))]
    pub async fn search_across_sessions(
        &self,
        ctx: &SecurityContext,
        query: &SearchQuery,
    ) -> Result<SearchPage> {
        let started = Instant::now();
        let parsed =
            parse_search_query(query.q.as_deref().unwrap_or("")).map_err(ChatEngineError::from)?;

        // Cross-session search is a SESSION list; the PDP returns owner
        // constraints applied by the backend's sessions join.
        // @cpt-cf-chat-engine-seq-authz-list
        // @cpt-cf-chat-engine-interface-pep
        // @cpt-cf-chat-engine-constraint-fail-closed-authz
        let access = self
            .enforcer
            .access_scope(ctx, &resource_types::SESSION, actions::LIST, None)
            .await?;
        let scope = scope_filter_from_access(&access, ctx, None);
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
            let snippet = make_snippet(&message_text(&hit.parts), &parsed.raw);
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
                parts: m.parts.clone(),
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
                parts: m.parts.clone(),
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
            .map(std::string::ToString::to_string);
        let tags = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("tags"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
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
#[path = "search_service_tests.rs"]
mod search_service_tests;
