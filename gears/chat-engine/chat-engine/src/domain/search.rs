//! Message search domain primitives (Phase 11).
//!
//! Owns the wire-shape DTOs and the cursor/pagination primitives used by the
//! `GET /sessions/{id}/search?q=...` (within-session) and `GET /search?q=...`
//! (cross-session) surfaces. The types here are framework-neutral; REST glue
//! lives in `api/rest/handlers/search.rs`, orchestration in
//! `domain/service/search_service.rs`.
//!
//! Two invariants drive the shapes below.
//!
//! 1. Search query input is bearer-untrusted text. The
//!    [`sanitize_for_tsquery`] free helper strips characters that would
//!    otherwise be interpreted as `tsquery` operators (`&`, `|`, `!`, parens,
//!    `<->`) — this prevents `tsquery` injection on the PostgreSQL path. The
//!    SQLite/ILIKE path uses [`escape_like_pattern`] which escapes `%` / `_`
//!    so user input cannot smuggle SQL wildcards.
//!
//! 2. The cross-session response MUST NOT include `user_id` or `tenant_id`
//!    even though those columns are part of the underlying join — the
//!    [`SessionMeta`] envelope is deliberately minimal. The session id is
//!    surfaced because the caller already owns it.
//
// @cpt-cf-chat-engine-domain-search:p11
// @cpt-cf-chat-engine-adr-search-strategy:p11

use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::message::MessageRole;

/// Maximum allowed query string length (characters). Inputs longer than this
/// are rejected with `QueryTooLong` before any sanitisation runs.
pub const MAX_QUERY_LENGTH: usize = 500;

/// Default page size when the caller does not pass `$top`.
pub const DEFAULT_PAGE_SIZE: u32 = 20;

/// Maximum page size. Larger requests are silently clamped to this value.
pub const MAX_PAGE_SIZE: u32 = 50;

/// Default context-window radius. The service returns N messages before and
/// N messages after each match in chronological order.
pub const DEFAULT_CONTEXT_RADIUS: u32 = 1;

/// Query DTO for the search endpoints.
///
/// `$top` and `$skip` mirror the OData spec; the `cursor` field is reserved
/// for the keyset pagination payload (encoded `(rank, message_id)`). The
/// `toolkit-odata`-derived `ODataQuery` lifts these field names verbatim,
/// hence the leading `$` on the wire (serde rename).
#[domain_model]
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchQuery {
    /// Raw query string. Empty or > `MAX_QUERY_LENGTH` characters → 400.
    pub q: Option<String>,
    /// OData `$top` — maximum number of results per page. Defaults to
    /// [`DEFAULT_PAGE_SIZE`]; clamped to [`MAX_PAGE_SIZE`].
    #[serde(rename = "$top", default)]
    pub top: Option<u32>,
    /// OData `$skip` — number of results to skip. Mutually exclusive with
    /// `cursor` in practice; when both are set `cursor` takes precedence.
    #[serde(rename = "$skip", default)]
    pub skip: Option<u32>,
    /// Opaque cursor token from a previous response. When supplied the
    /// service decodes it to a `(rank, message_id)` keyset.
    pub cursor: Option<String>,
    /// Context-window radius (N before / N after). Defaults to
    /// [`DEFAULT_CONTEXT_RADIUS`]; clamped to 5 to avoid blowing up the
    /// response.
    #[serde(rename = "context")]
    pub context_radius: Option<u32>,
}

impl SearchQuery {
    /// Effective `top` value after defaulting + clamping (1..=MAX_PAGE_SIZE).
    #[must_use]
    pub fn effective_top(&self) -> u32 {
        match self.top {
            None => DEFAULT_PAGE_SIZE,
            Some(0) => DEFAULT_PAGE_SIZE,
            Some(n) => n.min(MAX_PAGE_SIZE),
        }
    }

    /// Effective `skip` value (defaults to 0).
    #[must_use]
    pub fn effective_skip(&self) -> u32 {
        self.skip.unwrap_or(0)
    }

    /// Effective context window (defaults to [`DEFAULT_CONTEXT_RADIUS`],
    /// clamped to 5).
    #[must_use]
    pub fn effective_context_radius(&self) -> u32 {
        self.context_radius.unwrap_or(DEFAULT_CONTEXT_RADIUS).min(5)
    }
}

