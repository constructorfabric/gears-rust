//! Local reference reservations and retained release history.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
const PG_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_sku_reference (\n  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, sku_id uuid NOT NULL REFERENCES bss.products_sku(id),\n  owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN ('price_book_entry','plan_item','sold_as')), ref_id uuid NOT NULL,\n  state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  reserved_by uuid NOT NULL, reserved_at timestamptz NOT NULL, confirmed_at timestamptz NULL, released_at timestamptz NULL, released_by uuid NULL, release_reason text NULL, forced boolean NOT NULL DEFAULT false)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_reference_live ON bss.products_sku_reference USING btree (tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_reference_live ON bss.products_sku_reference USING btree (sku_id, state) WHERE state <> 'released'",
];
const SQLITE_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_sku_reference (\n  id text PRIMARY KEY, tenant_id text NOT NULL, sku_id text NOT NULL REFERENCES products_sku(id),\n  owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN ('price_book_entry','plan_item','sold_as')), ref_id text NOT NULL,\n  state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  reserved_by text NOT NULL, reserved_at text NOT NULL, confirmed_at text NULL, released_at text NULL, released_by text NULL, release_reason text NULL, forced integer NOT NULL DEFAULT false)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_sku_reference_live ON products_sku_reference (tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_reference_live ON products_sku_reference (sku_id, state) WHERE state <> 'released'",
];
const PG_DOWN: &[&str] = &["DROP TABLE IF EXISTS bss.products_sku_reference"];
const SQLITE_DOWN: &[&str] = &["DROP TABLE IF EXISTS products_sku_reference"];
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
#[path = "m20260925_000006_create_products_sku_reference_tests.rs"]
mod tests;
