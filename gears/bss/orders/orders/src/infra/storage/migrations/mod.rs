//! Ordered, forward-only PostgreSQL migrations; platform DDL stays with its owners.
use sea_orm_migration::prelude::*;

struct SqlMigration {
    name: &'static str,
    sql: &'static str,
}
impl MigrationName for SqlMigration {
    fn name(&self) -> &str {
        self.name
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SqlMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != sea_orm::DatabaseBackend::Postgres {
            return Err(DbErr::Migration(
                "Orders storage requires PostgreSQL 15 or later".into(),
            ));
        }
        manager
            .get_connection()
            .execute_unprepared(self.sql)
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "Orders evidence is retained: deploy a forward correction".into(),
        ))
    }
}

pub fn all() -> Vec<Box<dyn MigrationTrait>> {
    let mut migrations = event_broker_sdk::producer_registration_migrations();
    migrations.extend(toolkit_db::outbox::outbox_migrations());
    for (name, sql) in [
        (
            "m20261006_000001_orders_foundation",
            include_str!("01_schema.sql"),
        ),
        (
            "m20261006_000002_orders_capture",
            include_str!("02_schema.sql"),
        ),
        (
            "m20261006_000003_orders_diagnostics",
            include_str!("03_schema.sql"),
        ),
        (
            "m20261006_000004_orders_preconditions",
            include_str!("04_schema.sql"),
        ),
        ("m20261006_000005_orders_ttl", include_str!("05_schema.sql")),
        (
            "m20261006_000006_orders_access_log",
            include_str!("06_schema.sql"),
        ),
        (
            "m20261006_000007_orders_integrity",
            include_str!("07_integrity.sql"),
        ),
        (
            "m20261006_000008_orders_roles",
            include_str!("08_roles.sql"),
        ),
        (
            "m20261007_000009_orders_maintenance",
            include_str!("09_maintenance.sql"),
        ),
        (
            "m20261007_000010_orders_runtime_history",
            include_str!("10_runtime_history.sql"),
        ),
    ] {
        migrations.push(Box::new(SqlMigration { name, sql }));
    }
    migrations
}
