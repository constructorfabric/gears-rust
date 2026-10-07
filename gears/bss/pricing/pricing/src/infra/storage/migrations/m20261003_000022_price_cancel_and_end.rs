//! D-520 and D-521: a price row can ask to cancel or end another price, and a price can be
//! `cancelled`.
//!
//! A forward migration. The shipped chain is frozen, so this does not edit an earlier file.
//! Postgres adds the columns and replaces `pricing_price_state_check`. `SQLite` cannot widen a
//! `CHECK` in place, and the toolkit runner runs `up()` inside a transaction, where
//! `PRAGMA foreign_keys=OFF` has no effect. The `SQLite` arm therefore rebuilds `pricing_price`
//! only: nothing else references it (the self references are copied after the rows exist, so a
//! mutual pair survives). The rebuild keeps both indexes. The table has no triggers.
//!
//! An applied `cancel` or `end` row is `approved` and keeps the start of the price it names, so
//! the approved-start index covers `change_kind = 'set'` only: one price per start, and a change
//! record never takes that start (D-520).
//!
//! Two CHECKs pair the new columns (the branch review): a change names the price it changes and a
//! price (`set`) names none, `(change_kind = 'set') = (target_price_id IS NULL)`; and a cancelled
//! price names the unit that cancelled it and no other row names one,
//! `(state = 'cancelled') = (cancelled_by_unit_id IS NOT NULL)`. Every row before this migration is
//! a price that is not cancelled, so both hold for it.
//!
//! `down()` restores the previous shape. It fails on a database that holds a `cancelled` price
//! (the state check) or an applied change (the approved-start index); a draft or pending change
//! row goes back as a plain row.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "ALTER TABLE bss.pricing_price \
     ADD COLUMN change_kind text NOT NULL DEFAULT 'set', \
     ADD COLUMN target_price_id uuid, \
     ADD COLUMN cancelled_by_unit_id uuid",
    "ALTER TABLE bss.pricing_price \
     ADD CONSTRAINT pricing_price_change_kind_check \
       CHECK (change_kind IN ('set','cancel','end')), \
     ADD CONSTRAINT pricing_price_target_price_id_fkey \
       FOREIGN KEY (target_price_id) REFERENCES bss.pricing_price(id), \
     ADD CONSTRAINT pricing_price_cancelled_by_unit_id_fkey \
       FOREIGN KEY (cancelled_by_unit_id) REFERENCES bss.pricing_approval_unit(id)",
    "ALTER TABLE bss.pricing_price DROP CONSTRAINT pricing_price_state_check",
    "ALTER TABLE bss.pricing_price ADD CONSTRAINT pricing_price_state_check \
     CHECK (state IN ('draft','pending','approved','rejected','cancelled'))",
    "ALTER TABLE bss.pricing_price \
     ADD CONSTRAINT pricing_price_change_target_check \
       CHECK ((change_kind = 'set') = (target_price_id IS NULL)), \
     ADD CONSTRAINT pricing_price_cancelled_unit_check \
       CHECK ((state = 'cancelled') = (cancelled_by_unit_id IS NOT NULL))",
    "DROP INDEX bss.pricing_price_approved_start",
    "CREATE UNIQUE INDEX pricing_price_approved_start \
     ON bss.pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) \
     WHERE state = 'approved' AND change_kind = 'set'",
];

const PG_DOWN: &[&str] = &[
    "ALTER TABLE bss.pricing_price \
     DROP CONSTRAINT pricing_price_change_target_check, \
     DROP CONSTRAINT pricing_price_cancelled_unit_check",
    "ALTER TABLE bss.pricing_price DROP CONSTRAINT pricing_price_state_check",
    "ALTER TABLE bss.pricing_price ADD CONSTRAINT pricing_price_state_check \
     CHECK (state IN ('draft','pending','approved','rejected'))",
    "ALTER TABLE bss.pricing_price DROP CONSTRAINT pricing_price_change_kind_check",
    "ALTER TABLE bss.pricing_price DROP CONSTRAINT pricing_price_target_price_id_fkey",
    "ALTER TABLE bss.pricing_price DROP CONSTRAINT pricing_price_cancelled_by_unit_id_fkey",
    "DROP INDEX bss.pricing_price_approved_start",
    "CREATE UNIQUE INDEX pricing_price_approved_start \
     ON bss.pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) \
     WHERE state = 'approved'",
    "ALTER TABLE bss.pricing_price \
     DROP COLUMN change_kind, \
     DROP COLUMN target_price_id, \
     DROP COLUMN cancelled_by_unit_id",
];

