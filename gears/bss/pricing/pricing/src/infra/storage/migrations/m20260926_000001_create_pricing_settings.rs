//! Settings schema.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS bss.pricing_settings (
  tenant_id uuid PRIMARY KEY, default_timing text NOT NULL CHECK (default_timing IN ('advance','arrears')),
  default_rounding text NOT NULL, default_gl text, default_tax_category text,
  invoice_line_templates jsonb NOT NULL, version bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL
)"];
const SQLITE_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS pricing_settings (
  tenant_id text PRIMARY KEY, default_timing text NOT NULL CHECK (default_timing IN ('advance','arrears')),
  default_rounding text NOT NULL, default_gl text, default_tax_category text,
  invoice_line_templates text NOT NULL, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL
)"];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_settings"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_settings"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
