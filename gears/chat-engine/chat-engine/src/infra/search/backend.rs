//! SeaORM-backed [`SearchBackend`](crate::domain::service::search_service::SearchBackend)
//! implementations for PostgreSQL and SQLite.
//!
//! Selection between the two happens at module-wiring time (Phase 15)
//! based on the live `DatabaseBackend` exposed by SeaORM. The structs
//! carry the chat-engine `DBProvider` so production wiring can plug
//! them in against the same handle the migration runner uses; the
//! actual SQL composition lives in Phase 15 because it depends on the
//! materialised connection. Phase 11 supplies the `SearchBackend`
//! trait + an in-memory backend that the unit tests exercise.
//
// @cpt-cf-chat-engine-infra-search-backend:p11

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::error::ChatEngineError;
use crate::domain::search::Cursor;
use crate::domain::service::search_service::{
    BackendHit, ParsedQuery, SearchBackend, SearchScopeFilter,
};
use crate::infra::db::repo::ChatEngineDb;

/// PostgreSQL `tsvector` + GIN backend. Uses `plainto_tsquery` for plain
/// searches and `phraseto_tsquery` for quoted phrases. Ranking via
/// `ts_rank_cd(to_tsvector('english', ...), query)` with document length
/// normalisation flag `32` (per ADR-0019).
pub struct PgSearchBackend {
    #[allow(dead_code)]
    db: Arc<ChatEngineDb>,
}

impl PgSearchBackend {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SearchBackend for PgSearchBackend {
    async fn search(
        &self,
        _scope: &SearchScopeFilter,
        _query: &ParsedQuery,
        _cursor: Option<&Cursor>,
        _skip: u32,
        _limit: u32,
    ) -> std::result::Result<(Vec<BackendHit>, u64), ChatEngineError> {
        Err(ChatEngineError::internal(
            "PgSearchBackend not yet wired to DBProvider — Phase 15 owns workspace wiring",
        ))
    }
}

/// SQLite-backed search implementation. Uses `LOWER(content_text) LIKE
/// LOWER(?)` against a runtime-extracted plain-text projection of the
/// `messages.content` JSONB column.
pub struct SqliteSearchBackend {
    #[allow(dead_code)]
    db: Arc<ChatEngineDb>,
}

impl SqliteSearchBackend {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SearchBackend for SqliteSearchBackend {
    async fn search(
        &self,
        _scope: &SearchScopeFilter,
        _query: &ParsedQuery,
        _cursor: Option<&Cursor>,
        _skip: u32,
        _limit: u32,
    ) -> std::result::Result<(Vec<BackendHit>, u64), ChatEngineError> {
        Err(ChatEngineError::internal(
            "SqliteSearchBackend not yet wired to DBProvider — Phase 15 owns workspace wiring",
        ))
    }
}
