//! Close the policy upsert race with two partial unique indexes.
//!
//! Two partial indexes are needed because `NULL`s are distinct for uniqueness:
//! `policies_user_scope_unique_idx` covers `(tenant_id, scope, scope_owner_id)` when
//! `scope_owner_id IS NOT NULL`, `policies_tenant_scope_unique_idx` covers
//! `(tenant_id, scope)` when it is `NULL`. They back up the transaction in
//! `Store::upsert_policy`: a concurrent first-time upsert loses with a constraint
//! violation instead of duplicating the row.
//!
//! Existing duplicates are deleted first (keeping the greatest `updated_at`, then
//! `policy_id`) since index creation would otherwise fail; the `SQLite` variant uses
//! a correlated `EXISTS` instead of a window function. Re-running is a no-op.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
DELETE FROM policies p
    USING policies newer
    WHERE p.scope_owner_id IS NOT NULL
      AND newer.tenant_id = p.tenant_id
      AND newer.scope = p.scope
      AND newer.scope_owner_id = p.scope_owner_id
      AND (newer.updated_at, newer.policy_id) > (p.updated_at, p.policy_id);

DELETE FROM policies p
    USING policies newer
    WHERE p.scope_owner_id IS NULL
      AND newer.tenant_id = p.tenant_id
      AND newer.scope = p.scope
      AND newer.scope_owner_id IS NULL
      AND (newer.updated_at, newer.policy_id) > (p.updated_at, p.policy_id);

CREATE UNIQUE INDEX IF NOT EXISTS policies_user_scope_unique_idx
    ON policies (tenant_id, scope, scope_owner_id) WHERE scope_owner_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS policies_tenant_scope_unique_idx
    ON policies (tenant_id, scope) WHERE scope_owner_id IS NULL;
";

const SQLITE_UP: &str = r"
DELETE FROM policies
    WHERE scope_owner_id IS NOT NULL
      AND EXISTS (
          SELECT 1 FROM policies newer
          WHERE newer.tenant_id = policies.tenant_id
            AND newer.scope = policies.scope
            AND newer.scope_owner_id = policies.scope_owner_id
            AND (newer.updated_at > policies.updated_at
                 OR (newer.updated_at = policies.updated_at
                     AND newer.policy_id > policies.policy_id))
      );

DELETE FROM policies
    WHERE scope_owner_id IS NULL
      AND EXISTS (
          SELECT 1 FROM policies newer
          WHERE newer.tenant_id = policies.tenant_id
            AND newer.scope = policies.scope
            AND newer.scope_owner_id IS NULL
            AND (newer.updated_at > policies.updated_at
                 OR (newer.updated_at = policies.updated_at
                     AND newer.policy_id > policies.policy_id))
      );

CREATE UNIQUE INDEX IF NOT EXISTS policies_user_scope_unique_idx
    ON policies (tenant_id, scope, scope_owner_id) WHERE scope_owner_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS policies_tenant_scope_unique_idx
    ON policies (tenant_id, scope) WHERE scope_owner_id IS NULL;
";

const DOWN: &str = r"
DROP INDEX IF EXISTS policies_user_scope_unique_idx;
DROP INDEX IF EXISTS policies_tenant_scope_unique_idx;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
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
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
