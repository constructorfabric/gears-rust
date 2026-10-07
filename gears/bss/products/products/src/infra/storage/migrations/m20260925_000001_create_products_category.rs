//! Category schema and tenant uniqueness.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_category (id uuid PRIMARY KEY, tenant_id uuid NOT NULL, code text NOT NULL, name text NOT NULL, is_default boolean NOT NULL DEFAULT false, sort_order integer NOT NULL DEFAULT 0, status text NOT NULL CHECK (status IN ('active','retired')), version bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_category_code ON bss.products_category USING btree (tenant_id, code)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_category_default ON bss.products_category USING btree (tenant_id) WHERE is_default",
];
const SQLITE_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_category (id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL, is_default integer NOT NULL DEFAULT 0, sort_order integer NOT NULL DEFAULT 0, status text NOT NULL CHECK (status IN ('active','retired')), version integer NOT NULL DEFAULT 1, created_at text NOT NULL, updated_at text NOT NULL)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_category_code ON products_category (tenant_id, code)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_category_default ON products_category (tenant_id) WHERE is_default",
];
const PG_DOWN: &[&str] = &["DROP TABLE IF EXISTS bss.products_category"];
const SQLITE_DOWN: &[&str] = &["DROP TABLE IF EXISTS products_category"];

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
#[path = "m20260925_000001_create_products_category_tests.rs"]
mod tests;
