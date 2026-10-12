//! What the schema-guard suites of both tiers share (P-D-195): the refusal they expect, the
//! `products_sku_reference` shape the phase 2 rename replaced in place (`64e694a9f^`,
//! `m20260925_000006`), copied verbatim so a test can rebuild it, and the legacy chain's shapes of
//! the two tables whose names today's chain creates too, `products_sku` (`m20260829_000003`) and
//! `products_category` (`m20260901_000018`), neither with a `code` column: their `CREATE TABLE`
//! only (the guard reads columns), the SKU's foreign key to the legacy `products_product` left out
//! because today's chain does not create that table.

#![allow(dead_code, reason = "each test binary uses part of the module")]

/// The guard's migration name; it sorts before every other migration of the gear.
pub const GUARD: &str = "m0000_products_refuse_a_legacy_or_stale_schema";

/// Every table the legacy chain creates, counted independently of the guard's constant:
/// `git show bss/products-backup:gears/bss/products/products/src/infra/storage/migrations/*` has 82
/// `CREATE TABLE` statements (both dialects, every name a literal, no `RENAME TO`) naming these 40.
/// The legacy set the guard refuses is this list minus what a fresh chain creates (P-D-195, H1).
pub const LEGACY_CHAIN_TABLES: [&str; 40] = [
    "products_approval",
    "products_approval_decision",
    "products_attribute_definition",
    "products_attribute_value",
    "products_audit_log",
    "products_breakglass_session",
    "products_bulk_batch",
    "products_bulk_row",
    "products_catalog_version",
    "products_catalog_version_capture",
    "products_catalog_version_counter",
    "products_catalog_version_entry",
    "products_catalog_version_request",
    "products_category",
    "products_correction_override",
    "products_deferred_retirement",
    "products_entity_version",
    "products_freeze_ack",
    "products_freeze_participant",
    "products_idempotency",
    "products_identity_ref",
    "products_materiality_policy",
    "products_metadata",
    "products_pii_allowlist",
    "products_product",
    "products_product_category",
    "products_read_checkpoint",
    "products_read_deferred_intent",
    "products_read_delivery_state",
    "products_read_entity",
    "products_read_freeze_status",
    "products_read_inbox",
    "products_read_poison",
    "products_read_stamp",
    "products_recognized_set",
    "products_reference_member",
    "products_reference_producer",
    "products_reference_watermark",
    "products_scheduled_transition",
    "products_sku",
];

/// P-D-195's refusal, word for word.
#[must_use]
pub fn refusal(kind: &str, found: &str) -> String {
    format!(
        "bss-products: this database holds a {kind} bss-products schema ({found}); PriceBook \
         does not migrate it \u{2014} start from an empty data root / empty bss-products tables"
    )
}

/// What the guard reports for the pre-rename reference table.
pub const STALE_REFERENCE: &str =
    "products_sku_reference whose ref_kind CHECK does not admit price_book_entry";

pub const SKU_REFERENCE_BEFORE_RENAME_SQLITE: &[&str] = &[
    "CREATE TABLE products_sku_reference (\n  id text PRIMARY KEY, tenant_id text NOT NULL, sku_id text NOT NULL REFERENCES products_sku(id),\n  owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN ('price','plan_item','sold_as')), ref_id text NOT NULL,\n  state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  reserved_by text NOT NULL, reserved_at text NOT NULL, confirmed_at text NULL, released_at text NULL, released_by text NULL, release_reason text NULL, forced integer NOT NULL DEFAULT false)",
    "CREATE UNIQUE INDEX uq_products_sku_reference_live ON products_sku_reference (tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'",
    "CREATE INDEX ix_products_sku_reference_live ON products_sku_reference (sku_id, state) WHERE state <> 'released'",
];
pub const SKU_REFERENCE_BEFORE_RENAME_PG: &[&str] = &[
    "CREATE TABLE bss.products_sku_reference (\n  id uuid PRIMARY KEY, tenant_id uuid NOT NULL, sku_id uuid NOT NULL REFERENCES bss.products_sku(id),\n  owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN ('price','plan_item','sold_as')), ref_id uuid NOT NULL,\n  state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  reserved_by uuid NOT NULL, reserved_at timestamptz NOT NULL, confirmed_at timestamptz NULL, released_at timestamptz NULL, released_by uuid NULL, release_reason text NULL, forced boolean NOT NULL DEFAULT false)",
    "CREATE UNIQUE INDEX uq_products_sku_reference_live ON bss.products_sku_reference USING btree (tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'",
    "CREATE INDEX ix_products_sku_reference_live ON bss.products_sku_reference USING btree (sku_id, state) WHERE state <> 'released'",
];

