use crate::store::{JournalStore, repository::tests::journal};
use sea_orm_migration::prelude::*;
use toolkit_security::AccessScope;
struct Park;
#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for Park {
    async fn handle(
        &self,
        _: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        toolkit_db::outbox::MessageResult::Retry
    }
}
use crate::store::repository::tests::isolated_url;
use toolkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_gear};

fn names(migrations: &[Box<dyn MigrationTrait>]) -> Vec<String> {
    migrations.iter().map(|m| m.name().to_owned()).collect()
}

/// A migration of the embedding gear itself, sharing the migration history.
#[cfg(not(feature = "integration"))]
struct GearOwnMigration;
#[cfg(not(feature = "integration"))]
impl MigrationName for GearOwnMigration {
    fn name(&self) -> &'static str {
        "m0002_orders__init"
    }
}
#[cfg(not(feature = "integration"))]
#[async_trait::async_trait]
impl MigrationTrait for GearOwnMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("CREATE TABLE orders__orders (id TEXT PRIMARY KEY)")
            .await?;
        Ok(())
    }
}

#[test]
fn journal_and_outbox_migrations_are_recorded_separately() {
    let names = names(&super::migrations());
    assert_eq!(
        names,
        [
            "m0001_durable_execution__init",
            "m001_create_toolkit_outbox_schema__durable_execution__delivery_outbox",
        ]
    );
}

#[tokio::test]
async fn schema_migrations_are_repeatable_and_namespaced() -> anyhow::Result<()> {
    let database = isolated_url().await;
    let db = connect_db(&database, ConnectOpts::default()).await?;
    run_migrations_for_gear(&db, "durable-consumer", super::migrations()).await?;
    let store = JournalStore::new(std::sync::Arc::new(toolkit_db::DBProvider::new(db.clone())));
    let handle = store.start_outbox(Park).await?;
    handle.stop().await;
    let candidate = journal();
    let id = candidate.run.id;
    store.insert(AccessScope::allow_all(), candidate).await?;
    run_migrations_for_gear(&db, "durable-consumer", super::migrations()).await?;
    let restored = store.get(&AccessScope::allow_all(), id).await?.unwrap();
    assert_eq!(
        restored.run.status,
        crate::state::persisted::RunStatus::Queued
    );
    // Only the first sequential stage is materialized before it runs.
    assert_eq!(restored.run.activities.len(), 1);
    assert_eq!(
        store
            .deliveries(&AccessScope::allow_all(), chrono::Utc::now(), 100)
            .await?
            .len(),
        1
    );
    Ok(())
}

/// Library migrations share the embedding gear's history with its own migrations,
/// and a migration missing from an existing database is applied on the next run.
#[cfg(not(feature = "integration"))]
#[tokio::test]
async fn library_migrations_coexist_with_gear_migrations_and_catch_up() -> anyhow::Result<()> {
    use sea_orm::{ConnectionTrait, Statement};
    let path = std::env::temp_dir().join(format!("durable-migrations-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = connect_db(&url, ConnectOpts::default()).await?;

    // An existing database that only has the gear's own schema and the journal.
    let mut first: Vec<Box<dyn MigrationTrait>> = vec![Box::new(GearOwnMigration)];
    first.push(super::migrations().remove(0));
    run_migrations_for_gear(&db, "orders", first).await?;

    // The full list adds the outbox without touching what was applied.
    let mut all: Vec<Box<dyn MigrationTrait>> = vec![Box::new(GearOwnMigration)];
    all.extend(super::migrations());
    run_migrations_for_gear(&db, "orders", all).await?;
    let store = JournalStore::new(std::sync::Arc::new(toolkit_db::DBProvider::new(db.clone())));
    store.start_outbox(Park).await?.stop().await;
    store.insert(AccessScope::allow_all(), journal()).await?;

    // Every object the library created, the outbox's included, is namespaced.
    let catalog = sea_orm::Database::connect(&url).await?;
    let rows = catalog
        .query_all_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'"
                .to_owned(),
        ))
        .await?;
    let objects: Vec<String> = rows
        .iter()
        .map(|row| row.try_get::<String>("", "name"))
        .collect::<Result<_, _>>()?;
    // Everything that is neither the gear's own table nor the migration history
    // belongs to the library and must carry its namespace; nothing is pre-filtered
    // by the prefix under test.
    let library: Vec<&String> = objects
        .iter()
        .filter(|name| !name.starts_with("orders__") && !name.starts_with("toolkit_migrations__"))
        .collect();
    assert!(library.iter().any(|name| name.contains("delivery_outbox")));
    assert!(
        library
            .iter()
            .any(|name| *name == "durable_execution__runs")
    );
    for name in &library {
        // The ToolKit outbox names its indexes `idx_<prefix>_…`; that convention is
        // owned by toolkit-db, so only that exact form is accepted besides the namespace.
        let namespaced = name.starts_with("durable_execution__")
            || name.starts_with("idx_durable_execution__delivery_outbox_");
        assert!(namespaced, "{name}");
        assert!(name.len() <= 63, "{name}");
    }
    assert!(objects.iter().any(|name| name == "orders__orders"));
    catalog.close().await?;
    drop(store);
    drop(db);
    // Best-effort cleanup of the temporary database file.
    drop(std::fs::remove_file(path));
    Ok(())
}
