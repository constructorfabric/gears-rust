//! Tighten `file_versions_part_count_presence_check` (added by
//! `m20260707_000001_content_hash_modes`) to also require `part_count >= 2`
//! whenever it is present.
//!
//! ADR-0006 degenerates a one-part multipart plan to `whole-sha256` with
//! `part_count = NULL` (`multipart_service.rs::assemble_and_finish_inner`,
//! `single_part`/`WholeSha256` branch), so a genuine
//! `multipart-composite-sha256` row must never carry fewer than 2 parts. The
//! presence CHECK `m20260707` shipped only enforced *presence*
//! (`hash_mode = 'multipart-composite-sha256' <=> part_count IS NOT NULL`),
//! not the floor -- a `part_count = 1` composite row satisfied it.
//!
//! This invariant is deliberately shipped as its own, later-dated migration
//! rather than by editing `m20260707`'s SQL in place: `m20260707` has already
//! applied on any environment that ran it, and a migrator never re-runs an
//! already-applied migration, so editing its SQL body is a no-op there --
//! only a fresh database created after the edit would ever see the
//! stronger CHECK. A separately-numbered migration is the only way every
//! environment (fresh or already-migrated) converges on the same schema.
//!
//! **`PostgreSQL`**: one atomic `ALTER TABLE` -- `DROP CONSTRAINT` +
//! `ADD CONSTRAINT` in the same statement, so there is no window with no
//! presence CHECK at all. `ADD CONSTRAINT` validates every existing row by
//! default; the application must never have written a composite row with
//! `part_count = 1` (see above), so if one exists anyway, this migration is
//! *meant* to fail loudly here rather than silently accept the row -- that is
//! a data-integrity bug that needs investigating, not a migration to work
//! around.
//!
//! **`SQLite`**: a `CHECK` constraint cannot be altered in place, only by
//! rebuilding the table, and `file_versions` cannot be rebuilt the way
//! `m20260722`'s `multipart_uploads` rebuild does: `version_hash_manifest`
//! carries `REFERENCES file_versions (version_id) ON DELETE CASCADE`, sqlx
//! runs with `PRAGMA foreign_keys = ON`, and `PRAGMA foreign_keys` cannot be
//! toggled off mid-transaction on `SQLite` -- so the naive rebuild's
//! `DROP TABLE file_versions` would cascade-delete every
//! `version_hash_manifest` row before the table comes back, exactly the bug
//! `m20260722`'s doc comment describes for `multipart_upload_parts`. Instead,
//! the floor is enforced by two `BEFORE INSERT` / `BEFORE UPDATE OF
//! part_count` triggers with a fixed `RAISE(ABORT, ...)` message naming the
//! Postgres constraint, so a violation is identifiable the same way on both
//! dialects. Unlike the Postgres `ADD CONSTRAINT`, a trigger only guards
//! future writes -- it cannot retroactively validate rows already in the
//! table -- but the same "must never happen" reasoning applies: no code path
//! writes a composite row with `part_count = 1`.
//!
//! `down()`: `PostgreSQL` restores the original (Postgres) presence-only CHECK
//! from `m20260707`; `SQLite` drops both triggers.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE file_versions
    DROP CONSTRAINT file_versions_part_count_presence_check,
    ADD CONSTRAINT file_versions_part_count_presence_check
        CHECK ((hash_mode = 'multipart-composite-sha256') = (part_count IS NOT NULL)
               AND (part_count IS NULL OR part_count >= 2));
";

const SQLITE_UP: &str = r"
CREATE TRIGGER trg_file_versions_part_count_floor_insert
    BEFORE INSERT ON file_versions
    FOR EACH ROW
    WHEN NEW.part_count IS NOT NULL AND NEW.part_count < 2
BEGIN
    SELECT RAISE(ABORT, 'file_versions_part_count_presence_check: part_count must be >= 2 when present');
END;
CREATE TRIGGER trg_file_versions_part_count_floor_update
    BEFORE UPDATE OF part_count ON file_versions
    FOR EACH ROW
    WHEN NEW.part_count IS NOT NULL AND NEW.part_count < 2
BEGIN
    SELECT RAISE(ABORT, 'file_versions_part_count_presence_check: part_count must be >= 2 when present');
END;
";

const POSTGRES_DOWN: &str = r"
ALTER TABLE file_versions
    DROP CONSTRAINT file_versions_part_count_presence_check,
    ADD CONSTRAINT file_versions_part_count_presence_check
        CHECK ((hash_mode = 'multipart-composite-sha256') = (part_count IS NOT NULL));
";

const SQLITE_DOWN: &str = r"
DROP TRIGGER IF EXISTS trg_file_versions_part_count_floor_insert;
DROP TRIGGER IF EXISTS trg_file_versions_part_count_floor_update;
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
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
            // See `up()`'s matching arm.
            _ => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
