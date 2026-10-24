//! Drop the property graph `kb` that `m0002` created on `PostgreSQL` 19 beta
//! 1–3; `pg_dump` writes it as `CREATE PROPERTY GRAPH`, which released servers
//! refuse.
//!
//! Its existence is read from `pg_class` (kind `g`, schema matched by name, not
//! cast with `::regnamespace`, which case-folds), because `DROP PROPERTY GRAPH`
//! is a syntax error on a server without SQL/PGQ and would abort the migration
//! transaction. `down` does not recreate it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        let backend = manager.get_database_backend();
        let present = connection
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT 1 AS present FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relname = 'kb' AND c.relkind = 'g' \
                   AND n.nspname = current_schema()",
            ))
            .await?
            .is_some();
        if present {
            connection
                .execute_unprepared("DROP PROPERTY GRAPH kb")
                .await?;
            tracing::info!("dropped the retired SQL/PGQ property graph `kb`");
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
