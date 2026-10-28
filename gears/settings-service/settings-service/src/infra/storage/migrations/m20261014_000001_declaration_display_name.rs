// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-entity-schema:p1
//! The optional `display_name` column on `setting_declarations`.
//!
//! Additive: one nullable column, no default and no `NOT NULL`, so every
//! statement the previous release issued still applies and a rolling upgrade
//! can run old and new instances against the same schema.
//!
//! No backfill. A declaration written before the column existed has no label,
//! and the surface renders that as an omitted field. Deriving one from
//! `leaf_slug` here would store a guess as though an author had written it.
//!
//! The trigram index is PostgreSQL-only, for the reason the table migration
//! gives for its key and description indexes: `pg_trgm` is an extension, and
//! `SQLite` has no equivalent. The search predicate is `LIKE` on both backends;
//! on `SQLite` it scans, which is acceptable for the test backend.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum SettingDeclarations {
    Table,
    DisplayName,
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SettingDeclarations::Table)
                    .add_column(
                        ColumnDef::new(SettingDeclarations::DisplayName)
                            .string_len(256)
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        if manager.get_database_backend() == DatabaseBackend::Postgres {
            manager
                .get_connection()
                .execute_unprepared(
                    "CREATE INDEX IF NOT EXISTS idx_declarations_display_name_trgm
                         ON setting_declarations USING gin (display_name gin_trgm_ops);",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            manager
                .get_connection()
                .execute_unprepared("DROP INDEX IF EXISTS idx_declarations_display_name_trgm;")
                .await?;
        }
        manager
            .alter_table(
                Table::alter()
                    .table(SettingDeclarations::Table)
                    .drop_column(SettingDeclarations::DisplayName)
                    .to_owned(),
            )
            .await
    }
}