/// One enriched search hit returned to the caller.
///
/// `context_messages` carries the N-before / N-after window (chronological
/// order, hidden rows excluded). `parent_chain` carries the ancestor chain
/// from the matched message up to the session root, in root-first order, so
/// the UI can render thread context.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub message_id: Uuid,
    pub session_id: Uuid,
    /// Snippet of the matched content suitable for inline rendering.
    /// Truncated to ~120 chars around the first match.
    pub content_snippet: String,
    /// Relevance score. PostgreSQL path emits `ts_rank_cd`; SQLite path
    /// returns `0.0` because plain ILIKE has no ranking semantics.
    pub rank: f32,
    /// Hidden rows are filtered out before this list is populated.
    pub context_messages: Vec<MessageRef>,
    /// Root-first ancestry chain (empty when the match is itself a root).
    pub parent_chain: Vec<MessageRef>,
    /// Cross-session results carry the session-level metadata so the UI
    /// can render the result outside the source session. Session-scoped
    /// results leave this `None` to keep the payload small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_metadata: Option<SessionMeta>,
}

/// Compact reference to a message — enough to render in the search result
/// page without re-fetching the message body in a follow-up request.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct MessageRef {
    pub message_id: Uuid,
    pub role: MessageRole,
    /// Raw content JSONB. The UI is responsible for projecting `content.text`
    /// (SDK convention) versus structured content parts.
    pub content: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

/// Minimal session envelope embedded in cross-session results.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct SessionMeta {
    pub session_id: Uuid,
    pub title: Option<String>,
    /// Optional tag-like labels lifted from `session.metadata.tags`. Empty
    /// when no `tags` key is present or it is not a JSON array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Paginated search response — the canonical wire shape.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct SearchPage {
    pub items: Vec<SearchResult>,
    /// Total number of matches available (no pagination applied). On the
    /// SQLite path this is the count of all matching rows; on the
    /// PostgreSQL path it is the count of matching `tsquery` rows.
    pub total_count: u64,
    /// Opaque continuation cursor; `None` when no further pages exist.
    pub next_cursor: Option<String>,
    /// Echoed `per_page` value so the client can confirm clamping.
    pub per_page: u32,
}

/// Search-specific error type. Kept distinct from [`ChatEngineError`] so the
/// service can return structured failures without losing the "this is a
/// search-input problem" classification at the handler boundary.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// `q` was missing or empty — maps to HTTP 400.
    #[error("query required")]
    QueryRequired,
    /// `q` exceeded [`MAX_QUERY_LENGTH`] — maps to HTTP 400.
    #[error("query too long")]
    QueryTooLong,
    /// Session lookup returned `None` — maps to HTTP 404.
    #[error("session not found")]
    SessionNotFound,
    /// Identity claims absent or session not owned by caller — maps to 403.
    #[error("forbidden")]
    Forbidden,
    /// Underlying backend (DB / sea-orm) failure — maps to 500. Wraps the
    /// originating [`ChatEngineError`] as a source so the chain survives the
    /// round-trip (DE1302).
    #[error("backend error: {0}")]
    Backend(#[source] Box<ChatEngineError>),
}

impl From<SearchError> for ChatEngineError {
    fn from(err: SearchError) -> Self {
        match err {
            SearchError::QueryRequired => ChatEngineError::bad_request("query required"),
            SearchError::QueryTooLong => ChatEngineError::bad_request("query too long"),
            SearchError::SessionNotFound => ChatEngineError::not_found("session", "<scoped>"),
            SearchError::Forbidden => {
                ChatEngineError::forbidden("authenticated identity required to perform search")
            }
            SearchError::Backend(err) => ChatEngineError::Internal {
                reason: "search backend error".to_owned(),
                source: Some(err),
            },
        }
    }
}

impl From<ChatEngineError> for SearchError {
    fn from(err: ChatEngineError) -> Self {
        match err {
            ChatEngineError::NotFound { .. } => SearchError::SessionNotFound,
            ChatEngineError::Forbidden { .. } => SearchError::Forbidden,
            ChatEngineError::BadRequest { reason } => {
                if reason.contains("too long") {
                    SearchError::QueryTooLong
                } else {
                    SearchError::QueryRequired
                }
            }
            other => SearchError::Backend(Box::new(other)),
        }
    }
}

