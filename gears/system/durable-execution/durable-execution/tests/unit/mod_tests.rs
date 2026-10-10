use crate::infra::storage::{JournalStore, repository::tests::journal};
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
use crate::infra::storage::repository::tests::isolated_url;
use sea_orm_migration::MigrationTrait;
use toolkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_gear};
fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        Box::new(super::migration::Migration),
        Box::new(super::delivery_migration::Migration),
        Box::new(super::normalized_migration::Migration),
        Box::new(super::events_migration::Migration),
        Box::new(super::definition_migration::Migration),
    ]
}
#[tokio::test]
async fn lifecycle_migrations_keep_legacy_ids_and_are_repeatable() -> anyhow::Result<()> {
    let names: Vec<String> = migrations().iter().map(|m| m.name().to_owned()).collect();
    assert_eq!(
        names,
        [
            "migration",
            "migration_000002_framework_outbox",
            "migration_000003_activity_journal",
            "migration_000004_events",
            "migration_000005_definition_registry"
        ]
    );
    let database = isolated_url().await;
    let db = connect_db(&database, ConnectOpts::default()).await?;
    run_migrations_for_gear(&db, "durable-execution", migrations()).await?;
    let store = JournalStore::new(std::sync::Arc::new(toolkit_db::DBProvider::new(db.clone())));
    let handle = store.start_outbox(Park).await?;
    handle.stop().await;
    let candidate = journal();
    let id = candidate.run.id;
    store.insert(AccessScope::allow_all(), candidate).await?;
    run_migrations_for_gear(&db, "durable-execution", migrations()).await?;
    let restored = store.get(&AccessScope::allow_all(), id).await?.unwrap();
    assert_eq!(
        restored.run.status,
        crate::domain::persisted::RunStatus::Queued
    );
    assert_eq!(restored.run.activities.len(), 2);
    assert_eq!(
        store
            .deliveries(&AccessScope::allow_all(), chrono::Utc::now(), 100)
            .await?
            .len(),
        1
    );
    Ok(())
}
