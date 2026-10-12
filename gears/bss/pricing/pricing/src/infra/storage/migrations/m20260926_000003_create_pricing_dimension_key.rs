//! Dimension key schema.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[r#"CREATE TABLE IF NOT EXISTS bss.pricing_dimension_key (
  tenant_id uuid NOT NULL, key text NOT NULL, "values" jsonb NOT NULL,
  version bigint NOT NULL DEFAULT 1, PRIMARY KEY (tenant_id, key)
)"#];
const SQLITE_UP: &[&str] = &[r#"CREATE TABLE IF NOT EXISTS pricing_dimension_key (
  tenant_id text NOT NULL, key text NOT NULL, "values" text NOT NULL,
  version integer NOT NULL DEFAULT 1, PRIMARY KEY (tenant_id, key)
)"#];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_dimension_key"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_dimension_key"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
