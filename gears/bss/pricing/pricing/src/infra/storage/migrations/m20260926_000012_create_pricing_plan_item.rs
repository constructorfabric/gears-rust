//! Plan item schema (D-407, D-413).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// `included_qty` is canonical decimal TEXT on both dialects, for the reason `min_fee` is (sea-orm
// decodes a SQLite `Decimal` through `f64`). `reservation_id` stays null until a reserve answers:
// a copied item is written `unreserved` and attaches after the write (D-413).
const PG_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS bss.pricing_plan_item (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, revision_id uuid NOT NULL REFERENCES bss.pricing_plan_revision(id),
  sku_id uuid NOT NULL, price_book_entry_id uuid REFERENCES bss.pricing_price_book_entry(id),
  treatment text NOT NULL, included_qty text, qty_min integer, reservation_id uuid, reference_state text NOT NULL,
  version bigint NOT NULL DEFAULT 1, created_by uuid NOT NULL,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  CONSTRAINT pricing_plan_item_sku UNIQUE (revision_id, sku_id),
  CONSTRAINT chk_pricing_plan_item_treatment CHECK (treatment IN ('paid','optional','included')),
  CONSTRAINT chk_pricing_plan_item_entry CHECK (treatment = 'included' OR price_book_entry_id IS NOT NULL),
  CONSTRAINT chk_pricing_plan_item_included_qty CHECK (included_qty ~ '^[0-9]+(\.[0-9]+)?$'),
  CONSTRAINT chk_pricing_plan_item_qty_min CHECK (qty_min >= 0),
  CONSTRAINT chk_pricing_plan_item_reference_state CHECK (reference_state IN ('unreserved','confirmation_pending','confirmed','lost'))
)"];
const SQLITE_UP: &[&str] = &[r"CREATE TABLE IF NOT EXISTS pricing_plan_item (
  id text PRIMARY KEY, tenant_id text NOT NULL, revision_id text NOT NULL REFERENCES pricing_plan_revision(id),
  sku_id text NOT NULL, price_book_entry_id text REFERENCES pricing_price_book_entry(id),
  treatment text NOT NULL, included_qty text, qty_min integer, reservation_id text, reference_state text NOT NULL,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL,
  CONSTRAINT pricing_plan_item_sku UNIQUE (revision_id, sku_id),
  CONSTRAINT chk_pricing_plan_item_treatment CHECK (treatment IN ('paid','optional','included')),
  CONSTRAINT chk_pricing_plan_item_entry CHECK (treatment = 'included' OR price_book_entry_id IS NOT NULL),
  CONSTRAINT chk_pricing_plan_item_included_qty CHECK (included_qty GLOB '[0-9]*' AND included_qty NOT GLOB '*[^0-9.]*' AND included_qty NOT GLOB '*.*.*' AND included_qty NOT GLOB '*.'),
  CONSTRAINT chk_pricing_plan_item_qty_min CHECK (qty_min >= 0),
  CONSTRAINT chk_pricing_plan_item_reference_state CHECK (reference_state IN ('unreserved','confirmation_pending','confirmed','lost'))
)"];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_plan_item"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_plan_item"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
