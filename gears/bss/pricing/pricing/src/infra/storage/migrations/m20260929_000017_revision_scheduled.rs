//! D-446: a plan revision can be `scheduled` — approved, and waiting for its sale date. A FORWARD
//! migration (D-427 closed D-412: the chain is deployed and no shipped migration is edited again).
//! The state CHECK of `pricing_plan_revision` (`chk_pricing_plan_revision_state`, from
//! `m20260926_000011`) gains `'scheduled'`, and the partial unique index
//! `pricing_plan_revision_scheduled` admits at most one scheduled revision per plan. No column
//! changes and no row is rewritten.
//!
//! Postgres drops the CHECK (`IF EXISTS`, so a replay changes nothing) and adds it again widened,
//! then creates the index. `SQLite` cannot alter a CHECK, and the toolkit runner runs every `up()`
//! inside a transaction, where `PRAGMA foreign_keys=OFF` has no effect (sqlx turns `foreign_keys`
//! on), so dropping a parent that still has child rows fails. The `SQLite` arm therefore rebuilds
//! the family and uses no PRAGMA, as products `m20260925_000007` does (P-D-196). The family is
//! `pricing_plan_revision` and its only child, `pricing_plan_item` (`revision_id`); no other table
//! references either.
//!
//! 1. create `pricing_plan_revision__d446`, the revision table of `m20260926_000011` verbatim except
//!    for the widened CHECK, and copy every row into it;
//! 2. create `pricing_plan_item__d446`, the item table of `m20260926_000012` verbatim, referencing
//!    the new parent, and copy every row;
//! 3. drop the old child, then the old parent. Dropping the parent first fails on its child's rows
//!    (`FOREIGN KEY constraint failed`);
//! 4. rename the two new tables. `ALTER TABLE … RENAME` rewrites the child's reference to the
//!    parent's final name;
//! 5. recreate, with their original text (`000011`), the two partial unique indexes that went with
//!    the old parent: `pricing_plan_revision_open` and `pricing_plan_revision_published`. The item
//!    table has no index beside its inline key, and neither table has a trigger;
//! 6. create the new index, `pricing_plan_revision_scheduled`.
//!
//! A replay rebuilds the rebuilt family once more, to the same schema. `down()` is irreversible: a
//! revision stored `scheduled` has no state under the old CHECK.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Postgres: the CHECK dropped and added again widened, then the index. The CHECK's vocabulary is
/// `RevisionState::ALL` in order, spelled once per dialect (`tests/pure_model.rs` pins both).
const PG_UP: &[&str] = &[
    "ALTER TABLE bss.pricing_plan_revision DROP CONSTRAINT IF EXISTS chk_pricing_plan_revision_state",
    "ALTER TABLE bss.pricing_plan_revision ADD CONSTRAINT chk_pricing_plan_revision_state CHECK (state IN ('draft','pending','scheduled','published','superseded'))",
    r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_scheduled
  ON bss.pricing_plan_revision (plan_id) WHERE state = 'scheduled'",
];

/// Every column of each table, in declaration order, for the copies.
const REVISION_COLUMNS: &str = "id, tenant_id, plan_id, rev_no, book_id, state, available_from, \
     pending_unit_id, approved_by_unit_id, published_at, version, created_by, created_at, updated_at";
const ITEM_COLUMNS: &str = "id, tenant_id, revision_id, sku_id, price_book_entry_id, treatment, \
     included_qty, qty_min, reservation_id, reference_state, version, created_by, created_at, \
     updated_at";

/// The statements of the family rebuild, in order (the module doc's steps 1 to 6).
fn sqlite_up() -> Vec<String> {
    let mut statements = vec![
        // 1. The new parent: `000011`'s text but for the widened CHECK.
        r"CREATE TABLE pricing_plan_revision__d446 (
  id text PRIMARY KEY, tenant_id text NOT NULL, plan_id text NOT NULL REFERENCES pricing_plan(id),
  rev_no integer NOT NULL, book_id text NOT NULL REFERENCES pricing_price_book(id), state text NOT NULL,
  available_from text, pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), published_at text,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL,
  CONSTRAINT pricing_plan_revision_no UNIQUE (plan_id, rev_no),
  CONSTRAINT chk_pricing_plan_revision_state CHECK (state IN ('draft','pending','scheduled','published','superseded'))
)"
        .to_owned(),
        format!(
            "INSERT INTO pricing_plan_revision__d446 ({REVISION_COLUMNS}) \
             SELECT {REVISION_COLUMNS} FROM pricing_plan_revision"
        ),
        // 2. The new child, against the new parent: `000012`'s text.
        r"CREATE TABLE pricing_plan_item__d446 (
  id text PRIMARY KEY, tenant_id text NOT NULL, revision_id text NOT NULL REFERENCES pricing_plan_revision__d446(id),
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
)"
        .to_owned(),
        format!(
            "INSERT INTO pricing_plan_item__d446 ({ITEM_COLUMNS}) \
             SELECT {ITEM_COLUMNS} FROM pricing_plan_item"
        ),
    ];
    statements.extend(
        [
            // 3. The old child first, then the old parent. A dropped table takes its indexes with
            // it.
            "DROP TABLE pricing_plan_item",
            "DROP TABLE pricing_plan_revision",
            // 4. The final names.
            "ALTER TABLE pricing_plan_revision__d446 RENAME TO pricing_plan_revision",
            "ALTER TABLE pricing_plan_item__d446 RENAME TO pricing_plan_item",
            // 5. What went with the old parent, in its original text (`000011`).
            r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_open
  ON pricing_plan_revision (plan_id) WHERE state IN ('draft','pending')",
            r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_published
  ON pricing_plan_revision (plan_id) WHERE state = 'published'",
            // 6. One scheduled revision per plan.
            r"CREATE UNIQUE INDEX IF NOT EXISTS pricing_plan_revision_scheduled
  ON pricing_plan_revision (plan_id) WHERE state = 'scheduled'",
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

    /// Irreversible: a revision stored `scheduled` since this migration has no state to return to
    /// under the old CHECK.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} a plan revision may be stored scheduled (D-446), a state \
             the old CHECK refuses",
            self.name()
        )))
    }
}
