//! Index hardening: cover hot predicates that currently force a full table
//! scan, plus one tie-breaker fix on an existing covering index.
//!
//! 1. `idempotency_keys_file_idx` on `idempotency_keys (file_id)`.
//!    `idempotency_keys.file_id` carries `REFERENCES files (file_id) ON
//!    DELETE CASCADE` (`m20260701_000001_p2_initial`), but the table's only
//!    index is `idempotency_keys_expired_idx (expires_at)` — the primary key
//!    is the four-column `(tenant_id, owner_kind, owner_id,
//!    idempotency_key)`, which does not help a lookup keyed by `file_id`.
//!    Every `DELETE FROM files` therefore makes SQLite/Postgres seq-scan the
//!    whole `idempotency_keys` table to find the cascade victims, while
//!    already holding the row lock(s) on `files` inside the deleting
//!    transaction.
//!
//! 2. `multipart_uploads_sweep_idx` on `multipart_uploads (state,
//!    expires_at, lease_until)`. The orphan-reconciliation sweep
//!    (`MultipartRepo::list_expired`) filters on `expires_at < now AND
//!    (state = 'in_progress' OR (state = 'completing' AND lease_until <
//!    now))`. The existing `multipart_uploads_expired_idx` is a *partial*
//!    index restricted to `state = 'in_progress'` on Postgres (plain
//!    `(expires_at, state)` on `SQLite`, where `p2_initial` simply did not
//!    make it partial (`SQLite` has supported partial indexes since 3.8.0) —
//!    either way it does not serve the `completing AND lease_until < now`
//!    half of the OR, so that branch falls back to a full scan. The
//!    new index is deliberately non-partial and leads with `state` so both
//!    branches of the OR can use it.
//!
//! 3. `files_versionless_sweep_idx` on `files (created_at, file_id) WHERE
//!    content_id IS NULL`. The cleanup engine's versionless-orphan-file sweep
//!    (`FileRepo::list_versionless_orphan_files`) filters `content_id IS NULL
//!    AND created_at < cutoff`, ordered by `(created_at, file_id)`; `files`'s
//!    existing indexes are owner/tenant-oriented (`files_owner_listing_idx`,
//!    `files_tenant_gts_idx`) and do not serve this predicate, so the sweep
//!    fell back to a full scan + sort. `SQLite` has supported partial indexes
//!    since 3.8.0, so this is a partial index on both dialects, same as
//!    `multipart_uploads_expired_idx`.
//!
//! 4. `file_versions_file_created_idx` on `file_versions (file_id, created_at,
//!    version_id)`. `VersionRepo::list_by_file` (`GET /files/{id}/versions`,
//!    and the unbounded `Store::list_versions` used by backend migration,
//!    delete/expiry blob accounting, and the sweep engine) filters `file_id =
//!    ?` and sorts `created_at DESC`. The table's only index touching
//!    `file_id` is the composite primary key `(file_id, version_id)`, which
//!    serves the filter but not the sort -- versions are never pruned in P1/
//!    P2 (`docs/migration.sql`), so a long-lived file's version count grows
//!    without bound and every one of those callers pays a full per-file scan
//!    + sort with no supporting index.
//!
//!    Leading with `file_id` (not `created_at`) keeps the composite usable
//!    for the equality filter same as the PK; trailing with `version_id`
//!    also gives `list_by_file`'s own `ORDER BY (created_at, version_id)`
//!    a covering sort order.
//!
//! 5. `files_owner_listing_v2_idx` on `files (tenant_id, owner_kind,
//!    owner_id, created_at DESC, file_id DESC)`, replacing
//!    `files_owner_listing_idx (tenant_id, owner_kind, owner_id, created_at
//!    DESC)` from `m20260624_000001_p1_initial` (already released -- left
//!    untouched, dropped here instead). `FileRepo::list` (`GET /files`) sorts
//!    `ORDER BY created_at DESC, file_id DESC` (the tie-breaker so two
//!    `OFFSET`-paginated pages over rows sharing a `created_at` instant never
//!    skip or repeat a row -- see `tests/store_files_test.rs`'s
//!    `list_orders_by_created_at_then_file_id_so_paged_offsets_do_not_skip_or_repeat`).
//!    The old index's trailing column is only `created_at DESC`, so it serves
//!    the filter and the primary sort key but leaves the `file_id` tie-break
//!    to an extra in-memory sort of every row sharing a `created_at` value;
//!    appending `file_id DESC` makes the index itself already return rows in
//!    exactly `FileRepo::list`'s order.
//!
//! `down()` drops all five new indexes and recreates `files_owner_listing_idx`
//! on both dialects.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
CREATE INDEX IF NOT EXISTS files_versionless_sweep_idx
    ON files (created_at, file_id) WHERE content_id IS NULL;
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
";

const SQLITE_UP: &str = r"
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
CREATE INDEX IF NOT EXISTS files_versionless_sweep_idx
    ON files (created_at, file_id) WHERE content_id IS NULL;
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
";

const DOWN: &str = r"
DROP INDEX IF EXISTS files_owner_listing_v2_idx;
CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
DROP INDEX IF EXISTS file_versions_file_created_idx;
DROP INDEX IF EXISTS files_versionless_sweep_idx;
DROP INDEX IF EXISTS multipart_uploads_sweep_idx;
DROP INDEX IF EXISTS idempotency_keys_file_idx;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            // MySQL and any backend a future `sea_orm` adds to the
            // `#[non_exhaustive]` `DatabaseBackend` enum are refused
            // explicitly here, rather than left to a panic on an
            // uncovered pattern.
            _ => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared(DOWN).await?;
                Ok(())
            }
            // See `up()`'s matching arm.
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
