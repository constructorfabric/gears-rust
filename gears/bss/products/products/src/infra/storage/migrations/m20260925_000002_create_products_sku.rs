//! SKU heads and durable version snapshots.
//! @cpt-dod:cpt-cf-bss-products-dod-sku-version-table:p1
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
const PG_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_sku (\n  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, code text NOT NULL, name text NOT NULL,\n  type text NOT NULL CHECK (type IN ('recurring','usage','one_time','bundle')),\n  category_id uuid NOT NULL REFERENCES bss.products_category(id),\n  description text NOT NULL DEFAULT '', sellable boolean NOT NULL DEFAULT true,\n  lifecycle text NOT NULL CHECK (lifecycle IN ('draft','published','deprecated','retiring','retired')),\n  fence_prior_lifecycle text NULL CHECK (fence_prior_lifecycle IN ('published','deprecated')),\n  fenced_at timestamptz NULL, fence_op_id uuid NULL,        \n  revision bigint NOT NULL DEFAULT 1, published_version bigint NOT NULL DEFAULT 0,\n  gl_code text NULL, tax_category text NULL, invoice_line_template text NULL,\n  billing_timing text NULL CHECK (billing_timing IN ('advance','arrears')),\n  usage_type_ref text NULL, unit text NULL,\n  type_change_pending boolean NOT NULL DEFAULT false,\n  pending_unit_id uuid NULL, approved_by_unit_id uuid NULL,\n  created_by uuid NOT NULL, created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_code ON bss.products_sku USING btree (tenant_id, code)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_name ON bss.products_sku USING btree (tenant_id, name)",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_list ON bss.products_sku USING btree (tenant_id, lifecycle, type, category_id, code)",
    "CREATE TABLE IF NOT EXISTS bss.products_sku_version (\n  sku_id uuid NOT NULL REFERENCES bss.products_sku(id), tenant_id uuid NOT NULL, published_version bigint NOT NULL,\n  effective_from date NOT NULL, content jsonb NOT NULL, created_at timestamptz NOT NULL,\n  PRIMARY KEY (sku_id, published_version))",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_version_as_of ON bss.products_sku_version USING btree (sku_id, effective_from, published_version)",
    "CREATE OR REPLACE FUNCTION bss.products_sku_version_append_only() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_sku_version is append-only'; END; $$",
    "DROP TRIGGER IF EXISTS products_sku_version_append_only ON bss.products_sku_version",
    "CREATE TRIGGER products_sku_version_append_only BEFORE UPDATE OR DELETE ON bss.products_sku_version FOR EACH ROW EXECUTE FUNCTION bss.products_sku_version_append_only()",
];
const SQLITE_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_sku (\n  id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL,\n  type text NOT NULL CHECK (type IN ('recurring','usage','one_time','bundle')),\n  category_id text NOT NULL REFERENCES products_category(id),\n  description text NOT NULL DEFAULT '', sellable integer NOT NULL DEFAULT true,\n  lifecycle text NOT NULL CHECK (lifecycle IN ('draft','published','deprecated','retiring','retired')),\n  fence_prior_lifecycle text NULL CHECK (fence_prior_lifecycle IN ('published','deprecated')),\n  fenced_at text NULL, fence_op_id text NULL,        \n  revision integer NOT NULL DEFAULT 1, published_version integer NOT NULL DEFAULT 0,\n  gl_code text NULL, tax_category text NULL, invoice_line_template text NULL,\n  billing_timing text NULL CHECK (billing_timing IN ('advance','arrears')),\n  usage_type_ref text NULL, unit text NULL,\n  type_change_pending integer NOT NULL DEFAULT false,\n  pending_unit_id text NULL, approved_by_unit_id text NULL,\n  created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_code ON products_sku (tenant_id, code)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_name ON products_sku (tenant_id, name)",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_list ON products_sku (tenant_id, lifecycle, type, category_id, code)",
    "CREATE TABLE IF NOT EXISTS products_sku_version (\n  sku_id text NOT NULL REFERENCES products_sku(id), tenant_id text NOT NULL, published_version integer NOT NULL,\n  effective_from text NOT NULL, content text NOT NULL, created_at text NOT NULL,\n  PRIMARY KEY (sku_id, published_version))",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_version_as_of ON products_sku_version (sku_id, effective_from, published_version)",
    "DROP TRIGGER IF EXISTS products_sku_version_no_update",
    "CREATE TRIGGER products_sku_version_no_update BEFORE UPDATE ON products_sku_version BEGIN SELECT RAISE(ABORT, 'products_sku_version is append-only'); END",
    "DROP TRIGGER IF EXISTS products_sku_version_no_delete",
    "CREATE TRIGGER products_sku_version_no_delete BEFORE DELETE ON products_sku_version BEGIN SELECT RAISE(ABORT, 'products_sku_version is append-only'); END",
];
const PG_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss.products_sku_version",
    "DROP FUNCTION IF EXISTS bss.products_sku_version_append_only()",
    "DROP TABLE IF EXISTS bss.products_sku",
];
const SQLITE_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS products_sku_version",
    "DROP TABLE IF EXISTS products_sku",
];
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

#[cfg(test)]
#[path = "m20260925_000002_create_products_sku_tests.rs"]
mod tests;
