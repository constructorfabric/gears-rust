//! What the schema-guard suites of both tiers share (D-423): the refusal they expect, and the
//! shapes today's chain replaced, copied from git history so a test can rebuild them.
//!
//! Each shape is the DDL the chain executed before the edit that replaced it, verbatim:
//! - `pricing_reference_op` before D-412 (`06860f1fb^`, `m20260926_000006`): no `ref_kind`;
//! - the pre-rename chain (`fbff5dbd6^`): `pricing_price` was the entry (`m20260926_000005`),
//!   `pricing_price_row` the dated amount (`m20260926_000007`), and the reference op named its
//!   reference `price_id` (`m20260926_000006`);
//! - the legacy chain's `pricing_plan` (`bss/products-backup`, `m20260821_000021`), a table name
//!   today's chain creates too: a revision row keyed `(plan_id, revision)`, no `code` column. Its
//!   `CREATE TABLE` only: the guard reads columns, so the indexes and triggers are left out.

#![allow(dead_code, reason = "each test binary uses part of the module")]

/// The guard's migration name; it sorts before every other migration of the gear.
pub const GUARD: &str = "m0000_pricing_refuse_a_legacy_or_stale_schema";

/// The migrations the pre-rename chain did not have under these names.
pub const RENAMED: [&str; 2] = [
    "m20260926_000005_create_pricing_price_book_entry",
    "m20260926_000007_create_pricing_price",
];
/// The migrations the pre-rename chain recorded instead.
pub const PRE_RENAME_NAMES: [&str; 2] = [
    "m20260926_000005_create_pricing_price",
    "m20260926_000007_create_pricing_price_row",
];
/// The phase 3 migrations, which came after the rename.
pub const PLAN_MIGRATIONS: [&str; 3] = [
    "m20260926_000010_create_pricing_plan",
    "m20260926_000011_create_pricing_plan_revision",
    "m20260926_000012_create_pricing_plan_item",
];

/// Every table the pre-PriceBook chain creates, counted independently of the guard's constant:
/// `git show bss/products-backup:gears/bss/pricing/pricing/src/infra/storage/migrations/*` has 101
/// `CREATE TABLE` statements (both dialects, every name a literal, no `RENAME TO`) naming these 47.
/// The legacy set the guard refuses is this list minus what a fresh chain creates (D-423, H1).
pub const LEGACY_CHAIN_TABLES: [&str; 47] = [
    "pricing_approval",
    "pricing_approval_key",
    "pricing_approval_threshold",
    "pricing_approval_threshold_tombstone",
    "pricing_audit_log",
    "pricing_brand_taxonomy",
    "pricing_bulk_operation",
    "pricing_bulk_row_lock",
    "pricing_bundle",
    "pricing_bundle_component",
    "pricing_bundle_revshare",
    "pricing_bundle_revshare_group",
    "pricing_catalog_version_ref",
    "pricing_charge_line",
    "pricing_charge_line_version",
    "pricing_composite_meter",
    "pricing_customer_group_taxonomy",
    "pricing_draft_window",
    "pricing_gl_code_taxonomy",
    "pricing_group_membership",
    "pricing_idempotency_dedup",
    "pricing_market_price",
    "pricing_migration",
    "pricing_operator_flag",
    "pricing_org_tier_taxonomy",
    "pricing_outbox",
    "pricing_partner_taxonomy",
    "pricing_pin_frontier",
    "pricing_plan",
    "pricing_plan_addon_rule",
    "pricing_plan_descriptor_set",
    "pricing_plan_period_floor_cap",
    "pricing_plan_phase",
    "pricing_policy_object",
    "pricing_price",
    "pricing_price_overlay",
    "pricing_price_overlay_line",
    "pricing_price_overlay_line_amount",
    "pricing_price_tier_band",
    "pricing_price_window",
    "pricing_read_model",
    "pricing_region_taxonomy",
    "pricing_repricing_journal",
    "pricing_rounding_policy_taxonomy",
    "pricing_snapshot_provenance",
    "pricing_window_baseline",
    "pricing_window_guard",
];

/// D-423's refusal, word for word.
#[must_use]
pub fn refusal(kind: &str, found: &str) -> String {
    format!(
        "bss-pricing: this database holds a {kind} bss-pricing schema ({found}); PriceBook does \
         not migrate it \u{2014} start from an empty data root / empty bss-pricing tables"
    )
}

