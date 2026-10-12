//! Plan schema (D-394).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// `published_rev` is the projection a revision's apply writes. Retirement (D-410) and the sold-as
// bundle (D-411) are deferred by the owner, so the plan carries neither a state nor a bundle SKU.
const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss.pricing_plan (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, code text NOT NULL, name text NOT NULL, published_rev integer,
  version bigint NOT NULL DEFAULT 1, created_by uuid NOT NULL,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_code ON bss.pricing_plan (tenant_id, code)",
];
const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS pricing_plan (
  id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL, published_rev integer,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_code ON pricing_plan (tenant_id, code)",
];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_plan"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_plan"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
