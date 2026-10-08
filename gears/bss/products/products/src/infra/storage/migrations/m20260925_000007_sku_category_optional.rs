//! P-D-196: a SKU's category is optional, so `products_sku.category_id` loses its NOT NULL.
//!
//! A forward migration: the chain is deployed and no shipped migration is edited again. Postgres
//! drops the NOT NULL in place. `SQLite` cannot, and the toolkit runner runs every `up()` inside a
//! transaction, where `PRAGMA foreign_keys=OFF` has no effect (sqlx turns `foreign_keys` on), so
//! dropping a parent that still has child rows fails. The `SQLite` arm therefore rebuilds the
//! family and uses no PRAGMA:
//!
//! 1. create `products_sku__p196`, the `products_sku` of `m20260925_000002` verbatim except for
//!    `category_id text NULL`, and copy every row into it;
//! 2. create `products_sku_version__p196` and `products_sku_reference__p196`, the tables of
//!    `000002` and `000006` verbatim, referencing the new parent, and copy every row;
//! 3. drop the old children, then the old parent. Dropping the parent first fails on its
//!    children's rows (`FOREIGN KEY constraint failed`);
//! 4. rename the three new tables. `ALTER TABLE … RENAME` rewrites the children's references to
//!    the parent's final name;
//! 5. recreate, with their original text, every index and trigger that went with the old tables:
//!    the SKU's two unique indexes and list index, the version as-of index, the two append-only
//!    triggers, and the two live-reference indexes (the partial unique
//!    `uq_products_sku_reference_live` among them).
//!
//! `down()` is irreversible: a SKU without a category has no NOT NULL to return to.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &["ALTER TABLE bss.products_sku ALTER COLUMN category_id DROP NOT NULL"];

/// Every column of each table, in declaration order, for the copies.
const SKU_COLUMNS: &str = "id, tenant_id, code, name, type, category_id, description, sellable, \
     lifecycle, fence_prior_lifecycle, fenced_at, fence_op_id, revision, published_version, \
     gl_code, tax_category, invoice_line_template, billing_timing, usage_type_ref, unit, \
     type_change_pending, pending_unit_id, approved_by_unit_id, created_by, created_at, updated_at";
const VERSION_COLUMNS: &str =
    "sku_id, tenant_id, published_version, effective_from, content, created_at";
const REFERENCE_COLUMNS: &str = "id, tenant_id, sku_id, owner_gear, ref_kind, ref_id, state, \
     reserved_by, reserved_at, confirmed_at, released_at, released_by, release_reason, forced";

/// The statements of the family rebuild, in order (the module doc's steps 1 to 5).
fn sqlite_up() -> Vec<String> {
    let mut statements = vec![
        // 1. The new parent: `000002`'s text but for `category_id text NULL`.
        "CREATE TABLE products_sku__p196 (\n  id text PRIMARY KEY, tenant_id text NOT NULL, code text NOT NULL, name text NOT NULL,\n  type text NOT NULL CHECK (type IN ('recurring','usage','one_time','bundle')),\n  category_id text NULL REFERENCES products_category(id),\n  description text NOT NULL DEFAULT '', sellable integer NOT NULL DEFAULT true,\n  lifecycle text NOT NULL CHECK (lifecycle IN ('draft','published','deprecated','retiring','retired')),\n  fence_prior_lifecycle text NULL CHECK (fence_prior_lifecycle IN ('published','deprecated')),\n  fenced_at text NULL, fence_op_id text NULL,        \n  revision integer NOT NULL DEFAULT 1, published_version integer NOT NULL DEFAULT 0,\n  gl_code text NULL, tax_category text NULL, invoice_line_template text NULL,\n  billing_timing text NULL CHECK (billing_timing IN ('advance','arrears')),\n  usage_type_ref text NULL, unit text NULL,\n  type_change_pending integer NOT NULL DEFAULT false,\n  pending_unit_id text NULL, approved_by_unit_id text NULL,\n  created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL)".to_owned(),
        format!("INSERT INTO products_sku__p196 ({SKU_COLUMNS}) SELECT {SKU_COLUMNS} FROM products_sku"),
        // 2. The new children, against the new parent: `000002`'s and `000006`'s text.
        "CREATE TABLE products_sku_version__p196 (\n  sku_id text NOT NULL REFERENCES products_sku__p196(id), tenant_id text NOT NULL, published_version integer NOT NULL,\n  effective_from text NOT NULL, content text NOT NULL, created_at text NOT NULL,\n  PRIMARY KEY (sku_id, published_version))".to_owned(),
        format!(
            "INSERT INTO products_sku_version__p196 ({VERSION_COLUMNS}) SELECT {VERSION_COLUMNS} FROM products_sku_version"
        ),
        "CREATE TABLE products_sku_reference__p196 (\n  id text PRIMARY KEY, tenant_id text NOT NULL, sku_id text NOT NULL REFERENCES products_sku__p196(id),\n  owner_gear text NOT NULL, ref_kind text NOT NULL CHECK (ref_kind IN ('price_book_entry','plan_item','sold_as')), ref_id text NOT NULL,\n  state text NOT NULL CHECK (state IN ('reserved','confirmed','released')),\n  reserved_by text NOT NULL, reserved_at text NOT NULL, confirmed_at text NULL, released_at text NULL, released_by text NULL, release_reason text NULL, forced integer NOT NULL DEFAULT false)".to_owned(),
        format!(
            "INSERT INTO products_sku_reference__p196 ({REFERENCE_COLUMNS}) SELECT {REFERENCE_COLUMNS} FROM products_sku_reference"
        ),
    ];
    statements.extend(
        [
            // 3. The old children first, then the old parent. A dropped table takes its indexes
            // and triggers with it; `DROP TABLE` fires no trigger.
            "DROP TABLE products_sku_reference",
            "DROP TABLE products_sku_version",
            "DROP TABLE products_sku",
            // 4. The final names.
            "ALTER TABLE products_sku__p196 RENAME TO products_sku",
            "ALTER TABLE products_sku_version__p196 RENAME TO products_sku_version",
            "ALTER TABLE products_sku_reference__p196 RENAME TO products_sku_reference",
            // 5. What went with the old tables, in its original text (`000002`, `000006`).
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

    /// Irreversible: the SKUs saved without a category since this migration have no category to
    /// put back under a NOT NULL.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} a SKU's category is optional (P-D-196); a SKU saved \
             without one has nothing to satisfy a NOT NULL",
            self.name()
        )))
    }
}

#[cfg(test)]
#[path = "m20260925_000007_sku_category_optional_tests.rs"]
mod tests;