pub const REFERENCE_OP_BEFORE_D412_SQLITE: &[&str] = &[
    r"CREATE TABLE pricing_reference_op (
  op_id text PRIMARY KEY, tenant_id text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('create_entry','delete_entry','rereserve_entry')),
  price_book_entry_id text NOT NULL, sku_id text NOT NULL, reservation_id text,
  idempotency_key text,
  state text NOT NULL CHECK (state IN ('reserving','written','cancelling','releasing','done')),
  outcome text, attempts integer NOT NULL DEFAULT 0, next_attempt_at text NOT NULL,
  last_error text, created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL
)",
    r"CREATE INDEX pricing_reference_op_due ON pricing_reference_op (state, next_attempt_at) WHERE state <> 'done'",
];
pub const REFERENCE_OP_BEFORE_D412_PG: &[&str] = &[
    r"CREATE TABLE bss.pricing_reference_op (
  op_id uuid PRIMARY KEY, tenant_id uuid NOT NULL,
  kind text NOT NULL CHECK (kind IN ('create_entry','delete_entry','rereserve_entry')),
  price_book_entry_id uuid NOT NULL, sku_id uuid NOT NULL, reservation_id uuid,
  idempotency_key text,
  state text NOT NULL CHECK (state IN ('reserving','written','cancelling','releasing','done')),
  outcome text, attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL,
  last_error text, created_by uuid NOT NULL, created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL
)",
    r"CREATE INDEX pricing_reference_op_due ON bss.pricing_reference_op (state, next_attempt_at) WHERE state <> 'done'",
];

pub const REFERENCE_OP_BEFORE_RENAME_SQLITE: &[&str] = &[
    r"CREATE TABLE pricing_reference_op (
  op_id text PRIMARY KEY, tenant_id text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('create_price','delete_price','rereserve_price')),
  price_id text NOT NULL, sku_id text NOT NULL, reservation_id text,
  idempotency_key text,
  state text NOT NULL CHECK (state IN ('reserving','written','cancelling','releasing','done')),
  outcome text, attempts integer NOT NULL DEFAULT 0, next_attempt_at text NOT NULL,
  last_error text, created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL
)",
    r"CREATE INDEX pricing_reference_op_due ON pricing_reference_op (state, next_attempt_at) WHERE state <> 'done'",
];
pub const REFERENCE_OP_BEFORE_RENAME_PG: &[&str] = &[
    r"CREATE TABLE bss.pricing_reference_op (
  op_id uuid PRIMARY KEY, tenant_id uuid NOT NULL,
  kind text NOT NULL CHECK (kind IN ('create_price','delete_price','rereserve_price')),
  price_id uuid NOT NULL, sku_id uuid NOT NULL, reservation_id uuid,
  idempotency_key text,
  state text NOT NULL CHECK (state IN ('reserving','written','cancelling','releasing','done')),
  outcome text, attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL,
  last_error text, created_by uuid NOT NULL, created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL
)",
    r"CREATE INDEX pricing_reference_op_due ON bss.pricing_reference_op (state, next_attempt_at) WHERE state <> 'done'",
];

/// The pre-rename `pricing_price`: the book's line, keyed on the SKU (today's entry).
pub const ENTRY_SHAPED_PRICE_SQLITE: &[&str] = &[
    r"CREATE TABLE pricing_price (
  id text PRIMARY KEY, tenant_id text NOT NULL, book_id text NOT NULL REFERENCES pricing_price_book(id),
  sku_id text NOT NULL, charge_kind text NOT NULL CHECK (charge_kind IN ('recurring','usage','one_time')),
  period text, dimension_key text, invoice_line_override text,
  reservation_id text NOT NULL,
  reference_state text NOT NULL CHECK (reference_state IN ('confirmation_pending','confirmed','lost')),
  version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  FOREIGN KEY (tenant_id, dimension_key) REFERENCES pricing_dimension_key(tenant_id, key),
  CHECK ((charge_kind = 'recurring' AND period IS NOT NULL AND period IN ('month','year'))
    OR (charge_kind IN ('usage','one_time') AND period IS NULL))
)",
    r"CREATE UNIQUE INDEX pricing_price_key ON pricing_price (book_id, sku_id, charge_kind, coalesce(period, ''))",
];
pub const ENTRY_SHAPED_PRICE_PG: &[&str] = &[
    r"CREATE TABLE bss.pricing_price (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, book_id uuid NOT NULL REFERENCES bss.pricing_price_book(id),
  sku_id uuid NOT NULL, charge_kind text NOT NULL CHECK (charge_kind IN ('recurring','usage','one_time')),
  period text, dimension_key text, invoice_line_override text,
  reservation_id uuid NOT NULL,
  reference_state text NOT NULL CHECK (reference_state IN ('confirmation_pending','confirmed','lost')),
  version bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  FOREIGN KEY (tenant_id, dimension_key) REFERENCES bss.pricing_dimension_key(tenant_id, key),
  CHECK ((charge_kind = 'recurring' AND period IS NOT NULL AND period IN ('month','year'))
    OR (charge_kind IN ('usage','one_time') AND period IS NULL))
)",
    r"CREATE UNIQUE INDEX pricing_price_key ON bss.pricing_price (book_id, sku_id, charge_kind, coalesce(period, ''))",
];

