//! The `bss` schema itself, before anything is created in it.
//!
//! The old chain created it as a side effect of the first table's migration, which
//! meant the schema's existence was a property of whichever table happened to sort
//! first. It gets its own migration here so that ordering is stated rather than
//! inherited.
//!
//! `IF NOT EXISTS` because the coord gear's migration sorts ahead of this one (its
//! name begins `m0001_`) and creates the schema for its own lease table. Nothing
//! here may depend on which of the two ran first.
//!
//! `SQLite` has no schemas, so its side is empty and `down` drops nothing: dropping
//! the schema would take the coord gear's table with it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["CREATE SCHEMA IF NOT EXISTS bss"];

const PG_DOWN_STATEMENTS: &[&str] = &[];

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
