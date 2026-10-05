//! Price book schema.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS bss.pricing_price_book (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, code text NOT NULL, name text NOT NULL,
  currency char(3) NOT NULL, valid_from date, valid_until date, version bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  UNIQUE (tenant_id, code), UNIQUE (tenant_id, id),
  CHECK (valid_from IS NULL OR valid_until IS NULL OR valid_from < valid_until)
)"];
const SQLITE_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS pricing_price_book (
  id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL,
  currency char(3) NOT NULL, valid_from text, valid_until text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (tenant_id, code), UNIQUE (tenant_id, id),
  CHECK (valid_from IS NULL OR valid_until IS NULL OR valid_from < valid_until)
)"];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_price_book"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_price_book"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
