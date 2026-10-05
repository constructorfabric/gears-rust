//! P-D-263: a retired SKU or a retired category can be archived. The mark is two columns,
//! `archived_at` and `archived_by`, on `products_sku` and `products_category`; nothing that reads
//! the lifecycle or the status changes.
//!
//! A forward migration: the shipped chain is frozen. Both engines add the four nullable columns in
//! place. A mark is whole or absent: both columns null, or both set. Postgres pairs them with a
//! CHECK `(archived_at IS NULL) = (archived_by IS NULL)` on each table; `SQLite` cannot add a
//! CHECK to an existing table, so two triggers per table (insert and update) refuse a half mark,
//! as `m20261002_000013_derived_sku_unit` does. The partial index `ix_products_sku_unarchived`,
//! `(tenant_id, code) WHERE archived_at IS NULL`, is the default SKU page's order over the rows it
//! shows: the page walks it and reads no archived row.
//!
//! # Down
//!
//! The index, the CHECKs or the triggers, and the four columns go. Every archive mark is lost with
//! them: an archived row is listed again, as it was before this migration.

use sea_orm_migration::prelude::*;

use super::exec_backend;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "ALTER TABLE bss.products_sku \
     ADD COLUMN IF NOT EXISTS archived_at timestamptz NULL, \
     ADD COLUMN IF NOT EXISTS archived_by uuid NULL",
    "ALTER TABLE bss.products_category \
     ADD COLUMN IF NOT EXISTS archived_at timestamptz NULL, \
     ADD COLUMN IF NOT EXISTS archived_by uuid NULL",
    "ALTER TABLE bss.products_sku DROP CONSTRAINT IF EXISTS chk_products_sku_archive_mark",
    "ALTER TABLE bss.products_sku ADD CONSTRAINT chk_products_sku_archive_mark \
     CHECK ((archived_at IS NULL) = (archived_by IS NULL))",
    "ALTER TABLE bss.products_category DROP CONSTRAINT IF EXISTS chk_products_category_archive_mark",
    "ALTER TABLE bss.products_category ADD CONSTRAINT chk_products_category_archive_mark \
     CHECK ((archived_at IS NULL) = (archived_by IS NULL))",
    "CREATE INDEX IF NOT EXISTS ix_products_sku_unarchived \
     ON bss.products_sku (tenant_id, code) WHERE archived_at IS NULL",
];

const PG_DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS bss.ix_products_sku_unarchived",
    "ALTER TABLE bss.products_sku DROP CONSTRAINT IF EXISTS chk_products_sku_archive_mark",
    "ALTER TABLE bss.products_category DROP CONSTRAINT IF EXISTS chk_products_category_archive_mark",
    "ALTER TABLE bss.products_sku \
     DROP COLUMN IF EXISTS archived_at, DROP COLUMN IF EXISTS archived_by",
    "ALTER TABLE bss.products_category \
     DROP COLUMN IF EXISTS archived_at, DROP COLUMN IF EXISTS archived_by",
];

const SQLITE_UP: &[&str] = &[
    "ALTER TABLE products_sku ADD COLUMN archived_at text NULL",
    "ALTER TABLE products_sku ADD COLUMN archived_by text NULL",
    "ALTER TABLE products_category ADD COLUMN archived_at text NULL",
    "ALTER TABLE products_category ADD COLUMN archived_by text NULL",
    "DROP TRIGGER IF EXISTS products_sku_archive_mark_insert",
    "CREATE TRIGGER products_sku_archive_mark_insert BEFORE INSERT ON products_sku \
     WHEN (NEW.archived_at IS NULL) != (NEW.archived_by IS NULL) \
     BEGIN SELECT RAISE(ABORT, 'products_sku_archive_mark'); END",
    "DROP TRIGGER IF EXISTS products_sku_archive_mark_update",
    "CREATE TRIGGER products_sku_archive_mark_update BEFORE UPDATE ON products_sku \
     WHEN (NEW.archived_at IS NULL) != (NEW.archived_by IS NULL) \
     BEGIN SELECT RAISE(ABORT, 'products_sku_archive_mark'); END",
    "DROP TRIGGER IF EXISTS products_category_archive_mark_insert",
    "CREATE TRIGGER products_category_archive_mark_insert BEFORE INSERT ON products_category \
     WHEN (NEW.archived_at IS NULL) != (NEW.archived_by IS NULL) \
     BEGIN SELECT RAISE(ABORT, 'products_category_archive_mark'); END",
    "DROP TRIGGER IF EXISTS products_category_archive_mark_update",
    "CREATE TRIGGER products_category_archive_mark_update BEFORE UPDATE ON products_category \
     WHEN (NEW.archived_at IS NULL) != (NEW.archived_by IS NULL) \
     BEGIN SELECT RAISE(ABORT, 'products_category_archive_mark'); END",
    "CREATE INDEX ix_products_sku_unarchived ON products_sku (tenant_id, code) \
     WHERE archived_at IS NULL",
];

const SQLITE_DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS ix_products_sku_unarchived",
    "DROP TRIGGER IF EXISTS products_sku_archive_mark_insert",
    "DROP TRIGGER IF EXISTS products_sku_archive_mark_update",
    "DROP TRIGGER IF EXISTS products_category_archive_mark_insert",
    "DROP TRIGGER IF EXISTS products_category_archive_mark_update",
    "ALTER TABLE products_sku DROP COLUMN archived_at",
    "ALTER TABLE products_sku DROP COLUMN archived_by",
    "ALTER TABLE products_category DROP COLUMN archived_at",
    "ALTER TABLE products_category DROP COLUMN archived_by",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

#[cfg(test)]
#[path = "m20261003_000014_archive_mark_tests.rs"]
mod tests;
