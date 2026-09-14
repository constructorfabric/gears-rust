//! `btree_gist`, which the two `EXCLUDE` constraints need.
//!
//! `pricing_price_window` and `pricing_group_membership` each declare an `EXCLUDE
//! USING gist` that mixes equality on a scalar with overlap on a range, and gist
//! has no equality operator class for the scalar types without this extension.
//!
//! It is its own migration, ahead of every table, because an extension is a
//! database-level fact and neither of the two tables owns it.
//!
//! Postgres only: `SQLite` has neither the constraint kind nor the extension, and
//! substitutes triggers for both.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["CREATE EXTENSION IF NOT EXISTS btree_gist"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP EXTENSION IF EXISTS btree_gist"];

const SQLITE_UP_STATEMENTS: &[&str] = &[];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
