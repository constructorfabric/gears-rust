//! Price schema.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS bss.pricing_price (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, price_book_entry_id uuid NOT NULL REFERENCES bss.pricing_price_book_entry(id),
  version_no integer NOT NULL, dim_value text,
  model text NOT NULL CHECK (model IN ('flat','per_unit','graduated','volume','package')), price_json jsonb NOT NULL,
  min_fee text CHECK (min_fee ~ '^[0-9]+(\.[0-9]+)?$'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from date NOT NULL, effective_to date, keep_for_bound boolean NOT NULL DEFAULT false,
  closed_explicitly boolean NOT NULL DEFAULT false,
  temporary_until date, paired_price_id uuid REFERENCES bss.pricing_price(id),
  return_of_price_id uuid REFERENCES bss.pricing_price(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id uuid REFERENCES bss.pricing_approval_unit(id),
  approved_by_unit_id uuid REFERENCES bss.pricing_approval_unit(id), note text, created_by uuid NOT NULL,
  approved_at timestamptz, version bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_price_approved_start
  ON bss.pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'",
    r"CREATE INDEX IF NOT EXISTS pricing_price_chain
  ON bss.pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'",
];
// `min_fee` is canonical decimal TEXT on both dialects: sea-orm decodes a SQLite `Decimal`
// through `f64` whatever the column affinity ("30.00" read back as "30"; digits past f64's
// precision lost), and a fee is money. The CHECK admits unsigned plain decimals only.
const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE IF NOT EXISTS pricing_price (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_book_entry_id text NOT NULL REFERENCES pricing_price_book_entry(id),
  version_no integer NOT NULL, dim_value text,
  model text NOT NULL CHECK (model IN ('flat','per_unit','graduated','volume','package')), price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_price_id text REFERENCES pricing_price(id),
  return_of_price_id text REFERENCES pricing_price(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_price_approved_start
  ON pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'",
    r"CREATE INDEX IF NOT EXISTS pricing_price_chain
  ON pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'",
];
const PG_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS bss.pricing_price"];
const SQLITE_DOWN: &[&str] = &[r"DROP TABLE IF EXISTS pricing_price"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}
