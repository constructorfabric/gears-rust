//! P-D-248 and P-D-249: a retire under review keeps the SKU's lifecycle, and a dated lifecycle
//! change waits for its date.
//!
//! A forward migration: the chain is deployed and no shipped migration is edited again. Postgres
//! alters `bss.products_sku` in place. `SQLite` cannot drop or retype a column, and the toolkit
//! runner runs every `up()` inside a transaction, where `PRAGMA foreign_keys=OFF` has no effect,
//! so dropping a parent that still has child rows fails. The `SQLite` arm therefore rebuilds the
//! whole `m000007` family (`products_sku`, `products_sku_version`, `products_sku_reference`) and
//! uses no PRAGMA, as `m20260925_000007` does: a rebuild of `products_sku` alone fails the parent
//! drop.
//!
//! The rebuild, and the Postgres `UPDATE`, turn a stored `retiring` row into
//! `lifecycle = fence_prior_lifecycle, retire_pending = true`, then drop `fence_prior_lifecycle`.
//! The lifecycle CHECK becomes draft, published, deprecated and retired. `retire_pending` requires
//! `fenced_at`. `lifecycle_next` and `lifecycle_next_from` are both null or both set, and a next
//! lifecycle is never `retired`.
//!
//! `down()` is irreversible: the prior lifecycle of a converted retire lives only in the new
//! `lifecycle` column, and a dated change's next state has no old column to return to.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "ALTER TABLE bss.products_sku ADD COLUMN retire_pending boolean NOT NULL DEFAULT false",
    "ALTER TABLE bss.products_sku ADD COLUMN lifecycle_next text NULL",
    "ALTER TABLE bss.products_sku ADD COLUMN lifecycle_next_from date NULL",
    "UPDATE bss.products_sku SET lifecycle = fence_prior_lifecycle, retire_pending = true \
     WHERE lifecycle = 'retiring'",
    "DO $$\n\
     DECLARE r record;\n\
     BEGIN\n\
       FOR r IN\n\
         SELECT con.conname\n\
         FROM pg_constraint con\n\
         JOIN pg_class rel ON rel.oid = con.conrelid\n\
         JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace\n\
         WHERE nsp.nspname = 'bss' AND rel.relname = 'products_sku' AND con.contype = 'c'\n\
           AND pg_get_constraintdef(con.oid) ILIKE '%lifecycle%'\n\
       LOOP\n\
         EXECUTE format('ALTER TABLE bss.products_sku DROP CONSTRAINT %I', r.conname);\n\
       END LOOP;\n\
     END $$",
    "ALTER TABLE bss.products_sku DROP COLUMN fence_prior_lifecycle",
    "ALTER TABLE bss.products_sku ADD CONSTRAINT chk_products_sku_lifecycle \
     CHECK (lifecycle IN ('draft','published','deprecated','retired'))",
    "ALTER TABLE bss.products_sku ADD CONSTRAINT chk_products_sku_lifecycle_next \
     CHECK ((lifecycle_next IS NULL AND lifecycle_next_from IS NULL) OR \
     (lifecycle_next IN ('draft','published','deprecated') AND lifecycle_next_from IS NOT NULL))",
    "ALTER TABLE bss.products_sku ADD CONSTRAINT chk_products_sku_retire_pending \
     CHECK (NOT retire_pending OR fenced_at IS NOT NULL)",
];

const SKU_INSERT: &str = "INSERT INTO products_sku__p248 (\n  \
     id, tenant_id, code, name, type, category_id, description, sellable, lifecycle, fenced_at, \
     fence_op_id, revision, published_version, gl_code, tax_category, invoice_line_template, \
     billing_timing, usage_type_ref, unit, type_change_pending, retire_pending, lifecycle_next, \
     lifecycle_next_from, pending_unit_id, approved_by_unit_id, created_by, created_at, updated_at\
     )\n  SELECT id, tenant_id, code, name, type, category_id, description, sellable,\n  \
     CASE WHEN lifecycle = 'retiring' THEN fence_prior_lifecycle ELSE lifecycle END,\n  \
     fenced_at, fence_op_id, revision, published_version, gl_code, tax_category, \
     invoice_line_template, billing_timing, usage_type_ref, unit, type_change_pending,\n  \
     CASE WHEN lifecycle = 'retiring' THEN 1 ELSE 0 END, NULL, NULL,\n  \
     pending_unit_id, approved_by_unit_id, created_by, created_at, updated_at FROM products_sku";