/// The pre-rename `pricing_price_row`: a dated amount of the entry-shaped `pricing_price`.
pub const PRICE_ROW_SQLITE: &[&str] = &[
    r"CREATE TABLE pricing_price_row (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_id text NOT NULL REFERENCES pricing_price(id),
  version_no integer NOT NULL, dim_value text,
  model text NOT NULL CHECK (model IN ('flat','per_unit','graduated','volume','package')), price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_row_id text REFERENCES pricing_price_row(id),
  return_of_row_id text REFERENCES pricing_price_row(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)",
    r"CREATE UNIQUE INDEX pricing_price_row_approved_start
  ON pricing_price_row (price_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'",
    r"CREATE INDEX pricing_price_row_chain
  ON pricing_price_row (price_id, dim_value, effective_from) WHERE state = 'approved'",
];
pub const PRICE_ROW_PG: &[&str] = &[
    r"CREATE TABLE bss.pricing_price_row (
  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, price_id uuid NOT NULL REFERENCES bss.pricing_price(id),
  version_no integer NOT NULL, dim_value text,
  model text NOT NULL CHECK (model IN ('flat','per_unit','graduated','volume','package')), price_json jsonb NOT NULL,
  min_fee text CHECK (min_fee ~ '^[0-9]+(\.[0-9]+)?$'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from date NOT NULL, effective_to date, keep_for_bound boolean NOT NULL DEFAULT false,
  closed_explicitly boolean NOT NULL DEFAULT false,
  temporary_until date, paired_row_id uuid REFERENCES bss.pricing_price_row(id),
  return_of_row_id uuid REFERENCES bss.pricing_price_row(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id uuid REFERENCES bss.pricing_approval_unit(id),
  approved_by_unit_id uuid REFERENCES bss.pricing_approval_unit(id), note text, created_by uuid NOT NULL,
  approved_at timestamptz, version bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL,
  UNIQUE (price_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)",
    r"CREATE UNIQUE INDEX pricing_price_row_approved_start
  ON bss.pricing_price_row (price_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'",
    r"CREATE INDEX pricing_price_row_chain
  ON bss.pricing_price_row (price_id, dim_value, effective_from) WHERE state = 'approved'",
];

/// The pre-rename findings, in the order the guard reports them.
pub const PRE_RENAME_FINDINGS: &str = "table pricing_price_row; pricing_price without column \
     price_book_entry_id; pricing_reference_op without column ref_kind";

/// The legacy chain's `pricing_plan` (`m20260821_000021`): no `code` column.
pub const LEGACY_PLAN_SQLITE: &[&str] = &[r"CREATE TABLE pricing_plan (
            tenant_id                    text    NOT NULL,
            plan_id                      text    NOT NULL,
            revision                     bigint  NOT NULL,
            allowed_change_targets       text,
            available_from               text,
            available_to                 text,
            cloned_from                  text,
            comparability_rank           integer,
            custom_interval_n            int,
            custom_interval_unit         text,
            entitlement_grants           text,
            frequency                    text,
            descriptor_ext               text    NOT NULL DEFAULT '{}',
            lifecycle_state              text    NOT NULL,
            plan_name                    text    NOT NULL,
            plan_tier                    text,
            plan_tier_override           boolean NOT NULL DEFAULT 0,
            purchase_max_qty             bigint,
            purchase_min_qty             bigint,
            sku_id                       text    NOT NULL,
            usage_counter_on_plan_change text,
            created_at_utc               text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            created_by                   text    NOT NULL,
            row_version                  bigint  NOT NULL DEFAULT 0,
            PRIMARY KEY (plan_id, revision),
            CONSTRAINT chk_pricing_plan_availability CHECK (available_from IS NULL OR available_to IS NULL OR available_to > available_from),
            CONSTRAINT chk_pricing_plan_custom_interval_n CHECK (custom_interval_n IS NULL OR custom_interval_n > 0),
            CONSTRAINT chk_pricing_plan_custom_interval_pairing CHECK ((frequency IS NOT NULL AND frequency = 'custom_every_n') = (custom_interval_n IS NOT NULL AND custom_interval_unit IS NOT NULL)),
            CONSTRAINT chk_pricing_plan_custom_interval_unit CHECK (custom_interval_unit IS NULL OR custom_interval_unit IN ('days','months')),
            CONSTRAINT chk_pricing_plan_frequency CHECK (frequency IS NULL OR frequency IN ('monthly','quarterly','semiannual','annual','custom_every_n')),
            CONSTRAINT chk_pricing_plan_lifecycle_state CHECK (lifecycle_state IN ('draft','abandoned','published','superseded','retired')),
            CONSTRAINT chk_pricing_plan_purchase_max_qty CHECK (purchase_max_qty IS NULL OR purchase_max_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_min_qty CHECK (purchase_min_qty IS NULL OR purchase_min_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_qty CHECK (purchase_min_qty IS NULL OR purchase_max_qty IS NULL OR purchase_min_qty <= purchase_max_qty),
            CONSTRAINT chk_pricing_plan_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_plan_row_version CHECK (row_version >= 0)
        )"];
pub const LEGACY_PLAN_PG: &[&str] = &[r"CREATE TABLE bss.pricing_plan (
            tenant_id                    uuid        NOT NULL,
            plan_id                      uuid        NOT NULL,
            revision                     bigint      NOT NULL,
            allowed_change_targets       jsonb,
            available_from               timestamptz,
            available_to                 timestamptz,
            cloned_from                  uuid,
            comparability_rank           integer,
            custom_interval_n            integer,
            custom_interval_unit         text,
            entitlement_grants           jsonb,
            frequency                    text,
            descriptor_ext               jsonb       NOT NULL DEFAULT '{}'::jsonb,
            lifecycle_state              text        NOT NULL,
            plan_name                    text        NOT NULL,
            plan_tier                    text,
            plan_tier_override           boolean     NOT NULL DEFAULT false,
            purchase_max_qty             bigint,
            purchase_min_qty             bigint,
            sku_id                       uuid        NOT NULL,
            usage_counter_on_plan_change text,
            created_at_utc               timestamptz NOT NULL DEFAULT now(),
            created_by                   uuid        NOT NULL,
            row_version                  bigint      NOT NULL DEFAULT 0,
            CONSTRAINT chk_pricing_plan_availability CHECK (available_from IS NULL OR available_to IS NULL OR available_to > available_from),
            CONSTRAINT chk_pricing_plan_custom_interval_n CHECK (custom_interval_n IS NULL OR custom_interval_n > 0),
            CONSTRAINT chk_pricing_plan_custom_interval_pairing CHECK ((frequency IS NOT NULL AND frequency = 'custom_every_n') = (custom_interval_n IS NOT NULL AND custom_interval_unit IS NOT NULL)),
            CONSTRAINT chk_pricing_plan_custom_interval_unit CHECK (custom_interval_unit IS NULL OR custom_interval_unit IN ('days','months')),
            CONSTRAINT chk_pricing_plan_frequency CHECK (frequency IS NULL OR frequency IN ('monthly','quarterly','semiannual','annual','custom_every_n')),
            CONSTRAINT chk_pricing_plan_lifecycle_state CHECK (lifecycle_state IN ('draft','abandoned','published','superseded','retired')),
            CONSTRAINT chk_pricing_plan_purchase_max_qty CHECK (purchase_max_qty IS NULL OR purchase_max_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_min_qty CHECK (purchase_min_qty IS NULL OR purchase_min_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_qty CHECK (purchase_min_qty IS NULL OR purchase_max_qty IS NULL OR purchase_min_qty <= purchase_max_qty),
            CONSTRAINT chk_pricing_plan_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_plan_row_version CHECK (row_version >= 0),
            CONSTRAINT pricing_plan_pkey PRIMARY KEY (plan_id, revision)
        )"];
