//! Plan revision schema (D-394).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// One draft-or-pending and one published revision per plan, each a partial unique index of its
// own statement. Grants and the sold-as bundle SKU are deferred by the owner (D-411).
const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss.pricing_plan_revision (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, plan_id uuid NOT NULL REFERENCES bss.pricing_plan(id),
  rev_no integer NOT NULL, book_id uuid NOT NULL REFERENCES bss.pricing_price_book(id), state text NOT NULL,
  available_from date, pending_unit_id uuid REFERENCES bss.pricing_approval_unit(id),
  approved_by_unit_id uuid REFERENCES bss.pricing_approval_unit(id), published_at timestamptz,
  version bigint NOT NULL DEFAULT 1, created_by uuid NOT NULL,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  CONSTRAINT pricing_plan_revision_no UNIQUE (plan_id, rev_no),
  CONSTRAINT chk_pricing_plan_revision_state CHECK (state IN ('draft','pending','published','superseded'))
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_open
  ON bss.pricing_plan_revision (plan_id) WHERE state IN ('draft','pending')",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_published
  ON bss.pricing_plan_revision (plan_id) WHERE state = 'published'",
];
const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS pricing_plan_revision (
  id text PRIMARY KEY, tenant_id text NOT NULL, plan_id text NOT NULL REFERENCES pricing_plan(id),
  rev_no integer NOT NULL, book_id text NOT NULL REFERENCES pricing_price_book(id), state text NOT NULL,
  available_from text, pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), published_at text,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL,
  CONSTRAINT pricing_plan_revision_no UNIQUE (plan_id, rev_no),
  CONSTRAINT chk_pricing_plan_revision_state CHECK (state IN ('draft','pending','published','superseded'))
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_open
  ON pricing_plan_revision (plan_id) WHERE state IN ('draft','pending')",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_published
  ON pricing_plan_revision (plan_id) WHERE state = 'published'",
];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_plan_revision"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_plan_revision"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
