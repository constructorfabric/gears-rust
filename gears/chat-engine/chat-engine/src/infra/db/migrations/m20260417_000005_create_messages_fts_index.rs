// @cpt-cf-chat-engine-dbtable-messages-fts:p11
// @cpt-cf-chat-engine-adr-search-strategy:p11
//
// Phase 11 — Postgres-only GIN FTS index over `messages.content`.
//
// ADR-0019 mandates PostgreSQL `tsvector` + GIN as the production search
// backend. The Phase 1 messages migration intentionally deferred the index
// to this phase because `sea_orm_migration` does not expose
// `USING gin(to_tsvector(...))` portably across backends. We emit the
// index via raw SQL gated on the active backend so SQLite (dev/test)
// gracefully skips the index — the SQLite path uses `ILIKE` and has no
// equivalent expression-index primitive.
//
// The index is created on a functional expression
//   `to_tsvector('english', coalesce(content->>'text', content::text))`
// so messages with the SDK-canonical `{"text": "..."}` shape land in the
// fast path while plugin-defined content shapes still index (via the JSONB
// `::text` fallback) and remain searchable, just with lower precision.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Index name surfaced to `pg_indexes`. Kept stable so operational tooling
/// (REINDEX, ANALYZE) can target it.
pub const MESSAGES_FTS_INDEX: &str = "idx_messages_content_fts_gin";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "CREATE INDEX IF NOT EXISTS idx_messages_content_fts_gin \
                         ON messages \
                         USING gin (to_tsvector('english', coalesce(content->>'text', content::text)))",
                    )
                    .await?;
                // Composite index covering tenant + user search paths: the
                // cross-session search joins through `sessions` so the most
                // frequent predicate is `messages.session_id = ?`. We add
                // a btree covering `(session_id, created_at)` here so the
                // GIN scan can be intersected with the session filter
                // cheaply (the Phase 1 `idx_messages_session_created`
                // already covers this — emit the FTS index ONLY).
            }
            sea_orm::DatabaseBackend::Sqlite => {
                // SQLite path uses `LOWER(content) LIKE LOWER(?)` — no
                // expression index needed (SQLite would only do this via
                // FTS5 which would require a virtual table; out of scope
                // for Phase 11 per ADR-0019). Intentional no-op so the
                // migration succeeds on the dev/test backend.
            }
            sea_orm::DatabaseBackend::MySql => {
                // Out of scope (Chat Engine targets Postgres + SQLite only,
                // see ADR-0019). The migration is a no-op so a misconfigured
                // workspace MySQL doesn't fail outright.
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if matches!(backend, sea_orm::DatabaseBackend::Postgres) {
            manager
                .get_connection()
                .execute_unprepared("DROP INDEX IF EXISTS idx_messages_content_fts_gin")
                .await?;
        }
        Ok(())
    }
}
