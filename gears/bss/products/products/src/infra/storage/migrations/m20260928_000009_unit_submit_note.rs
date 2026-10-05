//! P-D-219: the submitter's note travels with the approval unit — `submit_note` on
//! `products_approval_unit`.
//!
//! A forward migration: the chain is deployed and no shipped migration is edited again. The
//! approval library's `ddl::up()` is the body of `m20260925_000003` and stays as it shipped; this
//! migration runs the library's separate step, `bss_approval::ddl::apply_add_submit_note`:
//! `ADD COLUMN submit_note text`, nullable, no default, no CHECK (its length, at most 2000
//! characters, is judged by the submit doors: `NOTE_TOO_LONG`).
//!
//! - Postgres: `ALTER TABLE bss.products_approval_unit ADD COLUMN IF NOT EXISTS submit_note text`.
//! - `SQLite`: the same `ADD COLUMN` without `IF NOT EXISTS` (the dialect has none there), run only
//!   when the table's catalog lacks the column, so `up` replays.
//!
//! A unit written before this migration reads `submit_note: null`. Nothing is backfilled: a
//! change's note also stands on its submit's audit row (P-D-213), which is where the history reads
//! it, and the audit log is not a source a migration copies from.
//!
//! `down()` drops the column the same way (`IF EXISTS` on Postgres; on `SQLite` only when it is
//! there). It loses the notes stored on the units; the submit's audit row keeps each one for the
//! history.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_add_submit_note(manager, "products_", Some("bss"))
            .await
            .map_err(|e| DbErr::Migration(format!("{}: {e}", self.name())))
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_drop_submit_note(manager, "products_", Some("bss"))
            .await
            .map_err(|e| DbErr::Migration(format!("{}: {e}", self.name())))
    }
}

#[cfg(test)]
#[path = "m20260928_000009_unit_submit_note_tests.rs"]
mod tests;