/// Cursor encoding for the `(created_at, message_id)` keyset (with an
/// optional `rank` carried for backends that score). Keyset pagination
/// requires the cursor key to match the *sort* key — the backend
/// produces results ordered by `(created_at DESC, message_id DESC)`, so
/// the cursor MUST surface enough information to drop every hit ordered
/// at-or-before it.
///
/// ## Wire format
///
/// Encoded as `r:<rank>:m:<uuid>:t:<unix_ns>` where `unix_ns` is the
/// hit's `created_at` expressed as Unix nanoseconds (signed `i128`).
/// The trailing `:t:<unix_ns>` segment is the load-bearing addition vs.
/// earlier releases — it carries the sort key the backend actually uses
/// to skip rows. The leading `r:<rank>:m:<uuid>` prefix is preserved so
/// any in-flight client cursor minted by the prior release still
/// decodes; legacy cursors land with `created_at = None` and the backend
/// falls back to a position-based drop (best-effort).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub rank: f32,
    pub message_id: Uuid,
    /// Sort-key timestamp — the cursor row's `created_at`. `None` only
    /// for legacy cursors minted before this field existed; new cursors
    /// always populate it so the backend can perform a real keyset skip.
    pub created_at: Option<time::OffsetDateTime>,
}

impl Cursor {
    #[must_use]
    pub fn new(rank: f32, message_id: Uuid, created_at: time::OffsetDateTime) -> Self {
        Self {
            rank,
            message_id,
            created_at: Some(created_at),
        }
    }

    /// Encode the cursor as `r:<rank>:m:<uuid>:t:<unix_ns>`. Stable
    /// across releases — legacy decoders that stop after the `:m:<uuid>`
    /// segment silently ignore the trailing `:t:<unix_ns>` (they parse
    /// the UUID with `Uuid::parse_str` which rejects trailing garbage,
    /// so a strict legacy parser would surface a malformed-cursor error;
    /// new encoders intentionally always emit the new shape so a server
    /// downgrade is detectable rather than silently corrupting paging).
    #[must_use]
    pub fn encode(&self) -> String {
        match self.created_at {
            Some(ts) => format!(
                "r:{}:m:{}:t:{}",
                self.rank,
                self.message_id,
                ts.unix_timestamp_nanos(),
            ),
            // Legacy emit — should never happen because Cursor::new
            // always sets created_at. Defensive: emit the prior wire
            // format so any code path still using the old constructor
            // shape remains decodable.
            None => format!("r:{}:m:{}", self.rank, self.message_id),
        }
    }

    /// Decode a cursor produced by [`Self::encode`] (current OR legacy
    /// `r:<rank>:m:<uuid>` form). Returns `SearchError::QueryRequired`
    /// for any malformed input — the cursor is opaque to the caller, so
    /// we treat malformed cursors as a client programming bug rather
    /// than a backend failure.
    pub fn decode(raw: &str) -> Result<Self, SearchError> {
        let mut parts = raw.split(':');
        let r_tag = parts.next().ok_or(SearchError::QueryRequired)?;
        let rank_str = parts.next().ok_or(SearchError::QueryRequired)?;
        let m_tag = parts.next().ok_or(SearchError::QueryRequired)?;
        let id_str = parts.next().ok_or(SearchError::QueryRequired)?;
        if r_tag != "r" || m_tag != "m" {
            return Err(SearchError::QueryRequired);
        }
        let rank: f32 = rank_str.parse().map_err(|_| SearchError::QueryRequired)?;
        let message_id = Uuid::parse_str(id_str).map_err(|_| SearchError::QueryRequired)?;

        // Optional `:t:<unix_ns>` tail. Present on cursors minted by
        // the current encoder; absent on legacy cursors round-tripped
        // by a pre-fix client.
        let created_at = match (parts.next(), parts.next()) {
            (Some("t"), Some(ts_str)) => {
                let nanos: i128 = ts_str.parse().map_err(|_| SearchError::QueryRequired)?;
                Some(
                    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
                        .map_err(|_| SearchError::QueryRequired)?,
                )
            }
            (None, None) => None,
            // Any other shape is a malformed cursor.
            _ => return Err(SearchError::QueryRequired),
        };

        // Reject trailing junk past the optional tail.
        if parts.next().is_some() {
            return Err(SearchError::QueryRequired);
        }

        Ok(Self {
            rank,
            message_id,
            created_at,
        })
    }
}

