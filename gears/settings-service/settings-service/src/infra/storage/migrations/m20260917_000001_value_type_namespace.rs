// Created: 2026-09-17 by Constructor Tech
//! Move stored `value_type_id`s into the gear's own namespace.
//!
//! The value-type catalogue moved from `gts.cf.toolkit.settings.type_*~` to
//! `gts.cf.core.settings.type_*~`, where the rest of the gear's types live and
//! where the SDK that defines the catalogue actually ships it (DESIGN.md §4.7,
//! ADR-002). A declaration row names its value type by id, so any row written
//! before the move still points at the old spelling — and the Type Validator
//! would refuse it as an unknown type, which fails every write to that setting
//! rather than only its next declaration change.
//!
//! Module-contributed declarations would heal themselves on the next reconcile.
//! Admin-authored ones would not: nothing re-registers them, so the rewrite has
//! to happen here.
//!
//! The prefix is rewritten, not the whole id: the catalogue's names did not
//! change, only whose namespace they sit in.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Prefixes, not identifiers: DE0901 reads any `gts.`-prefixed literal as a
// whole id and counts four tokens where a complete one has five.
//
// Written out rather than taken from `settings-service-sdk`: a migration is a
// record of one move that already happened. If the catalogue is ever renamed
// again, this must still rewrite `toolkit` to `core` on the databases that
// stopped there, not chase whatever the constant then holds.
#[allow(unknown_lints, de0901_gts_string_pattern)]
const OLD: &str = "gts.cf.toolkit.settings.type_";
#[allow(unknown_lints, de0901_gts_string_pattern)]
const NEW: &str = "gts.cf.core.settings.type_";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rewrite(manager, OLD, NEW).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rewrite(manager, NEW, OLD).await
    }
}

/// Repoint every declaration whose value type carries `from` at `to`.
///
/// Anchored with `LIKE 'prefix%'` and rewritten by length rather than by a
/// substring search, so an id that merely contains the prefix somewhere else is
/// left alone. Idempotent: a second run matches nothing.
async fn rewrite(manager: &SchemaManager<'_>, from: &str, to: &str) -> Result<(), DbErr> {
    let sql = format!(
        "UPDATE setting_declarations
            SET value_type_id = '{to}' || SUBSTR(value_type_id, {start})
          WHERE value_type_id LIKE '{from}%'",
        start = from.len() + 1,
    );
    manager.get_connection().execute_unprepared(&sql).await?;
    Ok(())
}

#[cfg(test)]
#[path = "m20260917_000001_value_type_namespace_tests.rs"]
mod tests;
