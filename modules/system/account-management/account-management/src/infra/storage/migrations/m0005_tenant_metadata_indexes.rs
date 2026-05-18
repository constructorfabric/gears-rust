//! Migration `m0005` — add `idx_tenant_metadata_schema ON
//! tenant_metadata(schema_uuid)` for the FEATURE 2.7 (Tenant Metadata)
//! walk-up resolver.
//!
//! The `tenant_metadata` table itself was created by `m0001_initial_schema`
//! (composite PK `(tenant_id, schema_uuid)`, FK `ON DELETE CASCADE` on
//! Postgres, plus the lookup index `idx_tenant_metadata_tenant`). This
//! migration adds the secondary index on `schema_uuid` only — the
//! resolver and the future per-schema cross-tenant scans benefit from a
//! direct probe on the schema column without forcing a tenant-prefix
//! seek.
//!
//! Both supported backends use `CREATE INDEX IF NOT EXISTS` so the
//! migration is idempotent if the migrator re-runs against a database
//! that already holds the index. `MySQL` is unsupported and returns
//! `DbErr::Custom(MYSQL_NOT_SUPPORTED)` — same contract as `m0001` and
//! `m0004`. `down` drops only the index; the table itself is owned by
//! `m0001` and MUST NOT be dropped here.
//!
//! # `CREATE INDEX` vs `CREATE INDEX CONCURRENTLY`
//!
//! On `PostgreSQL` a plain `CREATE INDEX` takes a `SHARE` lock that
//! blocks writes to `tenant_metadata` for the duration of the index
//! build. `CONCURRENTLY` avoids the write block at the cost of two
//! passes, and — critically — `CREATE INDEX CONCURRENTLY` CANNOT run
//! inside a transaction block. `sea-orm-migration` 1.1 wraps every
//! Postgres migration in a single transaction
//! (`exec_with_connection` in `sea-orm-migration::migrator`), so
//! emitting `CONCURRENTLY` here would fail with `25001` (active SQL
//! transaction) before any DDL ran.
//!
//! The lock impact is acceptable in practice for this specific
//! migration: FEATURE 2.7 (Tenant Metadata) is the only producer of
//! `tenant_metadata` rows, and the service that writes them is
//! introduced in the same release as this index. Environments running
//! `m0005` therefore see an empty (or near-empty) `tenant_metadata`
//! table and the index build is effectively instantaneous. Operators
//! retro-applying the index to a long-populated table (after a future
//! release widens producers) should run the index build out of band
//! with `CREATE INDEX CONCURRENTLY` via `psql` and then mark `m0005`
//! as applied; the migration's `IF NOT EXISTS` makes that flow a
//! no-op when the migrator next runs.

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
            // @cpt-begin:cpt-cf-account-management-dbtable-tenant-metadata:p2:inst-dbtable-tenant-metadata-index-schema
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => vec![
                "CREATE INDEX IF NOT EXISTS idx_tenant_metadata_schema ON tenant_metadata (schema_uuid);",
            ],
            // @cpt-end:cpt-cf-account-management-dbtable-tenant-metadata:p2:inst-dbtable-tenant-metadata-index-schema
            sea_orm::DatabaseBackend::MySql => {
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
        if matches!(backend, sea_orm::DatabaseBackend::MySql) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_tenant_metadata_schema;")
            .await?;
        Ok(())
    }
}