/// Sanitise raw user input for safe use in a PostgreSQL `tsquery` expression.
///
/// Strategy: strip every character that PostgreSQL would interpret as a
/// `tsquery` operator (`&`, `|`, `!`, parens, `<->`, `:`) and collapse
/// internal whitespace. The result is suitable for `plainto_tsquery` /
/// `phraseto_tsquery` — both of which already perform additional escaping —
/// but we strip the dangerous characters at the domain boundary so a future
/// caller using `to_tsquery` directly cannot accidentally inherit an
/// injection path.
#[must_use]
pub fn sanitize_for_tsquery(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_space = false;
    for ch in raw.chars() {
        match ch {
            '&' | '|' | '!' | '(' | ')' | ':' | '<' | '>' | '\'' | '"' | '\\' => {
                // drop entirely
            }
            c if c.is_whitespace() => {
                if !prev_space && !out.is_empty() {
                    out.push(' ');
                    prev_space = true;
                }
            }
            c => {
                out.push(c);
                prev_space = false;
            }
        }
    }
    let trimmed = out.trim_end();
    trimmed.to_string()
}

/// Escape `%` and `_` so they are matched literally inside a SQL `LIKE`
/// pattern. The escape character is `\` — every backend Chat Engine talks
/// to supports `ESCAPE '\'`.
#[must_use]
pub fn escape_like_pattern(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            c => out.push(c),
        }
    }
    out
}

/// Build a content snippet of at most ~120 characters around the first
/// occurrence of `needle` (case-insensitive). Falls back to the leading
/// 120 characters when no match is found inside the content text.
#[must_use]
pub fn make_snippet(content_text: &str, needle: &str) -> String {
    const RADIUS: usize = 60;
    const MAX: usize = 120;

    let haystack_lower = content_text.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let start = haystack_lower.find(&needle_lower);

    let body: String = match start {
        Some(idx) => {
            let begin = idx.saturating_sub(RADIUS);
            let end = (idx + needle.len() + RADIUS).min(content_text.len());
            // Char-boundary safe slice: walk forward/back to a valid index.
            let begin = floor_char_boundary(content_text, begin);
            let end = ceil_char_boundary(content_text, end);
            let mut s = String::new();
            if begin > 0 {
                s.push('\u{2026}');
            }
            s.push_str(&content_text[begin..end]);
            if end < content_text.len() {
                s.push('\u{2026}');
            }
            s
        }
        None => {
            let take = MAX.min(content_text.len());
            let end = ceil_char_boundary(content_text, take);
            let mut s = content_text[..end].to_string();
            if end < content_text.len() {
                s.push('\u{2026}');
            }
            s
        }
    };
    body
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Extract searchable plain text from a JSONB message `content` payload.
/// Mirrors the SDK convention `content.text: String` (Phase 5 + ADR-0006)
/// while remaining robust against plugin-defined content shapes (content
/// parts, tool calls, …).
#[must_use]
pub fn extract_searchable_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get("text") {
                return s.clone();
            }
            // Fall back to concatenating any nested `text` string fields
            // we can find at depth 1 — keeps plugin-defined content parts
            // searchable without requiring a full schema.
            let mut buf = String::new();
            for v in map.values() {
                push_text(v, &mut buf);
            }
            buf
        }
        serde_json::Value::Array(arr) => {
            let mut buf = String::new();
            for v in arr {
                push_text(v, &mut buf);
            }
            buf
        }
        _ => String::new(),
    }
}