fn sqlite_up() -> Vec<String> {
    let mut statements = vec![
        "CREATE TABLE products_sku__p248 (\n  \
         id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL,\n  \
         type text NOT NULL CHECK (type IN ('recurring','usage','one_time','bundle')),\n  \
         category_id text NULL REFERENCES products_category(id),\n  \
         description text NOT NULL DEFAULT '', sellable integer NOT NULL DEFAULT true,\n  \
         lifecycle text NOT NULL CHECK (lifecycle IN ('draft','published','deprecated','retired')),\n  \
         fenced_at text NULL, fence_op_id text NULL,\n  \
         revision integer NOT NULL DEFAULT 1, published_version integer NOT NULL DEFAULT 0,\n  \
         gl_code text NULL, tax_category text NULL, invoice_line_template text NULL,\n  \
         billing_timing text NULL CHECK (billing_timing IN ('advance','arrears')),\n  \
         usage_type_ref text NULL, unit text NULL,\n  \
         type_change_pending integer NOT NULL DEFAULT false,\n  \
         retire_pending integer NOT NULL DEFAULT false,\n  \
         lifecycle_next text NULL, lifecycle_next_from text NULL,\n  \
         pending_unit_id text NULL, approved_by_unit_id text NULL,\n  \
         created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL,\n  \
         CHECK (retire_pending = 0 OR fenced_at IS NOT NULL),\n  \
         CHECK ((lifecycle_next IS NULL AND lifecycle_next_from IS NULL) OR \
         (lifecycle_next IN ('draft','published','deprecated') AND lifecycle_next_from IS NOT NULL))\
         )"
        .to_owned(),
        SKU_INSERT.to_owned(),
        "CREATE TABLE products_sku_version__p248 (\n  \
         sku_id text NOT NULL REFERENCES products_sku__p248(id), tenant_id text NOT NULL, \
         published_version integer NOT NULL,\n  \
         effective_from text NOT NULL, content text NOT NULL, created_at text NOT NULL,\n  \
         PRIMARY KEY (sku_id, published_version))"
            .to_owned(),
        "INSERT INTO products_sku_version__p248 (sku_id, tenant_id, published_version, \
         effective_from, content, created_at) SELECT sku_id, tenant_id, published_version, \
         effective_from, content, created_at FROM products_sku_version"
            .to_owned(),
        "CREATE TABLE products_sku_reference__p248 (\n  \
         id text PRIMARY KEY, tenant_id text NOT NULL, sku_id text NOT NULL \
         REFERENCES products_sku__p248(id),\n  \
         owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN \
         ('price_book_entry','plan_item','sold_as')), ref_id text NOT NULL,\n  \
         state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  \
         reserved_by text NOT NULL, reserved_at text NOT NULL, confirmed_at text NULL, \
         released_at text NULL, released_by text NULL, release_reason text NULL, \
         forced integer NOT NULL DEFAULT false)"
            .to_owned(),
        "INSERT INTO products_sku_reference__p248 (id, tenant_id, sku_id, owner_gear, ref_kind, \
         ref_id, state, reserved_by, reserved_at, confirmed_at, released_at, released_by, \
         release_reason, forced) SELECT id, tenant_id, sku_id, owner_gear, ref_kind, ref_id, \
         state, reserved_by, reserved_at, confirmed_at, released_at, released_by, release_reason, \
         forced FROM products_sku_reference"
            .to_owned(),
    ];
    statements.extend(
        [
            "DROP TABLE products_sku_reference",
            "DROP TABLE products_sku_version",
            "DROP TABLE products_sku",
            "ALTER TABLE products_sku__p248 RENAME TO products_sku",
            "ALTER TABLE products_sku_version__p248 RENAME TO products_sku_version",
            "ALTER TABLE products_sku_reference__p248 RENAME TO products_sku_reference",
            "CREATE UNIQUE INDEX uq_products_sku_code ON products_sku (tenant_id, code)",
            "CREATE UNIQUE INDEX uq_products_sku_name ON products_sku (tenant_id, name)",
            "CREATE INDEX ix_products_sku_list ON products_sku (tenant_id, lifecycle, type, category_id, code)",
            "CREATE INDEX ix_products_sku_version_as_of ON products_sku_version (sku_id, effective_from, published_version)",
            "CREATE TRIGGER products_sku_version_no_update BEFORE UPDATE ON products_sku_version BEGIN SELECT RAISE(ABORT, 'products_sku_version is append-only'); END",
            "CREATE TRIGGER products_sku_version_no_delete BEFORE DELETE ON products_sku_version BEGIN SELECT RAISE(ABORT, 'products_sku_version is append-only'); END",
            "CREATE UNIQUE INDEX uq_products_sku_reference_live ON products_sku_reference (tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'",
            "CREATE INDEX ix_products_sku_reference_live ON products_sku_reference (sku_id, state) WHERE state <> 'released'",
        ]
        .map(str::to_owned),
    );
    statements
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sqlite = sqlite_up();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, PG_UP, &sqlite).await
    }

    /// Irreversible: a converted `retiring` row keeps its prior lifecycle only as `lifecycle`,
    /// and `fence_prior_lifecycle` is gone.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} a retire under review keeps its lifecycle \
             (P-D-248) and a dated change waits on lifecycle_next (P-D-249); neither has a \
             column to return to",
            self.name()
        )))
    }
}

#[cfg(test)]
#[path = "m20261001_000011_sku_lifecycle_honesty_tests.rs"]
mod tests;