/// The legacy chain's `products_sku` (`m20260829_000003`): `sku_code`, no `code`.
pub const LEGACY_SKU_SQLITE: &[&str] = &[r"CREATE TABLE products_sku (
            tenant_id           text    NOT NULL,
            sku_id              text    NOT NULL,
            product_id          text    NOT NULL,
            sku_code            text    NOT NULL,
            lifecycle_state     text    NOT NULL,
            internal_revision   bigint  NOT NULL,
            published_version   bigint  NOT NULL,
            composition_pending boolean NOT NULL DEFAULT 0,
            region_scope        text    NOT NULL DEFAULT '',
            brand_scope         text    NOT NULL DEFAULT '',
            created_by          text    NOT NULL,
            created_at          text    NOT NULL,
            cloned_from         text,
            cloned_from_version integer,
            deprecation_provenance text,
            replaced_by_sku_id  text,
            metering_unit       text,
            usage_type_ref      text,
            sku_type            text,
            sellable            integer NOT NULL DEFAULT 1,
            plan_tier           text,
            correction_ref      text,
            updated_at          text    NOT NULL,
            PRIMARY KEY (sku_id),
            CONSTRAINT chk_products_sku_lifecycle_state CHECK (lifecycle_state IN ('draft', 'published', 'deprecated', 'retired', 'discarded')),
            CONSTRAINT chk_products_sku_internal_revision CHECK (internal_revision >= 1),
            CONSTRAINT chk_products_sku_published_version CHECK (published_version >= 0),
            CONSTRAINT chk_products_sku_cloned_from_shape CHECK (cloned_from IS NOT NULL OR cloned_from_version IS NULL),
            CONSTRAINT chk_products_sku_meter_pair CHECK ((metering_unit IS NULL) = (usage_type_ref IS NULL)),
            CONSTRAINT chk_products_sku_type CHECK (sku_type IS NULL OR sku_type IN ('offer', 'component', 'bundle'))
        )"];
pub const LEGACY_SKU_PG: &[&str] = &[r"CREATE TABLE bss.products_sku (
            tenant_id           uuid        NOT NULL,
            sku_id              uuid        NOT NULL,
            product_id          uuid        NOT NULL,
            sku_code            text        NOT NULL,
            lifecycle_state     text        NOT NULL,
            internal_revision   bigint      NOT NULL,
            published_version   bigint      NOT NULL,
            composition_pending boolean     NOT NULL DEFAULT false,
            region_scope        text        NOT NULL DEFAULT '',
            brand_scope         text        NOT NULL DEFAULT '',
            created_by          text        NOT NULL,
            created_at          timestamptz NOT NULL,
            cloned_from         uuid,
            cloned_from_version bigint,
            deprecation_provenance text,
            replaced_by_sku_id  uuid,
            metering_unit       text,
            usage_type_ref      text,
            sku_type            text,
            sellable            boolean     NOT NULL DEFAULT true,
            plan_tier           text,
            correction_ref      uuid,
            updated_at          timestamptz NOT NULL,
            CONSTRAINT products_sku_pkey PRIMARY KEY (sku_id),
            CONSTRAINT chk_products_sku_lifecycle_state CHECK (lifecycle_state IN ('draft', 'published', 'deprecated', 'retired', 'discarded')),
            CONSTRAINT chk_products_sku_internal_revision CHECK (internal_revision >= 1),
            CONSTRAINT chk_products_sku_published_version CHECK (published_version >= 0),
            CONSTRAINT chk_products_sku_cloned_from_shape CHECK (cloned_from IS NOT NULL OR cloned_from_version IS NULL),
            CONSTRAINT chk_products_sku_meter_pair CHECK ((metering_unit IS NULL) = (usage_type_ref IS NULL)),
            CONSTRAINT chk_products_sku_type CHECK (sku_type IS NULL OR sku_type IN ('offer', 'component', 'bundle'))
        )"];

/// The legacy chain's `products_category` (`m20260901_000018`): `category_id` and `name`, no `code`.
pub const LEGACY_CATEGORY_SQLITE: &[&str] = &[r"CREATE TABLE products_category (
            tenant_id       text    NOT NULL,
            category_id     text    NOT NULL,
            parent_id       text,
            name            text    NOT NULL,
            name_normalized text    NOT NULL,
            state           text    NOT NULL,
            mutation_seq    integer NOT NULL DEFAULT 0,
            is_default      integer NOT NULL DEFAULT 0,
            created_at      text    NOT NULL,
            updated_at      text    NOT NULL,
            PRIMARY KEY (tenant_id, category_id),
            CONSTRAINT chk_products_category_name CHECK (name <> ''),
            CONSTRAINT chk_products_category_name_normalized CHECK (name_normalized <> ''),
            CONSTRAINT chk_products_category_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_products_category_mutation_seq CHECK (mutation_seq >= 0),
            CONSTRAINT chk_products_category_not_own_parent CHECK (parent_id IS NULL OR parent_id <> category_id),
            CONSTRAINT fk_products_category_parent FOREIGN KEY (tenant_id, parent_id)
                REFERENCES products_category (tenant_id, category_id)
        )"];
pub const LEGACY_CATEGORY_PG: &[&str] = &[r"CREATE TABLE bss.products_category (
            tenant_id       uuid        NOT NULL,
            category_id     uuid        NOT NULL,
            parent_id       uuid,
            name            text        NOT NULL,
            name_normalized text        NOT NULL,
            state           text        NOT NULL,
            mutation_seq    bigint      NOT NULL DEFAULT 0,
            is_default      boolean     NOT NULL DEFAULT false,
            created_at      timestamptz NOT NULL,
            updated_at      timestamptz NOT NULL,
            CONSTRAINT products_category_pkey PRIMARY KEY (tenant_id, category_id),
            CONSTRAINT chk_products_category_name CHECK (name <> ''),
            CONSTRAINT chk_products_category_name_normalized CHECK (name_normalized <> ''),
            CONSTRAINT chk_products_category_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_products_category_mutation_seq CHECK (mutation_seq >= 0),
            CONSTRAINT chk_products_category_not_own_parent CHECK (parent_id IS NULL OR parent_id <> category_id),
            CONSTRAINT fk_products_category_parent FOREIGN KEY (tenant_id, parent_id)
                REFERENCES bss.products_category (tenant_id, category_id)
        )"];
