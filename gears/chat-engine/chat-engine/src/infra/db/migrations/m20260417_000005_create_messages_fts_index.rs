// @cpt-cf-chat-engine-dbtable-message-parts:p11
// @cpt-cf-chat-engine-adr-search-strategy:p11
//
// Phase 11 — Postgres-only GIN FTS index over `message_parts` text content.
//
// ADR-0019 mandates PostgreSQL `tsvector` + GIN as the production search
// backend. The message body lives in `message_parts` (ordered typed parts),
// so full-text search targets the `text`-typed parts. The index is deferred
// to this migration because `sea_orm_migration` does not expose
// `USING gin(to_tsvector(...))` portably across backends. We emit it via raw
// SQL gated on the active backend so SQLite (dev/test) gracefully skips it —
// the SQLite path uses `LIKE` and has no equivalent expression-index primitive.
//
// The index is partial (`WHERE type = 'text'`) over the functional expression
//   `to_tsvector('english', content->>'text')`
// so only text parts — whose canonical shape is `{"text": "..."}` — are
// indexed; non-text parts (images/videos/links/statuses) are excluded from
// text search per FR-022.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Index name surfaced to `pg_indexes`. Kept stable so operational tooling
/// (REINDEX, ANALYZE) can target it.
pub const MESSAGES_FTS_INDEX: &str = "idx_message_parts_text_fts_gin";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "CREATE INDEX IF NOT EXISTS idx_message_parts_text_fts_gin \
                         ON message_parts \
                         USING gin (to_tsvector('english', content->>'text')) \
                         WHERE type = 'text'",
                    )
                    .await?;
                // The cross-session search joins parts → messages → sessions;
                // the GIN scan above is intersected with the message/session
                // filters via the Phase 1 btree indexes. Emit the FTS index
                // ONLY here.
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
                .execute_unprepared("DROP INDEX IF EXISTS idx_message_parts_text_fts_gin")
                .await?;
        }
        Ok(())
    }
}
