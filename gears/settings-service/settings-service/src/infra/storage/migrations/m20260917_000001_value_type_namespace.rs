// Created: 2026-09-17 by Virtuozzo International GmbH
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
/// left alone. The prefix is matched literally — its `_` is escaped, since to
/// `LIKE` it would mean "any one character" — and every input reaches the
/// database as a bound value, never as SQL text. Idempotent: a second run
/// matches nothing.
async fn rewrite(manager: &SchemaManager<'_>, from: &str, to: &str) -> Result<(), DbErr> {
    let start = <i32 as std::convert::TryFrom<usize>>::try_from(from.len() + 1)
        .map_err(|_| DbErr::Custom("value type prefix longer than any id".to_owned()))?;
    let statement = Query::update()
        .table(Alias::new("setting_declarations"))
        .value(
            Alias::new("value_type_id"),
            Expr::cust_with_values(
                "? || SUBSTR(value_type_id, ?)",
                [Value::from(to.to_owned()), Value::from(start)],
            ),
        )
        .and_where(
            Expr::col(Alias::new("value_type_id"))
                .like(LikeExpr::new(format!("{}%", like_literal(from))).escape('\\')),
        )
        .to_owned();
    manager.exec_stmt(statement).await
}

/// `text` as a `LIKE` pattern that matches it and nothing else: the wildcards
/// `%` and `_`, and the escape itself, escaped with `\`.
fn like_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[path = "m20260917_000001_value_type_namespace_tests.rs"]
mod tests;