fn push_text(v: &serde_json::Value, buf: &mut String) {
    match v {
        serde_json::Value::String(s) => {
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(s);
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get("text") {
                if !buf.is_empty() {
                    buf.push(' ');
                }
                buf.push_str(s);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    #[test]
    fn search_query_effective_top_defaults_and_clamps() {
        let q = SearchQuery::default();
        assert_eq!(q.effective_top(), DEFAULT_PAGE_SIZE);

        let q = SearchQuery {
            top: Some(1000),
            ..Default::default()
        };
        assert_eq!(q.effective_top(), MAX_PAGE_SIZE);

        let q = SearchQuery {
            top: Some(0),
            ..Default::default()
        };
        assert_eq!(q.effective_top(), DEFAULT_PAGE_SIZE);

        let q = SearchQuery {
            top: Some(7),
            ..Default::default()
        };
        assert_eq!(q.effective_top(), 7);
    }

    #[test]
    fn search_query_effective_skip_defaults_to_zero() {
        let q = SearchQuery::default();
        assert_eq!(q.effective_skip(), 0);
        let q = SearchQuery {
            skip: Some(42),
            ..Default::default()
        };
        assert_eq!(q.effective_skip(), 42);
    }

    #[test]
    fn search_query_context_radius_clamped() {
        let q = SearchQuery {
            context_radius: Some(50),
            ..Default::default()
        };
        assert_eq!(q.effective_context_radius(), 5);
        let q = SearchQuery {
            context_radius: None,
            ..Default::default()
        };
        assert_eq!(q.effective_context_radius(), DEFAULT_CONTEXT_RADIUS);
    }

    #[test]
    fn sanitize_strips_tsquery_operators() {
        let s = sanitize_for_tsquery("hello & world | foo!");
        // Operators are dropped and surrounding whitespace collapses to a
        // single space (consecutive whitespace runs are coalesced).
        assert_eq!(s, "hello world foo");
    }

    #[test]
    fn sanitize_drops_parens_and_quotes() {
        let s = sanitize_for_tsquery("(quick \"brown\" fox)");
        assert_eq!(s, "quick brown fox");
    }

    #[test]
    fn sanitize_collapses_whitespace() {
        let s = sanitize_for_tsquery("   foo \t\n bar   ");
        assert_eq!(s, "foo bar");
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like_pattern("100%"), "100\\%");
        assert_eq!(escape_like_pattern("a_b"), "a\\_b");
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
        assert_eq!(escape_like_pattern("plain"), "plain");
    }

    #[test]
    fn snippet_centers_on_match() {
        let content = "the quick brown fox jumps over the lazy dog and runs across the field";
        let s = make_snippet(content, "brown");
        assert!(s.contains("brown"));
    }

    #[test]
    fn snippet_falls_back_on_no_match() {
        let content = "short body";
        let s = make_snippet(content, "missing");
        assert_eq!(s, "short body");
    }

    #[test]
    fn extract_text_from_sdk_shape() {
        let v = serde_json::json!({ "text": "hello world" });
        assert_eq!(extract_searchable_text(&v), "hello world");
    }

    #[test]
    fn extract_text_from_nested_parts() {
        let v = serde_json::json!([{ "text": "alpha" }, { "text": "beta" }]);
        assert_eq!(extract_searchable_text(&v), "alpha beta");
    }

    #[test]
    fn extract_text_handles_unknown_shape() {
        let v = serde_json::json!({ "foo": 42 });
        assert_eq!(extract_searchable_text(&v), "");
    }

    #[test]
    fn cursor_round_trip() {
        let ts = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(123);
        let c = Cursor::new(0.42, Uuid::nil(), ts);
        let encoded = c.encode();
        let decoded = Cursor::decode(&encoded).unwrap();
        assert_eq!(decoded.message_id, Uuid::nil());
        assert!((decoded.rank - 0.42).abs() < 1e-5);
        assert_eq!(
            decoded.created_at,
            Some(ts),
            "round-tripped cursor must preserve the created_at sort key",
        );
    }

    #[test]
    fn cursor_decodes_legacy_format_without_created_at() {
        // Cursors minted before the `:t:<unix_ns>` tail was added MUST
        // still decode — clients in flight at the cutover hold them.
        let decoded = Cursor::decode("r:0.42:m:00000000-0000-0000-0000-000000000000").unwrap();
        assert_eq!(decoded.message_id, Uuid::nil());
        assert!(
            decoded.created_at.is_none(),
            "legacy cursor must surface as created_at=None so the backend falls \
             back to the position-based skip path",
        );
    }

    #[test]
    fn cursor_rejects_malformed_input() {
        assert!(Cursor::decode("garbage").is_err());
        assert!(Cursor::decode("r:notafloat:m:nil").is_err());
        assert!(Cursor::decode("x:1.0:m:00000000-0000-0000-0000-000000000000").is_err());
        // Trailing junk past the optional `:t:<unix_ns>` tail.
        assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:0:bogus").is_err());
        // `t` segment without a value.
        assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:").is_err());
        // `t` segment with a non-numeric value.
        assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:notanint").is_err());
    }

    #[test]
    fn search_error_maps_to_chat_engine_error() {
        let err: ChatEngineError = SearchError::QueryRequired.into();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));

        let err: ChatEngineError = SearchError::QueryTooLong.into();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));

        let err: ChatEngineError = SearchError::SessionNotFound.into();
        assert!(matches!(err, ChatEngineError::NotFound { .. }));

        let err: ChatEngineError = SearchError::Forbidden.into();
        assert!(matches!(err, ChatEngineError::Forbidden { .. }));

        let err: ChatEngineError =
            SearchError::Backend(Box::new(ChatEngineError::internal("boom"))).into();
        assert!(matches!(err, ChatEngineError::Internal { .. }));
    }

    #[test]
    fn message_ref_serializes_with_rfc3339() {
        let r = MessageRef {
            message_id: Uuid::nil(),
            role: MessageRole::User,
            content: serde_json::json!({"text": "x"}),
            created_at: OffsetDateTime::UNIX_EPOCH,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("1970-01-01"));
    }
}
