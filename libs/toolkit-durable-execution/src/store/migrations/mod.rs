/// The journal schema.
pub mod init;

use sea_orm_migration::{MigrationName, MigrationTrait, SchemaManager};

/// Schema migrations of the durable-execution journal and its delivery outbox.
///
/// Append them to the embedding gear's `DatabaseCapability::migrations`; they do
/// not depend on other tables. Each migration is recorded under its own name in the
/// embedding gear's migration history, so migrations added by later releases of
/// this library or of the `ToolKit` outbox are applied to existing databases.
/// Migrations are forward-only.
///
/// Tables use the fixed `durable_execution__` prefix, so one database or schema
/// hosts one engine.
#[must_use]
pub fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    let mut migrations: Vec<Box<dyn MigrationTrait>> = vec![Box::new(init::Migration)];
    match toolkit_db::outbox::outbox_migrations_with_prefix(crate::delivery::outbox::PREFIX) {
        Ok(outbox) => migrations.extend(outbox),
        Err(error) => migrations.push(Box::new(InvalidOutboxPrefix(error.to_string()))),
    }
    migrations
}

/// Reports an invalid outbox prefix when migrations run instead of panicking while
/// the host collects them. The prefix is a constant, so this is unreachable unless
/// the outbox naming rules change.
struct InvalidOutboxPrefix(String);

impl MigrationName for InvalidOutboxPrefix {
    fn name(&self) -> &'static str {
        "m001_create_toolkit_outbox_schema__durable_execution__invalid_prefix"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for InvalidOutboxPrefix {
    async fn up(&self, _: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(format!(
            "durable delivery outbox prefix is invalid: {}",
            self.0
        )))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/unit/mod_tests.rs"]
mod tests;