const SQLITE_NEW: &str = "CREATE TABLE pricing_price__d520 (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_book_entry_id text NOT NULL REFERENCES pricing_price_book_entry(id),
  version_no integer NOT NULL, dim_value text,
  price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_price_id text REFERENCES pricing_price__d520(id),
  return_of_price_id text REFERENCES pricing_price__d520(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected','cancelled')),
  change_kind text NOT NULL DEFAULT 'set' CHECK (change_kind IN ('set','cancel','end')),
  target_price_id text REFERENCES pricing_price__d520(id),
  cancelled_by_unit_id text REFERENCES pricing_approval_unit(id),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to),
  CHECK ((change_kind = 'set') = (target_price_id IS NULL)),
  CHECK ((state = 'cancelled') = (cancelled_by_unit_id IS NOT NULL))
)";

const SQLITE_OLD: &str = "CREATE TABLE pricing_price__d520_old (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_book_entry_id text NOT NULL REFERENCES pricing_price_book_entry(id),
  version_no integer NOT NULL, dim_value text,
  price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_price_id text REFERENCES pricing_price__d520_old(id),
  return_of_price_id text REFERENCES pricing_price__d520_old(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected')),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to)
)";

const PRICE_COLUMNS: &str = "id, tenant_id, price_book_entry_id, version_no, dim_value, price_json, \
min_fee, eligibility, effective_from, effective_to, keep_for_bound, closed_explicitly, \
temporary_until, state, pending_unit_id, approved_by_unit_id, note, created_by, approved_at, \
version, created_at, updated_at";

fn sqlite_up() -> Vec<String> {
    vec![
        SQLITE_NEW.to_owned(),
        format!(
            "INSERT INTO pricing_price__d520 ({PRICE_COLUMNS}) SELECT {PRICE_COLUMNS} FROM pricing_price"
        ),
        "UPDATE pricing_price__d520 SET \
         paired_price_id = (SELECT p.paired_price_id FROM pricing_price p WHERE p.id = pricing_price__d520.id), \
         return_of_price_id = (SELECT p.return_of_price_id FROM pricing_price p WHERE p.id = pricing_price__d520.id)"
            .to_owned(),
        "DROP TABLE pricing_price".to_owned(),
        "ALTER TABLE pricing_price__d520 RENAME TO pricing_price".to_owned(),
        "CREATE UNIQUE INDEX pricing_price_approved_start\n  ON pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved' AND change_kind = 'set'"
            .to_owned(),
        "CREATE INDEX pricing_price_chain\n  ON pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'"
            .to_owned(),
    ]
}

fn sqlite_down() -> Vec<String> {
    vec![
        SQLITE_OLD.to_owned(),
        format!(
            "INSERT INTO pricing_price__d520_old ({PRICE_COLUMNS}) SELECT {PRICE_COLUMNS} FROM pricing_price"
        ),
        "UPDATE pricing_price__d520_old SET \
         paired_price_id = (SELECT p.paired_price_id FROM pricing_price p WHERE p.id = pricing_price__d520_old.id), \
         return_of_price_id = (SELECT p.return_of_price_id FROM pricing_price p WHERE p.id = pricing_price__d520_old.id)"
            .to_owned(),
        "DROP TABLE pricing_price".to_owned(),
        "ALTER TABLE pricing_price__d520_old RENAME TO pricing_price".to_owned(),
        "CREATE UNIQUE INDEX pricing_price_approved_start\n  ON pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved'"
            .to_owned(),
        "CREATE INDEX pricing_price_chain\n  ON pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'"
            .to_owned(),
    ]
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sqlite = sqlite_up();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, PG_UP, &sqlite).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sqlite = sqlite_down();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, PG_DOWN, &sqlite).await
    }
}

#[cfg(test)]
#[path = "m20261003_000022_price_cancel_and_end_tests.rs"]
mod tests;
