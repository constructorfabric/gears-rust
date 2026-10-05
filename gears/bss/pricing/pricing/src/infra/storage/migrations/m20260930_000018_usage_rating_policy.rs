//! D-502: immutable entry policies. Legacy entries keep all three reference columns null.
//! `SQLite` rebuilds the entry and its two children inside the runner transaction, as migration
//! 17 does, without disabling foreign keys. Prices retain their self references and indexes;
//! items retain their legacy columns and constraints. No commercial history is inferred.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
const PG_UP: &[&str] = &[
    r"CREATE TABLE bss.pricing_usage_rating_policy (
  tenant_id uuid NOT NULL, policy_id uuid NOT NULL, version bigint NOT NULL CHECK (version > 0),
  digest text NOT NULL CHECK (digest ~ '^[0-9a-f]{64}$'), content jsonb NOT NULL,
  created_at timestamptz NOT NULL, created_by uuid NOT NULL,
  PRIMARY KEY (tenant_id, policy_id, version),
  UNIQUE (tenant_id, digest), UNIQUE (tenant_id, policy_id, version, digest)
)",
    r"ALTER TABLE bss.pricing_price_book_entry
  ADD COLUMN usage_policy_id uuid,
  ADD COLUMN usage_policy_version bigint,
  ADD COLUMN usage_policy_digest text,
  ADD CONSTRAINT pricing_entry_policy_complete CHECK (
    (usage_policy_id IS NULL AND usage_policy_version IS NULL AND usage_policy_digest IS NULL) OR
    (usage_policy_id IS NOT NULL AND usage_policy_version IS NOT NULL AND usage_policy_digest IS NOT NULL AND charge_kind = 'usage')),
  ADD CONSTRAINT pricing_entry_policy_fk FOREIGN KEY (tenant_id, usage_policy_id, usage_policy_version, usage_policy_digest)
    REFERENCES bss.pricing_usage_rating_policy (tenant_id, policy_id, version, digest)",
    r"DROP INDEX bss.pricing_price_book_entry_key",
    r"CREATE UNIQUE INDEX pricing_price_book_entry_key ON bss.pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model, coalesce(usage_policy_digest, ''))",
    r"CREATE FUNCTION bss.pricing_usage_policy_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'usage rating policies are append-only'; END; $$",
    r"CREATE TRIGGER pricing_usage_policy_immutable BEFORE UPDATE OR DELETE ON bss.pricing_usage_rating_policy
  FOR EACH ROW EXECUTE FUNCTION bss.pricing_usage_policy_immutable()",
];
const SQLITE_UP: &[&str] = &[
    r"CREATE TABLE pricing_usage_rating_policy (
  tenant_id text NOT NULL, policy_id text NOT NULL, version integer NOT NULL CHECK (version > 0),
  digest text NOT NULL CHECK (length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'), content text NOT NULL,
  created_at text NOT NULL, created_by text NOT NULL,
  PRIMARY KEY (tenant_id, policy_id, version),
  UNIQUE (tenant_id, digest), UNIQUE (tenant_id, policy_id, version, digest)
)",
    r"CREATE TABLE pricing_price_book_entry__d502 (
  id text PRIMARY KEY, tenant_id text NOT NULL, book_id text NOT NULL REFERENCES pricing_price_book(id),
  sku_id text NOT NULL, charge_kind text NOT NULL CHECK (charge_kind IN ('recurring','usage','one_time')),
  period text, dimension_key text, invoice_line_override text,
  reservation_id text NOT NULL,
  reference_state text NOT NULL CHECK (reference_state IN ('confirmation_pending','confirmed','lost')),
  version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  model text NOT NULL DEFAULT 'flat' CHECK (model IN ('flat','per_unit','graduated','volume','package')),
  usage_policy_id text, usage_policy_version integer, usage_policy_digest text,
  FOREIGN KEY (tenant_id, dimension_key) REFERENCES pricing_dimension_key(tenant_id, key),
  CHECK ((charge_kind = 'recurring' AND period IS NOT NULL AND period IN ('month','year'))
    OR (charge_kind IN ('usage','one_time') AND period IS NULL))
,
  CONSTRAINT pricing_entry_policy_complete CHECK (
    (usage_policy_id IS NULL AND usage_policy_version IS NULL AND usage_policy_digest IS NULL) OR
    (usage_policy_id IS NOT NULL AND usage_policy_version IS NOT NULL AND usage_policy_digest IS NOT NULL AND charge_kind = 'usage')),
  FOREIGN KEY (tenant_id, usage_policy_id, usage_policy_version, usage_policy_digest)
    REFERENCES pricing_usage_rating_policy (tenant_id, policy_id, version, digest)
)",
    r"INSERT INTO pricing_price_book_entry__d502 (id, tenant_id, book_id, sku_id, charge_kind, period, dimension_key, invoice_line_override, reservation_id, reference_state, version, created_at, updated_at, model) SELECT id, tenant_id, book_id, sku_id, charge_kind, period, dimension_key, invoice_line_override, reservation_id, reference_state, version, created_at, updated_at, model FROM pricing_price_book_entry",
    r"CREATE TABLE pricing_price__d502 (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_book_entry_id text NOT NULL REFERENCES pricing_price_book_entry__d502(id),
  version_no integer NOT NULL, dim_value text,
  price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_price_id text REFERENCES pricing_price__d502(id),
  return_of_price_id text REFERENCES pricing_price__d502(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)",
    r"INSERT INTO pricing_price__d502 SELECT * FROM pricing_price",
    r"CREATE TABLE pricing_plan_item__d502 (
  id text PRIMARY KEY, tenant_id text NOT NULL, revision_id text NOT NULL REFERENCES pricing_plan_revision(id),
  sku_id text NOT NULL, price_book_entry_id text REFERENCES pricing_price_book_entry__d502(id),
  treatment text NOT NULL, included_qty text, qty_min integer, reservation_id text, reference_state text NOT NULL,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL,
  CONSTRAINT pricing_plan_item_sku UNIQUE (revision_id, sku_id),
  CONSTRAINT chk_pricing_plan_item_treatment CHECK (treatment IN ('paid','optional','included')),
  CONSTRAINT chk_pricing_plan_item_entry CHECK (treatment = 'included' OR price_book_entry_id IS NOT NULL),
  CONSTRAINT chk_pricing_plan_item_included_qty CHECK (included_qty GLOB '[0-9]*' AND included_qty NOT GLOB '*[^0-9.]*' AND included_qty NOT GLOB '*.*.*' AND included_qty NOT GLOB '*.'),
  CONSTRAINT chk_pricing_plan_item_qty_min CHECK (qty_min >= 0),
  CONSTRAINT chk_pricing_plan_item_reference_state CHECK (reference_state IN ('unreserved','confirmation_pending','confirmed','lost'))
)",
    r"INSERT INTO pricing_plan_item__d502 SELECT * FROM pricing_plan_item",
    r"DROP TABLE pricing_plan_item",
    r"DROP TABLE pricing_price",
    r"DROP TABLE pricing_price_book_entry",
    r"ALTER TABLE pricing_price_book_entry__d502 RENAME TO pricing_price_book_entry",
    r"ALTER TABLE pricing_price__d502 RENAME TO pricing_price",
    r"ALTER TABLE pricing_plan_item__d502 RENAME TO pricing_plan_item",
    r"CREATE UNIQUE INDEX pricing_price_book_entry_key ON pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model, coalesce(usage_policy_digest, ''))",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_price_approved_start
  ON pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'",
    r"CREATE INDEX IF NOT EXISTS pricing_price_chain
  ON pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'",
    r"CREATE TRIGGER pricing_usage_policy_no_update BEFORE UPDATE ON pricing_usage_rating_policy BEGIN SELECT RAISE(ABORT, 'usage rating policies are append-only'); END",
    r"CREATE TRIGGER pricing_usage_policy_no_delete BEFORE DELETE ON pricing_usage_rating_policy BEGIN SELECT RAISE(ABORT, 'usage rating policies are append-only'); END",
];
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible — immutable entry policies retain commercial history (D-502)",
            self.name()
        )))
    }
}
