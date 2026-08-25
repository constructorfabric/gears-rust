//! Migration `m0006` — add four nullable per-transition audit comment
//! columns to `conversion_requests` (`requested_comment`,
//! `approved_comment`, `cancelled_comment`, `rejected_comment`) with
//! `length BETWEEN 1 AND 1000` `CHECK` constraints.
//!
//! Each conversion-request lifecycle transition (request / approve /
//! cancel / reject) MAY carry an optional caller-supplied rationale
//! string persisted to its dedicated column. Per-decision storage (one
//! column per transition rather than a single rewriteable
//! `audit_comment`) preserves the full audit story across the lifecycle:
//! the counterparty's "why approved" cannot overwrite the initiator's
//! "why requested".
//!
//! All four columns are `NULL`-able and the `CHECK` is null-permissive —
//! existing rows pre-`m0006` keep their values intact and absent
//! comments stay absent. The `1..=1000` length guard is pinned at the
//! DB layer so a service-layer mis-write cannot persist an empty string
//! (`Some("")` is a contract bug, not a "no comment" sentinel) nor an
//! oversized payload; the service layer enforces the same range in
//! `request_conversion`, `approve`, `cancel`, `reject` as
//! defence-in-depth.
//!
//! No new indexes: comment columns are not filterable / orderable on
//! the public `OData` surface and never appear in a `WHERE` predicate
//! outside of point reads by `id`.
//!
//! Backend coverage: per-backend raw DDL mirrors the m0002 / m0004
//! convention. `SQLite` supports inline `CHECK` clauses on `ADD COLUMN`
//! since 3.25 (every supported AM `SQLite` target satisfies that).
//! `MySQL` is unsupported and returns
//! `DbErr::Custom(MYSQL_NOT_SUPPORTED)`.
//!
//! Rollback (`down()`) is destructive. The four `DROP COLUMN`
//! statements unconditionally remove the audit-comment columns and
//! every value persisted while the migration was active — including
//! comments written between `up()` and the rollback. There is no
//! recovery path; operators rolling back in production MUST snapshot
//! `requested_comment` / `approved_comment` / `cancelled_comment` /
//! `rejected_comment` first if the audit trail is load-bearing.
//! Postgres uses `DROP COLUMN IF EXISTS` so re-running `down()` is
//! idempotent; `SQLite`'s `ALTER TABLE ... DROP COLUMN` (added in
//! 3.35) does NOT accept `IF EXISTS` — `down()` is therefore single-
//! shot on `SQLite`, which is acceptable because `SQLite` is only used
//! by per-test in-memory databases that are torn down between runs.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "account-management migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite); add a dedicated MySQL migration set \
    before running against MySQL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "ALTER TABLE conversion_requests ADD COLUMN IF NOT EXISTS requested_comment TEXT NULL CONSTRAINT ck_conversion_requests_requested_comment_len CHECK (length(requested_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN IF NOT EXISTS approved_comment TEXT NULL CONSTRAINT ck_conversion_requests_approved_comment_len CHECK (length(approved_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN IF NOT EXISTS cancelled_comment TEXT NULL CONSTRAINT ck_conversion_requests_cancelled_comment_len CHECK (length(cancelled_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN IF NOT EXISTS rejected_comment TEXT NULL CONSTRAINT ck_conversion_requests_rejected_comment_len CHECK (length(rejected_comment) BETWEEN 1 AND 1000);",
            ],
            // SQLite does not support `IF NOT EXISTS` on `ADD COLUMN`
            // (it parses the keyword but errors at planning time). The
            // migration is therefore not idempotent on SQLite — but
            // SQLite is only used for in-process tests where the
            // migrator runs against a freshly-created database per test,
            // so re-application is not a concern.
            sea_orm::DatabaseBackend::Sqlite => vec![
                "ALTER TABLE conversion_requests ADD COLUMN requested_comment TEXT NULL CONSTRAINT ck_conversion_requests_requested_comment_len CHECK (length(requested_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN approved_comment TEXT NULL CONSTRAINT ck_conversion_requests_approved_comment_len CHECK (length(approved_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN cancelled_comment TEXT NULL CONSTRAINT ck_conversion_requests_cancelled_comment_len CHECK (length(cancelled_comment) BETWEEN 1 AND 1000);",
                "ALTER TABLE conversion_requests ADD COLUMN rejected_comment TEXT NULL CONSTRAINT ck_conversion_requests_rejected_comment_len CHECK (length(rejected_comment) BETWEEN 1 AND 1000);",
            ],
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "ALTER TABLE conversion_requests DROP COLUMN IF EXISTS rejected_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN IF EXISTS cancelled_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN IF EXISTS approved_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN IF EXISTS requested_comment;",
            ],
            // SQLite supports `DROP COLUMN` since 3.35 (2021-03); every
            // supported AM SQLite target satisfies that. Mirrors the
            // m0002 down-migration convention.
            sea_orm::DatabaseBackend::Sqlite => vec![
                "ALTER TABLE conversion_requests DROP COLUMN rejected_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN cancelled_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN approved_comment;",
                "ALTER TABLE conversion_requests DROP COLUMN requested_comment;",
            ],
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }
}
